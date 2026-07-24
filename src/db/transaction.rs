//! Transaction management methods.
//!
//! Provides closures and manual transaction control for both read and write operations.
use crate::api::transaction::{ReadTransaction, WriteTransaction};
use crate::core::error::{Result, ResultExt, TransactionError};
use crate::core::hlc::HybridTimestamp;
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::storage::index_persistence::tracker::PersistenceTracker;
use crate::storage::wal::WriteOptions;
use std::sync::Arc;

/// Record persistence mutations after a successful commit.
///
/// Only scans write flags when persistence tracking is active,
/// avoiding unnecessary buffer scans when persistence is disabled.
fn record_tx_mutations(tracker: Option<&Arc<PersistenceTracker>>, tx: &WriteTransaction) {
    if let Some(tracker) = tracker {
        if tx.has_node_writes() || tx.has_edge_writes() {
            tracker.record_graph_mutation();
            tracker.record_temporal_mutation();
            // Labels are interned strings, so node/edge writes always mutate the interner
            tracker.record_string_mutation();
        }
        if tx.has_vector_writes() {
            tracker.record_vector_mutation();
        }
    }
}

impl AletheiaDB {
    /// Compute a snapshot timestamp that is strictly greater than the last commit
    /// timestamp, ensuring all previously committed transactions are visible, and
    /// **reserve** it by advancing the HLC frontier to that value.
    ///
    /// The MVCC visibility check uses strict less-than (`commit_ts < snapshot_ts`),
    /// so the snapshot must be strictly greater than the most recent commit to see it.
    /// When the system clock advances past the last commit, `now()` is sufficient.
    /// When it hasn't (e.g., multiple operations within the same clock tick), we
    /// advance one logical tick past the last commit.
    ///
    /// Reservation is load-bearing for snapshot isolation, not a mere optimization.
    /// The snapshot `S` we hand out is always strictly greater than the prior
    /// frontier in *both* branches, so in *both* branches we advance the frontier
    /// to `S` under the same lock guard (compare-and-advance) — the reservation is
    /// unconditional, not limited to the same-tick case. In the wallclock-advanced
    /// branch `now()` becomes the new frontier; in the same-tick branch the
    /// logical-incremented stamp does. Either way this guarantees any *subsequent*
    /// commit's HLC `send()` yields a stamp strictly greater than `S`. That matters
    /// most when a commit lands in the same wallclock tick as the read: without the
    /// reservation, its `send()` would recompute the identical stamp `S`, closing a
    /// superseded version's transaction-time interval at exactly `[C1, S)`; the
    /// half-open upper bound (`TimeRange::contains` uses `< end`) then excludes `S`,
    /// so the historical fallback misses the version and returns `NodeNotFound` — a
    /// snapshot-isolation violation. Reserving `S` sorts every later commit strictly
    /// after this read, keeping it invisible to the snapshot as required.
    ///
    /// Lock discipline: `current_timestamp` is first in the project lock order, and
    /// this method holds only that guard and calls into no other subsystem while
    /// held, so the read-compute-write sequence is atomic and lock-order-safe.
    fn snapshot_timestamp_for_read(&self) -> Result<Timestamp> {
        // Capture current time before acquiring lock to minimize lock contention.
        let now = crate::core::temporal::time::now();

        // Hold the frontier guard across read + compute + write so the
        // reservation is atomic against concurrent snapshots and commits.
        let mut frontier =
            self.current_timestamp
                .lock()
                .map_err(|_| TransactionError::LockPoisoned {
                    resource: "current_timestamp".to_string(),
                })?;
        let last_commit = *frontier;

        let snapshot =
            if now > last_commit {
                // Wallclock advanced past the last commit — now is sufficient.
                now
            } else {
                // Wallclock hasn't advanced — advance one logical tick past the last
                // commit so that `commit_ts < snapshot_ts` holds for all committed txns.
                // Use checked_add to handle potential overflow (theoretically requires
                // 4B+ events per microsecond, but we handle it for correctness).
                let next_logical = last_commit.logical().checked_add(1).ok_or(
                    crate::core::error::Error::Temporal(
                        crate::core::error::TemporalError::LogicalCounterOverflow {
                            wallclock: last_commit.wallclock(),
                            current_logical: last_commit.logical(),
                        },
                    ),
                )?;

                // SAFETY: wallclock is copied from an existing valid HybridTimestamp,
                // and next_logical is bounded by u32::MAX via checked_add above.
                HybridTimestamp::new_unchecked(last_commit.wallclock(), next_logical)
            };

        // Reserve the snapshot tick: `snapshot` is strictly greater than
        // `last_commit` in both branches, so this only ever advances the frontier.
        // No future commit can now reuse or land on this exact stamp.
        *frontier = snapshot;
        Ok(snapshot)
    }

