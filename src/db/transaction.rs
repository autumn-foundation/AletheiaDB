use crate::api::transaction::{ReadTransaction, WriteTransaction};
use crate::core::temporal::Timestamp;
use crate::db::GallifreyDB;
use crate::storage::wal::WriteOptions;
use crate::utils::error::Result;
use crate::utils::lock::MutexExt;
use std::sync::Arc;

impl GallifreyDB {
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
    /// ```ignore
    /// let tx = db.read_transaction()?;
    /// let node = tx.get_node(node_id)?;
    /// // No commit needed - transaction is read-only
    /// ```
    pub fn read_transaction(&self) -> Result<ReadTransaction> {
        let tx_id = self.tx_id_gen.next();
        let snapshot_timestamp = *self.current_timestamp.lock_or_err()?;

        // Register as active
        self.visibility_manager.register_active(tx_id);

        // Capture snapshot
        let snapshot = self.visibility_manager.capture_snapshot(snapshot_timestamp);

        Ok(ReadTransaction::new(
            tx_id,
            snapshot,
            Arc::clone(&self.current),
            Arc::clone(&self.visibility_manager),
            Arc::clone(&self.historical),
        ))
    }

    /// Execute a read-only operation in a transaction.
    ///
    /// This is a closure-based API that automatically manages the transaction lifecycle.
    /// The transaction is automatically cleaned up after the closure completes.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let name = db.read(|tx| {
    ///     let node = tx.get_node(node_id)?;
    ///     Ok(node.get_property("name").cloned())
    /// })?;
    /// ```
    pub fn read<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&ReadTransaction) -> Result<T>,
    {
        let tx = self.read_transaction()?;
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
    /// ```ignore
    /// let mut tx = db.write_transaction()?;
    /// let node_id = tx.create_node("Person", props)?;
    /// tx.create_edge(node_id, other, "KNOWS", edge_props)?;
    /// tx.commit()?;  // or tx.rollback()
    /// ```
    pub fn write_transaction(&self) -> Result<WriteTransaction> {
        let tx_id = self.tx_id_gen.next();

        // Capture snapshot timestamp using current wallclock time, ensuring it's
        // >= the last commit timestamp (monotonicity). This allows the transaction
        // to see all commits that happened before it started.
        let snapshot_timestamp = {
            let ts = self.current_timestamp.lock_or_err()?;
            std::cmp::max(crate::core::temporal::time::now(), *ts)
        };

        // Register as active
        self.visibility_manager.register_active(tx_id);

        // Capture snapshot
        let snapshot = self.visibility_manager.capture_snapshot(snapshot_timestamp);

        Ok(WriteTransaction::new(
            tx_id,
            snapshot,
            Arc::clone(&self.current),
            Arc::clone(&self.historical),
            Arc::clone(&self.temporal_indexes),
            Arc::clone(&self.wal),
            Arc::clone(&self.current_timestamp),
            Arc::clone(&self.visibility_manager),
            Arc::clone(&self.node_id_gen),
            Arc::clone(&self.edge_id_gen),
            Arc::clone(&self.version_id_gen),
        ))
    }

    /// Execute a write operation in a transaction.
    ///
    /// This is a closure-based API that automatically manages the transaction lifecycle.
    /// The transaction is automatically committed on Ok, or rolled back on Err.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let node_id = db.write(|tx| {
    ///     let id = tx.create_node("Person", props)?;
    ///     tx.create_edge(id, other, "KNOWS", edge_props)?;
    ///     Ok(id)
    /// })?;
    /// ```
    pub fn write<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut WriteTransaction) -> Result<T>,
    {
        let mut tx = self.write_transaction()?;
        let result = f(&mut tx)?;

        // Track mutations for persistence before committing
        let has_node_writes = tx.has_node_writes();
        let has_edge_writes = tx.has_edge_writes();
        let has_vector_writes = tx.has_vector_writes();

        tx.commit()?; // Ignore commit timestamp for simple write()

        // Record mutations after successful commit
        if let Some(ref tracker) = self.persistence_tracker {
            if has_node_writes || has_edge_writes {
                tracker.record_graph_mutation();
                tracker.record_temporal_mutation();
                // String interner mutations happen with every node/edge (labels)
                tracker.record_string_mutation();
            }
            if has_vector_writes {
                tracker.record_vector_mutation();
            }
        }

        Ok(result)
    }

    /// Execute a write operation and return both the result and commit timestamp.
    ///
    /// This is useful for benchmarks and tests that need to query the database
    /// at the exact commit timestamp to verify temporal semantics.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let (node_id, commit_ts) = db.write_with_timestamp(|tx| {
    ///     tx.create_node("Person", properties)
    /// })?;
    ///
    /// // Query at exact commit timestamp
    /// let node = db.get_node_at_time(node_id, commit_ts, commit_ts)?;
    /// ```
    pub fn write_with_timestamp<F, T>(&self, f: F) -> Result<(T, Timestamp)>
    where
        F: FnOnce(&mut WriteTransaction) -> Result<T>,
    {
        let mut tx = self.write_transaction()?;
        let result = f(&mut tx)?;

        // Track mutations for persistence before committing
        let has_node_writes = tx.has_node_writes();
        let has_edge_writes = tx.has_edge_writes();
        let has_vector_writes = tx.has_vector_writes();

        let commit_ts = tx.commit_with_timestamp()?;

        // Record mutations after successful commit
        if let Some(ref tracker) = self.persistence_tracker {
            if has_node_writes || has_edge_writes {
                tracker.record_graph_mutation();
                tracker.record_temporal_mutation();
            }
            if has_vector_writes {
                tracker.record_vector_mutation();
            }
        }

        Ok((result, commit_ts))
    }

    /// Execute a write operation with custom durability options.
    ///
    /// This allows overriding the database's default durability mode for
    /// specific transactions. Useful for bulk loading (Async mode) or
    /// critical operations (Synchronous mode override).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::{GallifreyDB, WriteOptions, DurabilityMode};
    ///
    /// let db = GallifreyDB::new();
    ///
    /// // Use Async mode for bulk loading (faster but less durable)
    /// let options = WriteOptions::new()
    ///     .with_durability(DurabilityMode::async_mode(100));
    ///
    /// db.write_with_options(options, |tx| {
    ///     for item in bulk_data {
    ///         tx.create_node("Item", item.into())?;
    ///     }
    ///     Ok(())
    /// })?;
    /// ```
    pub fn write_with_options<F, T>(&self, options: WriteOptions, f: F) -> Result<T>
    where
        F: FnOnce(&mut WriteTransaction) -> Result<T>,
    {
        let mut tx = self.write_transaction_with_options(options)?;
        let result = f(&mut tx)?;

        // Track mutations for persistence before committing
        let has_node_writes = tx.has_node_writes();
        let has_edge_writes = tx.has_edge_writes();
        let has_vector_writes = tx.has_vector_writes();

        tx.commit()?; // Ignore commit timestamp for simple write_with_options()

        // Record mutations after successful commit
        if let Some(ref tracker) = self.persistence_tracker {
            if has_node_writes || has_edge_writes {
                tracker.record_graph_mutation();
                tracker.record_temporal_mutation();
                // String interner mutations happen with every node/edge (labels)
                tracker.record_string_mutation();
            }
            if has_vector_writes {
                tracker.record_vector_mutation();
            }
        }

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
    /// ```ignore
    /// let options = WriteOptions::new()
    ///     .with_durability(DurabilityMode::Synchronous);
    ///
    /// let mut tx = db.write_transaction_with_options(options)?;
    /// tx.create_node("Critical", props)?;
    /// tx.commit()?;
    /// ```
    pub fn write_transaction_with_options(
        &self,
        options: WriteOptions,
    ) -> Result<WriteTransaction> {
        let tx_id = self.tx_id_gen.next();

        // Capture snapshot timestamp using current wallclock time, ensuring it's
        // >= the last commit timestamp (monotonicity). This allows the transaction
        // to see all commits that happened before it started.
        let snapshot_timestamp = {
            let ts = self.current_timestamp.lock_or_err()?;
            std::cmp::max(crate::core::temporal::time::now(), *ts)
        };

        // Register as active
        self.visibility_manager.register_active(tx_id);

        // Capture snapshot
        let snapshot = self.visibility_manager.capture_snapshot(snapshot_timestamp);

        // Determine effective durability mode
        let durability = options.effective_durability(self.default_durability);

        Ok(WriteTransaction::new_with_durability(
            tx_id,
            snapshot,
            Arc::clone(&self.current),
            Arc::clone(&self.historical),
            Arc::clone(&self.temporal_indexes),
            Arc::clone(&self.wal),
            Arc::clone(&self.current_timestamp),
            Arc::clone(&self.visibility_manager),
            Arc::clone(&self.node_id_gen),
            Arc::clone(&self.edge_id_gen),
            Arc::clone(&self.version_id_gen),
            durability,
        ))
    }
}
