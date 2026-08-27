//! Transaction management methods.
//!
//! Provides closures and manual transaction control for both read and write operations.
use crate::api::transaction::{ReadTransaction, WriteTransaction};
use crate::core::error::{Result, ResultExt};
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
    /// Hand out a snapshot timestamp, reserving it against later commits.
    ///
    /// Delegates to [`CommitClock::snapshot_for_read`], which does this without
    /// taking any lock. The reservation, why it is load-bearing rather than an
    /// optimization, and why a lock-free reader has to know whether a commit is
    /// in flight are all documented on
    /// [`crate::core::commit_clock`] -- that module is the place to read before
    /// changing any of this.
    ///
    /// This used to lock the same mutex committers hold across the *entire*
    /// commit, so every temporal read, `AS OF` query, and snapshot-isolated
    /// scan queued behind the write path and behind each other.
    fn snapshot_timestamp_for_read(&self) -> Result<Timestamp> {
        self.current_timestamp.snapshot_for_read()
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
            // Deliberately NOT registered in the active set. Membership is only
            // ever tested for the transaction that *created* a version, and a
            // read transaction creates none -- so registering one is invisible
            // to every visibility check and costs two lock acquisitions plus two
            // copy-on-write clones of the whole set (here and on drop). See
            // `TxVisibilityManager`.
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

#[cfg(test)]
mod snapshot_visibility_tests {
    use crate::AletheiaDB;
    use crate::api::transaction::{ReadOps, WriteOps};
    use crate::core::property::PropertyValue;
    use crate::properties;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// End-to-end cover for the invariant the lock-free snapshot path rests on:
    /// a commit becomes *visible* (MVCC `commit_ts < snapshot_ts`) only once its
    /// writes are actually readable.
    ///
    /// Committers publish visibility after `finalize_current_commit_timestamps`.
    /// Publish it any earlier -- or infer "in flight" wrongly in
    /// [`crate::core::commit_clock`] -- and a reader is handed a snapshot past a
    /// commit whose node is still absent from current storage, so this read
    /// fails to find a node it has already been told it can see.
    ///
    /// Each writer commit creates one node and records its id only after the
    /// commit returns, so every id a reader picks up is, by construction,
    /// already committed: a `NodeNotFound` here is the anomaly, not a race in
    /// the test.
    #[test]
    fn a_visible_commit_is_always_readable() {
        let db = Arc::new(AletheiaDB::new().expect("db"));
        let stop = Arc::new(AtomicBool::new(false));
        let published = Arc::new(AtomicU64::new(0));
        let ids: Arc<dashmap::DashMap<u64, u64>> = Arc::new(dashmap::DashMap::new());

        let writer = {
            let db = Arc::clone(&db);
            let stop = Arc::clone(&stop);
            let published = Arc::clone(&published);
            let ids = Arc::clone(&ids);
            std::thread::spawn(move || {
                let mut seq = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let mut tx = db.write_transaction().expect("tx");
                    let id = tx
                        .create_node("Seq", properties! { "seq" => seq as i64 })
                        .expect("create");
                    tx.commit().expect("commit");
                    // Only after commit returns: from here the node is
                    // unambiguously committed, so any reader that can see this
                    // slot must be able to read the node.
                    ids.insert(seq, id.as_u64());
                    published.store(seq + 1, Ordering::Release);
                    seq += 1;
                }
                seq
            })
        };

        let readers: Vec<_> = (0..3)
            .map(|_| {
                let db = Arc::clone(&db);
                let stop = Arc::clone(&stop);
                let published = Arc::clone(&published);
                let ids = Arc::clone(&ids);
                std::thread::spawn(move || {
                    let mut checked = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let count = published.load(Ordering::Acquire);
                        if count == 0 {
                            continue;
                        }
                        let tx = db.read_transaction().expect("read tx");
                        // Every already-published node must be readable at this
                        // snapshot -- including the newest, which is exactly the
                        // one an unpublished-but-visible commit would lose.
                        for seq in count.saturating_sub(8)..count {
                            let Some(id) = ids.get(&seq).map(|e| *e.value()) else {
                                continue;
                            };
                            let node = tx
                                .get_node(crate::core::id::NodeId::new(id).expect("id"))
                                .unwrap_or_else(|e| {
                                    panic!(
                                        "committed node seq={seq} id={id} was visible to the \
                                         snapshot but could not be read: {e}"
                                    )
                                });
                            assert_eq!(
                                node.properties.get("seq"),
                                Some(&PropertyValue::Int(seq as i64)),
                                "node seq={seq} came back with the wrong contents"
                            );
                            checked += 1;
                        }
                    }
                    checked
                })
            })
            .collect();

        std::thread::sleep(std::time::Duration::from_millis(500));
        stop.store(true, Ordering::Relaxed);

        let commits = writer.join().expect("writer");
        let checked: u64 = readers.into_iter().map(|r| r.join().expect("reader")).sum();

        assert!(commits > 0, "writer made no progress");
        assert!(
            checked > 0,
            "readers verified nothing, so the test proved nothing"
        );
    }
}