    /// Create a new read-only transaction.
    ///
    /// Read-only transactions are lightweight and have zero overhead:
    /// - No write buffer
    /// - No WAL logging
    /// - Snapshot-based reads for consistency
    /// - No commit overhead
    ///
    /// # Errors
    ///
    /// Returns an error if the timestamp lock is poisoned.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::NodeId;
    /// # use aletheiadb::api::ReadOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let node_id = NodeId::new(1)?;
    /// let tx = db.read_transaction()?;
    /// let node = tx.get_node(node_id)?;
    /// // No commit needed - transaction is read-only
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn read_transaction(&self) -> Result<ReadTransaction> {
        let result = (|| {
            let tx_id = self.tx_id_gen.next();
            let snapshot_timestamp = self.snapshot_timestamp_for_read()?;
            self.visibility_manager.register_active(tx_id);
            let snapshot = self.visibility_manager.capture_snapshot(snapshot_timestamp);

            Ok(ReadTransaction::new(
                tx_id,
                snapshot,
                Arc::clone(&self.current),
                Arc::clone(&self.visibility_manager),
                Arc::clone(&self.historical),
            ))
        })();
        result.record_error_metric()
    }

    /// Execute a read-only operation in a transaction.
    ///
    /// This is a closure-based API that automatically manages the transaction lifecycle.
    /// The transaction is automatically cleaned up after the closure completes.
    ///
    /// The error type is generic, allowing you to use custom error types that implement
    /// `From<aletheiadb::Error>` for seamless error conversion with the `?` operator.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::NodeId;
    /// # use aletheiadb::api::ReadOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let node_id = NodeId::new(1)?;
    /// // With AletheiaDB's error type (default)
    /// let name: Option<aletheiadb::PropertyValue> = db.read(|tx| {
    ///     let node = tx.get_node(node_id)?;
    ///     Ok::<_, aletheiadb::Error>(node.get_property("name").cloned())
    /// })?;
    ///
    /// // With custom error type
    /// #[derive(Debug)]
    /// enum RepositoryError {
    ///     Database(aletheiadb::Error),
    ///     NotFound,
    /// }
    ///
    /// impl From<aletheiadb::Error> for RepositoryError {
    ///     fn from(e: aletheiadb::Error) -> Self {
    ///         RepositoryError::Database(e)
    ///     }
    /// }
    ///
    /// let name: Result<String, RepositoryError> = db.read(|tx| {
    ///     let node = tx.get_node(node_id)?; // ? operator works!
    ///     node.get_property("name")
    ///         .and_then(|v| v.as_str())
    ///         .map(|s| s.to_string())
    ///         .ok_or(RepositoryError::NotFound)
    /// });
    /// # Ok(())
    /// # }
    /// ```
    pub fn read<F, T, E>(&self, f: F) -> std::result::Result<T, E>
    where
        F: FnOnce(&ReadTransaction) -> std::result::Result<T, E>,
        E: From<crate::core::error::Error>,
    {
        let tx = self.read_transaction().map_err(E::from)?;
        f(&tx)
    }

