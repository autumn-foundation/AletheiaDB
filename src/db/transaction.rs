use crate::api::transaction::{ReadTransaction, WriteTransaction};
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::storage::wal::WriteOptions;
use crate::utils::error::Result;
use std::sync::Arc;

impl AletheiaDB {
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
        let snapshot_timestamp = *self.current_timestamp.lock().unwrap();

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
    /// The error type is generic, allowing you to use custom error types that implement
    /// `From<aletheiadb::Error>` for seamless error conversion with the `?` operator.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // With AletheiaDB's error type (default)
    /// let name = db.read(|tx| {
    ///     let node = tx.get_node(node_id)?;
    ///     Ok(node.get_property("name").cloned())
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
    ///         .ok_or(RepositoryError::NotFound)
    /// });
    /// ```
    pub fn read<F, T, E>(&self, f: F) -> std::result::Result<T, E>
    where
        F: FnOnce(&ReadTransaction) -> std::result::Result<T, E>,
        E: From<crate::utils::error::Error>,
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
            let ts = self.current_timestamp.lock().unwrap();
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
    /// The error type is generic, allowing you to use custom error types that implement
    /// `From<aletheiadb::Error>` for seamless error conversion with the `?` operator.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // With AletheiaDB's error type (default)
    /// let node_id = db.write(|tx| {
    ///     let id = tx.create_node("Person", props)?;
    ///     tx.create_edge(id, other, "KNOWS", edge_props)?;
    ///     Ok(id)
    /// })?;
    ///
    /// // With custom error type
    /// let node_id: Result<NodeId, RepositoryError> = db.write(|tx| {
    ///     let id = tx.create_node("Person", props)?; // ? operator works!
    ///     if !validate_node(id) {
    ///         return Err(RepositoryError::ValidationFailed);
    ///     }
    ///     Ok(id)
    /// });
    /// ```
    pub fn write<F, T, E>(&self, f: F) -> std::result::Result<T, E>
    where
        F: FnOnce(&mut WriteTransaction) -> std::result::Result<T, E>,
        E: From<crate::utils::error::Error>,
    {
        let mut tx = self.write_transaction().map_err(E::from)?;
        let result = f(&mut tx)?;

        // Track mutations for persistence before committing
        let has_node_writes = tx.has_node_writes();
        let has_edge_writes = tx.has_edge_writes();
        let has_vector_writes = tx.has_vector_writes();

        tx.commit().map_err(E::from)?; // Ignore commit timestamp for simple write()

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
    /// The error type is generic, allowing you to use custom error types that implement
    /// `From<aletheiadb::Error>` for seamless error conversion with the `?` operator.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // With AletheiaDB's error type (default)
    /// let (node_id, commit_ts) = db.write_with_timestamp(|tx| {
    ///     tx.create_node("Person", properties)
    /// })?;
    ///
    /// // Query at exact commit timestamp
    /// let node = db.get_node_at_time(node_id, commit_ts, commit_ts)?;
    ///
    /// // With custom error type
    /// let result: Result<(NodeId, Timestamp), RepositoryError> =
    ///     db.write_with_timestamp(|tx| {
    ///         let id = tx.create_node("Person", properties)?;
    ///         Ok(id)
    ///     });
    /// ```
    pub fn write_with_timestamp<F, T, E>(&self, f: F) -> std::result::Result<(T, Timestamp), E>
    where
        F: FnOnce(&mut WriteTransaction) -> std::result::Result<T, E>,
        E: From<crate::utils::error::Error>,
    {
        let mut tx = self.write_transaction().map_err(E::from)?;
        let result = f(&mut tx)?;

        // Track mutations for persistence before committing
        let has_node_writes = tx.has_node_writes();
        let has_edge_writes = tx.has_edge_writes();
        let has_vector_writes = tx.has_vector_writes();

        let commit_ts = tx.commit_with_timestamp().map_err(E::from)?;

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
    /// The error type is generic, allowing you to use custom error types that implement
    /// `From<aletheiadb::Error>` for seamless error conversion with the `?` operator.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::{AletheiaDB, WriteOptions, DurabilityMode};
    ///
    /// let db = AletheiaDB::new();
    ///
    /// // Use Async mode for bulk loading (faster but less durable)
    /// let mode = DurabilityMode::async_mode_validated(100)?;
    /// let options = WriteOptions::new().with_durability(mode);
    ///
    /// // With AletheiaDB's error type (default)
    /// db.write_with_options(options, |tx| {
    ///     for item in bulk_data {
    ///         tx.create_node("Item", item.into())?;
    ///     }
    ///     Ok(())
    /// })?;
    ///
    /// // With custom error type
    /// let result: Result<(), RepositoryError> = db.write_with_options(options, |tx| {
    ///     for item in bulk_data {
    ///         tx.create_node("Item", item.into())?; // ? operator works!
    ///     }
    ///     Ok(())
    /// });
    /// ```
    pub fn write_with_options<F, T, E>(
        &self,
        options: WriteOptions,
        f: F,
    ) -> std::result::Result<T, E>
    where
        F: FnOnce(&mut WriteTransaction) -> std::result::Result<T, E>,
        E: From<crate::utils::error::Error>,
    {
        let mut tx = self
            .write_transaction_with_options(options)
            .map_err(E::from)?;
        let result = f(&mut tx)?;

        // Track mutations for persistence before committing
        let has_node_writes = tx.has_node_writes();
        let has_edge_writes = tx.has_edge_writes();
        let has_vector_writes = tx.has_vector_writes();

        tx.commit().map_err(E::from)?; // Ignore commit timestamp for simple write_with_options()

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
            let ts = self.current_timestamp.lock().unwrap();
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