    /// Create a new write transaction.
    ///
    /// Write transactions provide full ACID guarantees:
    /// - **Atomicity**: All-or-nothing commit via write buffering
    /// - **Consistency**: Referential integrity validation before commit
    /// - **Isolation**: Snapshot Isolation with write-write conflict detection
    /// - **Durability**: WAL with fsync for true durability
    ///
    /// # Errors
    ///
    /// Returns an error if the timestamp lock is poisoned.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMapBuilder};
    /// # use aletheiadb::core::NodeId;
    /// # use aletheiadb::api::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let props = PropertyMapBuilder::new().build();
    /// # let edge_props = PropertyMapBuilder::new().build();
    /// # let other = NodeId::new(2)?;
    /// let mut tx = db.write_transaction()?;
    /// let node_id = tx.create_node("Person", props)?;
    /// tx.create_edge(node_id, other, "KNOWS", edge_props)?;
    /// tx.commit()?;  // or tx.rollback()
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn write_transaction(&self) -> Result<WriteTransaction> {
        let result = (|| {
            // Read-only replica enforcement (Issue #3355, Slice A): reject
            // before any transaction state (tx id, snapshot, visibility
            // registration) is allocated.
            crate::db::replication_role::reject_if_replica(&self.role)?;

            let tx_id = self.tx_id_gen.next();
            let snapshot_timestamp = self.snapshot_timestamp_for_read()?;
            self.visibility_manager.register_active(tx_id);
            let snapshot = self.visibility_manager.capture_snapshot(snapshot_timestamp);

            let tx = WriteTransaction::new_with_clock_observed_at(
                tx_id,
                snapshot,
                Arc::clone(&self.current),
                Arc::clone(&self.historical),
                Arc::clone(&self.temporal_indexes),
                Arc::clone(&self.wal),
                Arc::clone(&self.current_timestamp),
                Arc::clone(&self.commit_clock_observed_at),
                Arc::clone(&self.visibility_manager),
                Arc::clone(&self.node_id_gen),
                Arc::clone(&self.edge_id_gen),
                Arc::clone(&self.version_id_gen),
            )
            .with_constraint_registry(Arc::clone(&self.constraint_registry))
            .with_in_flight_tracker(Arc::clone(&self.in_flight))
            .with_role_cell(Arc::clone(&self.role))
            .with_changefeed(Arc::clone(&self.changefeed));
            // GDPR crypto-shred (Issue #3359, PR-1b): attach the seal context when
            // the database has active designations (fail-closed on cipher-build).
            #[cfg(feature = "audit-export")]
            let tx = match self.sealing_context()? {
                Some(ctx) => tx.with_sealing_context(ctx),
                None => tx,
            };
            Ok(tx)
        })();
        result.record_error_metric()
    }

    /// Execute a write operation in a transaction.
    ///
    /// This is a closure-based API that automatically manages the transaction lifecycle.
    /// The transaction is automatically committed on Ok, or rolled back on Err.
    ///
    /// The error type is generic, allowing you to use custom error types that implement
    /// `From<aletheiadb::Error>` for seamless error conversion with the `?` operator.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMapBuilder};
    /// # use aletheiadb::core::NodeId;
    /// # use aletheiadb::api::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let props = PropertyMapBuilder::new().build();
    /// # let edge_props = PropertyMapBuilder::new().build();
    /// # let other = NodeId::new(2)?;
    /// # fn validate_node(id: NodeId) -> bool { true }
    /// // With AletheiaDB's error type (default)
    /// let node_id = db.write(|tx| {
    ///     let id = tx.create_node("Person", props.clone())?;
    ///     tx.create_edge(id, other, "KNOWS", edge_props.clone())?;
    ///     Ok::<_, aletheiadb::Error>(id)
    /// })?;
    ///
    /// #[derive(Debug)]
    /// enum RepositoryError {
    ///     Database(aletheiadb::Error),
    ///     ValidationFailed,
    /// }
    /// impl From<aletheiadb::Error> for RepositoryError {
    ///     fn from(e: aletheiadb::Error) -> Self { RepositoryError::Database(e) }
    /// }
    ///
    /// // With custom error type
    /// let node_id: Result<NodeId, RepositoryError> = db.write(|tx| {
    ///     let id = tx.create_node("Person", props)?; // ? operator works!
    ///     if !validate_node(id) {
    ///         return Err(RepositoryError::ValidationFailed);
    ///     }
    ///     Ok(id)
    /// });
    /// # Ok(())
    /// # }
    /// ```
    pub fn write<F, T, E>(&self, f: F) -> std::result::Result<T, E>
    where
        F: FnOnce(&mut WriteTransaction) -> std::result::Result<T, E>,
        E: From<crate::core::error::Error>,
    {
        let mut tx = self.write_transaction().map_err(E::from)?;
        let result = f(&mut tx)?;
        // Provenance-chain capture (Issue #3351): only when the chain is
        // enabled; a disabled chain adds nothing beyond this Option check.
        let chain_capture = self.chain.as_ref().map(|_| self.precapture_chain(&tx));
        record_tx_mutations(self.persistence_tracker.as_ref(), &tx);
        let commit_ts = tx.commit_with_timestamp().map_err(E::from)?;
        self.enqueue_chain_commit(chain_capture, commit_ts);
        Ok(result)
    }

    /// Execute a write operation and return both the result and commit timestamp.
    ///
    /// This is useful for benchmarks and tests that need to query the database
    /// at the exact commit timestamp to verify temporal semantics.
    ///
    /// The error type is generic, allowing you to use custom error types that implement
    /// `From<aletheiadb::Error>` for seamless error conversion with the `?` operator.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, PropertyMapBuilder};
    /// # use aletheiadb::core::NodeId;
    /// # use aletheiadb::core::temporal::Timestamp;
    /// # use aletheiadb::api::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let properties = PropertyMapBuilder::new().build();
    /// // With AletheiaDB's error type (default)
    /// let (node_id, commit_ts) = db.write_with_timestamp(|tx| {
    ///     tx.create_node("Person", properties.clone())
    /// })?;
    ///
    /// // Query at exact commit timestamp
    /// let node = db.get_node_at_time(node_id, commit_ts, commit_ts)?;
    ///
    /// #[derive(Debug)]
    /// enum RepositoryError {
    ///     Database(aletheiadb::Error),
    /// }
    /// impl From<aletheiadb::Error> for RepositoryError {
    ///     fn from(e: aletheiadb::Error) -> Self { RepositoryError::Database(e) }
    /// }
    ///
    /// // With custom error type
    /// let result: Result<(NodeId, Timestamp), RepositoryError> =
    ///     db.write_with_timestamp(|tx| {
    ///         let id = tx.create_node("Person", properties)?;
    ///         Ok(id)
    ///     });
    /// # Ok(())
    /// # }
    /// ```
    pub fn write_with_timestamp<F, T, E>(&self, f: F) -> std::result::Result<(T, Timestamp), E>
    where
        F: FnOnce(&mut WriteTransaction) -> std::result::Result<T, E>,
        E: From<crate::core::error::Error>,
    {
        let mut tx = self.write_transaction().map_err(E::from)?;
        let result = f(&mut tx)?;
        let chain_capture = self.chain.as_ref().map(|_| self.precapture_chain(&tx));
        record_tx_mutations(self.persistence_tracker.as_ref(), &tx);
        let commit_ts = tx.commit_with_timestamp().map_err(E::from)?;
        self.enqueue_chain_commit(chain_capture, commit_ts);
        Ok((result, commit_ts))
    }

    /// Execute a write operation with custom durability options.
    ///
    /// This allows overriding the database's default durability mode for
    /// specific transactions. Useful for bulk loading (Async mode) or
    /// critical operations (Synchronous mode override).
    ///
    /// The error type is generic, allowing you to use custom error types that implement
    /// `From<aletheiadb::Error>` for seamless error conversion with the `?` operator.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, WriteOptions, DurabilityMode, PropertyMap};
    /// # use aletheiadb::api::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// let bulk_data = vec![PropertyMap::new()];
    ///
    /// // Use Async mode for bulk loading (faster but less durable)
    /// let mode = DurabilityMode::async_mode_validated(100)?;
    /// let options = WriteOptions::new().with_durability(mode);
    ///
    /// // With AletheiaDB's error type (default)
    /// db.write_with_options(options.clone(), |tx| {
    ///     for item in &bulk_data {
    ///         tx.create_node("Item", item.clone())?;
    ///     }
    ///     Ok::<_, aletheiadb::Error>(())
    /// })?;
    ///
    /// #[derive(Debug)]
    /// enum RepositoryError {
    ///     Database(aletheiadb::Error),
    /// }
    /// impl From<aletheiadb::Error> for RepositoryError {
    ///     fn from(e: aletheiadb::Error) -> Self { RepositoryError::Database(e) }
    /// }
    ///
    /// // With custom error type
    /// let result: Result<(), RepositoryError> = db.write_with_options(options, |tx| {
    ///     for item in &bulk_data {
    ///         tx.create_node("Item", item.clone())?; // ? operator works!
    ///     }
    ///     Ok(())
    /// });
    /// # Ok(())
    /// # }
    /// ```
    pub fn write_with_options<F, T, E>(
        &self,
        options: WriteOptions,
        f: F,
    ) -> std::result::Result<T, E>
    where
        F: FnOnce(&mut WriteTransaction) -> std::result::Result<T, E>,
        E: From<crate::core::error::Error>,
    {
        let mut tx = self
            .write_transaction_with_options(options)
            .map_err(E::from)?;
        let result = f(&mut tx)?;
        let chain_capture = self.chain.as_ref().map(|_| self.precapture_chain(&tx));
        record_tx_mutations(self.persistence_tracker.as_ref(), &tx);
        let commit_ts = tx.commit_with_timestamp().map_err(E::from)?;
        self.enqueue_chain_commit(chain_capture, commit_ts);
        Ok(result)
    }

    /// Create a write transaction with custom durability options.
    ///
    /// This is the low-level API for creating transactions with specific
    /// durability settings. The transaction must be manually committed or
    /// rolled back.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, WriteOptions, DurabilityMode, PropertyMapBuilder};
    /// # use aletheiadb::api::WriteOps;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let props = PropertyMapBuilder::new().build();
    /// let options = WriteOptions::new()
    ///     .with_durability(DurabilityMode::Synchronous);
    ///
    /// let mut tx = db.write_transaction_with_options(options)?;
    /// tx.create_node("Critical", props)?;
    /// tx.commit()?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn write_transaction_with_options(
        &self,
        options: WriteOptions,
    ) -> Result<WriteTransaction> {
        let result = (|| {
            // Read-only replica enforcement (Issue #3355, Slice A): reject
            // before any transaction state is allocated.
            crate::db::replication_role::reject_if_replica(&self.role)?;

            let tx_id = self.tx_id_gen.next();
            let snapshot_timestamp = self.snapshot_timestamp_for_read()?;
            self.visibility_manager.register_active(tx_id);
            let snapshot = self.visibility_manager.capture_snapshot(snapshot_timestamp);

            let durability = options.effective_durability(self.default_durability);

            let tx = WriteTransaction::new_with_durability_and_clock_observed_at(
                tx_id,
                snapshot,
                Arc::clone(&self.current),
                Arc::clone(&self.historical),
                Arc::clone(&self.temporal_indexes),
                Arc::clone(&self.wal),
                Arc::clone(&self.current_timestamp),
                Arc::clone(&self.commit_clock_observed_at),
                Arc::clone(&self.visibility_manager),
                Arc::clone(&self.node_id_gen),
                Arc::clone(&self.edge_id_gen),
                Arc::clone(&self.version_id_gen),
                durability,
            )
            .with_constraint_registry(Arc::clone(&self.constraint_registry))
            .with_in_flight_tracker(Arc::clone(&self.in_flight))
            .with_role_cell(Arc::clone(&self.role))
            .with_changefeed(Arc::clone(&self.changefeed));
            // GDPR crypto-shred (Issue #3359, PR-1b): attach the seal context when
            // the database has active designations (fail-closed on cipher-build).
            #[cfg(feature = "audit-export")]
            let tx = match self.sealing_context()? {
                Some(ctx) => tx.with_sealing_context(ctx),
                None => tx,
            };
            Ok(tx)
        })();
        result.record_error_metric()
    }
}
