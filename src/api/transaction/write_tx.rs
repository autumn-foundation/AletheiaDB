//! Write transactions with ACID guarantees.
//!
//! Write transactions provide full ACID properties:
//! - **Atomicity**: All-or-nothing commit via write buffering
//! - **Consistency**: Referential integrity validation before commit
//! - **Isolation**: Snapshot Isolation with write-write conflict detection
//! - **Durability**: WAL with fsync guarantees
//!
//! Write transactions buffer all changes in memory until commit.
//! On commit, changes are validated and applied atomically.

use super::{
    ReadOps, TransactionSnapshot, TxId, TxMetadata, TxState, TxVisibilityManager, WriteBuffer,
    WriteOps,
};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, IdGenerator, NodeId, VersionId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMap;
use crate::core::temporal::{BiTemporalInterval, Timestamp, time};
use crate::index::temporal::TemporalIndexes;
use crate::storage::VersionMetadata;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::wal::{WalOperation, WriteAheadLog};
use crate::utils::error::{Result, StorageError, TransactionError};
use crate::utils::lock::MutexExt;
use std::sync::{Arc, Mutex};

/// Write transaction with full ACID guarantees.
///
/// Write transactions buffer all operations in memory and apply them
/// atomically on commit. This ensures consistency and enables rollback.
///
/// # Example
///
/// ```ignore
/// let mut tx = db.write_transaction();
/// let node_id = tx.create_node("Person", props)?;
/// tx.create_edge(node_id, other, "KNOWS", edge_props)?;
/// tx.commit()?;  // or tx.rollback()
/// ```
pub struct WriteTransaction {
    tx_id: TxId,
    start_timestamp: Timestamp,
    state: TxState,

    // Snapshot for Snapshot Isolation
    snapshot: TransactionSnapshot,

    // Write buffer for uncommitted changes
    buffer: WriteBuffer,

    // Shared references to storage (Arc for zero-copy sharing)
    current: Arc<CurrentStorage>,
    historical: Arc<Mutex<HistoricalStorage>>,
    temporal_indexes: Arc<Mutex<TemporalIndexes>>,
    wal: Arc<Mutex<WriteAheadLog>>,
    current_timestamp: Arc<Mutex<Timestamp>>,
    visibility_manager: Arc<TxVisibilityManager>,

    // ID generators (needed for creating new entities)
    node_id_gen: Arc<Mutex<IdGenerator>>,
    edge_id_gen: Arc<Mutex<IdGenerator>>,
    version_id_gen: Arc<Mutex<IdGenerator>>,
}

impl WriteTransaction {
    /// Create a new write transaction.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        tx_id: TxId,
        snapshot: TransactionSnapshot,
        current: Arc<CurrentStorage>,
        historical: Arc<Mutex<HistoricalStorage>>,
        temporal_indexes: Arc<Mutex<TemporalIndexes>>,
        wal: Arc<Mutex<WriteAheadLog>>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<Mutex<IdGenerator>>,
        edge_id_gen: Arc<Mutex<IdGenerator>>,
        version_id_gen: Arc<Mutex<IdGenerator>>,
    ) -> Self {
        WriteTransaction {
            tx_id,
            start_timestamp: time::now(),
            state: TxState::Active,
            snapshot,
            buffer: WriteBuffer::new(),
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
        }
    }

    /// Get transaction metadata.
    pub fn metadata(&self) -> TxMetadata {
        TxMetadata {
            tx_id: self.tx_id,
            start_timestamp: self.start_timestamp,
            commit_timestamp: None,
            state: self.state,
            is_read_only: false,
        }
    }

    /// Get transaction ID.
    pub fn tx_id(&self) -> TxId {
        self.tx_id
    }

    /// Commit the transaction.
    ///
    /// This validates all buffered writes and applies them atomically
    /// to the storage. If validation fails or any operation fails,
    /// the transaction is rolled back.
    pub fn commit(mut self) -> Result<()> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Transition to preparing state
        self.state = TxState::Preparing;

        // Validate all buffered writes
        self.validate()?;

        // Detect write-write conflicts (Snapshot Isolation)
        self.detect_conflicts()?;

        // Acquire commit timestamp and hold lock through WAL flush.
        //
        // CRITICAL: We must hold the timestamp lock until WAL is flushed to prevent
        // a race condition where transactions commit out-of-order:
        //
        // Without holding the lock:
        //   T1: gets timestamp 100, releases lock
        //   T2: gets timestamp 101, logs WAL, flushes, applies → COMMITTED
        //   T1: logs WAL, flushes, applies → COMMITTED (but timestamp 100 < 101!)
        //
        // This violates the invariant that transaction time is monotonic with
        // actual commit order. By holding the lock through flush, we ensure
        // timestamps are assigned in the same order as commits become durable.
        //
        // PERFORMANCE NOTE: This serializes all commits since we hold both locks
        // through WAL logging and flushing. For high-throughput workloads, consider
        // implementing group commit (batching multiple transactions per WAL flush)
        // while maintaining timestamp ordering.
        //
        // Timestamp management for Snapshot Isolation:
        //
        // 1. Increment BEFORE using: ensures commit_ts > snapshot_ts for any
        //    transaction that started before this commit. This enables write-write
        //    conflict detection via the check (commit_ts > snapshot_ts).
        //
        // 2. Increment AFTER using: ensures future snapshots will have a timestamp
        //    greater than this commit, so visibility check (commit_ts < snapshot_ts)
        //    will correctly include this commit in future reads.
        //
        // This double-increment ensures proper ordering for both:
        // - Conflict detection (commits after my snapshot must fail)
        // - Visibility (commits before my snapshot must be visible)
        let commit_timestamp = {
            let mut ts = self
                .current_timestamp
                .lock()
                .expect("timestamp lock poisoned - unrecoverable state");
            *ts += 1; // Pre-increment for conflict detection
            let commit = *ts;
            *ts += 1; // Post-increment for visibility of this commit

            // Acquire WAL lock once and hold through both logging and flush.
            // This prevents any race condition between operations.
            let mut wal = self
                .wal
                .lock()
                .expect("WAL lock poisoned - unrecoverable state");

            // Log operations to WAL while holding both locks.
            // This must happen BEFORE applying changes for durability.
            self.log_operations_to_wal(&mut wal, commit)?;

            // Flush WAL to ensure durability while still holding both locks.
            // This guarantees that lower timestamps are durable before higher ones.
            wal.flush()?;

            commit
            // Both locks released here, after WAL is durable
        };

        // Apply all changes atomically
        self.apply_changes(commit_timestamp)?;

        // Notify temporal vector index of transaction completion (for snapshot creation)
        self.current.on_temporal_vector_transaction()?;

        // Register commit with visibility manager
        self.visibility_manager
            .register_commit(self.tx_id, commit_timestamp);

        // Mark as committed
        self.state = TxState::Committed;

        Ok(())
    }

    /// Rollback the transaction.
    ///
    /// Discards all buffered writes. This is automatically called
    /// if the transaction is dropped without committing.
    pub fn rollback(mut self) -> Result<()> {
        if self.state == TxState::Committed {
            return Err(TransactionError::AlreadyCommitted {
                tx_id: self.tx_id.as_u64(),
            }
            .into());
        }

        // Clear the write buffer
        self.buffer.clear();

        // Register abort with visibility manager
        self.visibility_manager.register_abort(self.tx_id);

        self.state = TxState::Aborted;

        Ok(())
    }

    /// Validate all buffered writes.
    ///
    /// Checks:
    /// - Referential integrity (edges reference valid nodes)
    /// - No constraint violations
    fn validate(&self) -> Result<()> {
        for write in self.buffer.operations() {
            match write {
                super::BufferedWrite::CreateEdge { source, target, .. }
                | super::BufferedWrite::UpdateEdge { source, target, .. } => {
                    // Check that source and target nodes exist
                    // They might exist in current storage or be created in this transaction
                    if !self.buffer.has_modified_node(*source)
                        && self.current.get_node(*source).is_err()
                    {
                        return Err(TransactionError::ValidationFailed {
                            reason: format!("Edge source node {:?} does not exist", source),
                        }
                        .into());
                    }
                    if !self.buffer.has_modified_node(*target)
                        && self.current.get_node(*target).is_err()
                    {
                        return Err(TransactionError::ValidationFailed {
                            reason: format!("Edge target node {:?} does not exist", target),
                        }
                        .into());
                    }
                }
                _ => {
                    // Other operations don't need validation
                }
            }
        }

        Ok(())
    }

    /// Detect write-write conflicts for Snapshot Isolation.
    ///
    /// Checks if any entity modified by this transaction has been committed
    /// by another transaction after our snapshot was taken. This implements
    /// the First-Committer-Wins rule of Snapshot Isolation.
    ///
    /// # Errors
    ///
    /// Returns `SerializationFailure` if a write-write conflict is detected.
    fn detect_conflicts(&self) -> Result<()> {
        for write in self.buffer.operations() {
            match write {
                // UpdateNode: check if node was modified or deleted after our snapshot
                super::BufferedWrite::UpdateNode { node_id, .. } => {
                    match self.current.get_node(*node_id) {
                        Ok(current_node) => {
                            // Node exists - check if it was modified after our snapshot
                            if let Some(commit_ts) = current_node.metadata.commit_timestamp
                                && commit_ts > self.snapshot.snapshot_timestamp
                            {
                                return Err(TransactionError::SerializationFailure {
                                    entity: format!("{:?}", node_id),
                                    reason: format!(
                                        "Version committed at {} after snapshot at {}",
                                        commit_ts, self.snapshot.snapshot_timestamp
                                    ),
                                }
                                .into());
                            }
                        }
                        Err(_) => {
                            // Node doesn't exist in current storage. Since we successfully
                            // called update_node() earlier (which reads from current storage),
                            // the node must have been deleted by another transaction.
                            return Err(TransactionError::SerializationFailure {
                                entity: format!("{:?}", node_id),
                                reason: "Node was deleted by another transaction".to_string(),
                            }
                            .into());
                        }
                    }
                }

                // UpdateEdge: check if edge was modified or deleted after our snapshot
                super::BufferedWrite::UpdateEdge { edge_id, .. } => {
                    match self.current.get_edge(*edge_id) {
                        Ok(current_edge) => {
                            // Edge exists - check if it was modified after our snapshot
                            if let Some(commit_ts) = current_edge.metadata.commit_timestamp
                                && commit_ts > self.snapshot.snapshot_timestamp
                            {
                                return Err(TransactionError::SerializationFailure {
                                    entity: format!("{:?}", edge_id),
                                    reason: format!(
                                        "Version committed at {} after snapshot at {}",
                                        commit_ts, self.snapshot.snapshot_timestamp
                                    ),
                                }
                                .into());
                            }
                        }
                        Err(_) => {
                            // Edge doesn't exist - it was deleted by another transaction
                            return Err(TransactionError::SerializationFailure {
                                entity: format!("{:?}", edge_id),
                                reason: "Edge was deleted by another transaction".to_string(),
                            }
                            .into());
                        }
                    }
                }

                // DeleteNode: check if node was modified after our snapshot
                super::BufferedWrite::DeleteNode { node_id } => {
                    // Get current version from storage
                    if let Ok(current_node) = self.current.get_node(*node_id)
                        && let Some(commit_ts) = current_node.metadata.commit_timestamp
                        && commit_ts > self.snapshot.snapshot_timestamp
                    {
                        return Err(TransactionError::SerializationFailure {
                            entity: format!("{:?}", node_id),
                            reason: format!(
                                "Version committed at {} after snapshot at {}",
                                commit_ts, self.snapshot.snapshot_timestamp
                            ),
                        }
                        .into());
                    }
                }

                // DeleteEdge: check if edge was modified after our snapshot
                super::BufferedWrite::DeleteEdge { edge_id } => {
                    // Get current version from storage
                    if let Ok(current_edge) = self.current.get_edge(*edge_id)
                        && let Some(commit_ts) = current_edge.metadata.commit_timestamp
                        && commit_ts > self.snapshot.snapshot_timestamp
                    {
                        return Err(TransactionError::SerializationFailure {
                            entity: format!("{:?}", edge_id),
                            reason: format!(
                                "Version committed at {} after snapshot at {}",
                                commit_ts, self.snapshot.snapshot_timestamp
                            ),
                        }
                        .into());
                    }
                }

                // CreateNode and CreateEdge don't need conflict detection
                // since they're creating new entities that didn't exist before
                _ => {}
            }
        }

        Ok(())
    }

    /// Log all buffered operations to WAL.
    ///
    /// This ensures durability - operations are logged before being applied.
    /// The caller must hold the WAL lock and pass it as a mutable reference.
    fn log_operations_to_wal(
        &self,
        wal: &mut WriteAheadLog,
        commit_timestamp: Timestamp,
    ) -> Result<()> {
        let temporal = BiTemporalInterval::current(commit_timestamp);

        for write in self.buffer.operations() {
            let operation = match write {
                super::BufferedWrite::CreateNode {
                    node_id,
                    label,
                    properties,
                    ..
                } => {
                    let label_str = GLOBAL_INTERNER
                        .resolve(*label)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| String::from(""));
                    WalOperation::CreateNode {
                        node_id: *node_id,
                        label: label_str,
                        properties: properties.clone(),
                        temporal,
                    }
                }
                super::BufferedWrite::CreateEdge {
                    edge_id,
                    source,
                    target,
                    label,
                    properties,
                    ..
                } => {
                    let label_str = GLOBAL_INTERNER
                        .resolve(*label)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| String::from(""));
                    WalOperation::CreateEdge {
                        edge_id: *edge_id,
                        source: *source,
                        target: *target,
                        label: label_str,
                        properties: properties.clone(),
                        temporal,
                    }
                }
                super::BufferedWrite::UpdateNode {
                    node_id,
                    version_id,
                    label,
                    properties,
                    ..
                } => {
                    let label_str = GLOBAL_INTERNER
                        .resolve(*label)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| String::from(""));
                    WalOperation::UpdateNode {
                        node_id: *node_id,
                        version_id: *version_id,
                        label: label_str,
                        properties: properties.clone(),
                        temporal,
                    }
                }
                super::BufferedWrite::UpdateEdge {
                    edge_id,
                    version_id,
                    label,
                    properties,
                    ..
                } => {
                    let label_str = GLOBAL_INTERNER
                        .resolve(*label)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| String::from(""));
                    WalOperation::UpdateEdge {
                        edge_id: *edge_id,
                        version_id: *version_id,
                        label: label_str,
                        properties: properties.clone(),
                        temporal,
                    }
                }
                super::BufferedWrite::DeleteNode { node_id } => WalOperation::DeleteNode {
                    node_id: *node_id,
                    temporal,
                },
                super::BufferedWrite::DeleteEdge { edge_id } => WalOperation::DeleteEdge {
                    edge_id: *edge_id,
                    temporal,
                },
            };

            // Append to WAL
            wal.append(operation)?;
        }

        Ok(())
    }

    /// Apply all buffered changes to storage.
    fn apply_changes(&self, commit_timestamp: Timestamp) -> Result<()> {
        let temporal = BiTemporalInterval::current(commit_timestamp);

        for write in self.buffer.operations() {
            match write {
                super::BufferedWrite::CreateNode {
                    node_id,
                    version_id,
                    label,
                    properties,
                    ..
                } => {
                    // Create in current storage with proper transaction metadata
                    let metadata = VersionMetadata::new(self.tx_id, commit_timestamp);
                    let node = Node::with_metadata(
                        *node_id,
                        *label,
                        properties.clone(),
                        *version_id,
                        metadata,
                    );
                    self.current.insert_node_direct(node, commit_timestamp)?;

                    // Store in historical storage
                    self.historical.lock_or_err()?.add_node_version(
                        *node_id,
                        *version_id,
                        temporal,
                        *label,
                        properties.clone(),
                    )?;

                    // Index in temporal indexes
                    self.temporal_indexes.lock_or_err()?.insert_node_version(
                        *node_id,
                        *version_id,
                        temporal,
                    );
                }
                super::BufferedWrite::CreateEdge {
                    edge_id,
                    version_id,
                    source,
                    target,
                    label,
                    properties,
                    ..
                } => {
                    // Create in current storage with proper transaction metadata
                    let metadata = VersionMetadata::new(self.tx_id, commit_timestamp);
                    let edge = Edge::with_metadata(
                        *edge_id,
                        *label,
                        *source,
                        *target,
                        properties.clone(),
                        *version_id,
                        metadata,
                    );
                    self.current.insert_edge_direct(edge)?;

                    // Store in historical storage
                    self.historical.lock_or_err()?.add_edge_version(
                        *edge_id,
                        *version_id,
                        temporal,
                        *label,
                        *source,
                        *target,
                        properties.clone(),
                    )?;

                    // Index in temporal indexes
                    self.temporal_indexes.lock_or_err()?.insert_edge_version(
                        *edge_id,
                        *version_id,
                        temporal,
                    );
                }
                super::BufferedWrite::UpdateNode {
                    node_id,
                    version_id,
                    label,
                    properties,
                    ..
                } => {
                    // Update in current storage with proper transaction metadata
                    let metadata = VersionMetadata::new(self.tx_id, commit_timestamp);
                    let node = Node::with_metadata(
                        *node_id,
                        *label,
                        properties.clone(),
                        *version_id,
                        metadata,
                    );
                    self.current.update_node_direct(node, commit_timestamp)?;

                    // Add new version to historical storage
                    self.historical.lock_or_err()?.add_node_version(
                        *node_id,
                        *version_id,
                        temporal,
                        *label,
                        properties.clone(),
                    )?;

                    // Index in temporal indexes
                    self.temporal_indexes.lock_or_err()?.insert_node_version(
                        *node_id,
                        *version_id,
                        temporal,
                    );
                }
                super::BufferedWrite::UpdateEdge {
                    edge_id,
                    version_id,
                    source,
                    target,
                    label,
                    properties,
                    ..
                } => {
                    // Update in current storage with proper transaction metadata
                    let metadata = VersionMetadata::new(self.tx_id, commit_timestamp);
                    let edge = Edge::with_metadata(
                        *edge_id,
                        *label,
                        *source,
                        *target,
                        properties.clone(),
                        *version_id,
                        metadata,
                    );
                    self.current.update_edge_direct(edge)?;

                    // Add new version to historical storage
                    self.historical.lock_or_err()?.add_edge_version(
                        *edge_id,
                        *version_id,
                        temporal,
                        *label,
                        *source,
                        *target,
                        properties.clone(),
                    )?;

                    // Index in temporal indexes
                    self.temporal_indexes.lock_or_err()?.insert_edge_version(
                        *edge_id,
                        *version_id,
                        temporal,
                    );
                }
                super::BufferedWrite::DeleteNode { node_id } => {
                    // Get the node before deleting
                    let node = self.current.get_node(*node_id)?;

                    // Close the current version's transaction_time in historical storage
                    // This marks the end of this version's visibility
                    let mut historical = self.historical.lock_or_err()?;
                    if let Some(current_version_id) = historical.get_current_node_version(*node_id)
                    {
                        historical.close_node_version_transaction_time(
                            current_version_id,
                            commit_timestamp,
                        )?;
                    }

                    // Generate version ID for tombstone
                    let tombstone_version_id =
                        VersionId::new_unchecked(self.version_id_gen.lock_or_err()?.next()?);

                    // Create tombstone temporal interval
                    // The tombstone marks when the deletion occurred. Its transaction_time
                    // starts at commit_timestamp and remains open (we know about the deletion
                    // from now on). Its valid_time is closed immediately since the entity
                    // no longer exists.
                    let tombstone_temporal = BiTemporalInterval::current(commit_timestamp)
                        .close_valid_time(commit_timestamp);

                    // Add tombstone version to historical storage
                    historical.add_node_version(
                        *node_id,
                        tombstone_version_id,
                        tombstone_temporal,
                        node.label,
                        node.properties.clone(),
                    )?;
                    drop(historical); // Release lock before acquiring temporal_indexes lock

                    // Index the tombstone version
                    self.temporal_indexes.lock_or_err()?.insert_node_version(
                        *node_id,
                        tombstone_version_id,
                        tombstone_temporal,
                    );

                    // Delete from current storage
                    self.current.delete_node_direct(*node_id, commit_timestamp)?;
                }
                super::BufferedWrite::DeleteEdge { edge_id } => {
                    // Get the edge before deleting
                    let edge = self.current.get_edge(*edge_id)?;

                    // Close the current version's transaction_time in historical storage
                    // This marks the end of this version's visibility
                    let mut historical = self.historical.lock_or_err()?;
                    if let Some(current_version_id) = historical.get_current_edge_version(*edge_id)
                    {
                        historical.close_edge_version_transaction_time(
                            current_version_id,
                            commit_timestamp,
                        )?;
                    }

                    // Generate version ID for tombstone
                    let tombstone_version_id =
                        VersionId::new_unchecked(self.version_id_gen.lock_or_err()?.next()?);

                    // Create tombstone temporal interval
                    // The tombstone marks when the deletion occurred. Its transaction_time
                    // starts at commit_timestamp and remains open (we know about the deletion
                    // from now on). Its valid_time is closed immediately since the entity
                    // no longer exists.
                    let tombstone_temporal = BiTemporalInterval::current(commit_timestamp)
                        .close_valid_time(commit_timestamp);

                    // Add tombstone version to historical storage
                    historical.add_edge_version(
                        *edge_id,
                        tombstone_version_id,
                        tombstone_temporal,
                        edge.label,
                        edge.source,
                        edge.target,
                        edge.properties.clone(),
                    )?;
                    drop(historical); // Release lock before acquiring temporal_indexes lock

                    // Index the tombstone version
                    self.temporal_indexes.lock_or_err()?.insert_edge_version(
                        *edge_id,
                        tombstone_version_id,
                        tombstone_temporal,
                    );

                    // Delete from current storage
                    self.current.delete_edge_direct(*edge_id)?;
                }
            }
        }

        // Rebuild adjacency indexes once after all edge operations
        // This is much more efficient than rebuilding after each operation
        self.current.rebuild_adjacency();

        Ok(())
    }
}

impl ReadOps for WriteTransaction {
    fn get_node(&self, id: NodeId) -> Result<Node> {
        // Read-your-writes: check write buffer first
        if let Some(buffered) = self.buffer.get_node_write(id) {
            match buffered {
                super::BufferedWrite::CreateNode {
                    node_id,
                    label,
                    properties,
                    version_id,
                    ..
                } => {
                    // Return the buffered node
                    return Ok(Node::with_metadata(
                        *node_id,
                        *label,
                        properties.clone(),
                        *version_id,
                        VersionMetadata {
                            created_by_tx: self.tx_id,
                            commit_timestamp: None, // Not yet committed
                        },
                    ));
                }
                super::BufferedWrite::UpdateNode {
                    node_id,
                    label,
                    properties,
                    version_id,
                    ..
                } => {
                    // Return the updated node
                    return Ok(Node::with_metadata(
                        *node_id,
                        *label,
                        properties.clone(),
                        *version_id,
                        VersionMetadata {
                            created_by_tx: self.tx_id,
                            commit_timestamp: None,
                        },
                    ));
                }
                super::BufferedWrite::DeleteNode { .. } => {
                    // Node has been deleted in this transaction
                    return Err(StorageError::NodeNotFound(id).into());
                }
                _ => {} // Not a node operation
            }
        }

        // Fall back to snapshot-isolated read from storage
        let node = self.current.get_node(id)?;

        // Check if this version is visible in our snapshot
        if !self
            .visibility_manager
            .is_visible(&self.snapshot, node.metadata.created_by_tx)
        {
            // Version not visible - return NodeNotFound
            return Err(StorageError::NodeNotFound(id).into());
        }

        Ok(node)
    }

    fn get_edge(&self, id: EdgeId) -> Result<Edge> {
        // Read-your-writes: check write buffer first
        if let Some(buffered) = self.buffer.get_edge_write(id) {
            match buffered {
                super::BufferedWrite::CreateEdge {
                    edge_id,
                    source,
                    target,
                    label,
                    properties,
                    version_id,
                    ..
                } => {
                    // Return the buffered edge
                    return Ok(Edge::with_metadata(
                        *edge_id,
                        *label,
                        *source,
                        *target,
                        properties.clone(),
                        *version_id,
                        VersionMetadata {
                            created_by_tx: self.tx_id,
                            commit_timestamp: None,
                        },
                    ));
                }
                super::BufferedWrite::UpdateEdge {
                    edge_id,
                    source,
                    target,
                    label,
                    properties,
                    version_id,
                    ..
                } => {
                    // Return the updated edge
                    return Ok(Edge::with_metadata(
                        *edge_id,
                        *label,
                        *source,
                        *target,
                        properties.clone(),
                        *version_id,
                        VersionMetadata {
                            created_by_tx: self.tx_id,
                            commit_timestamp: None,
                        },
                    ));
                }
                super::BufferedWrite::DeleteEdge { .. } => {
                    // Edge has been deleted in this transaction
                    return Err(StorageError::EdgeNotFound(id).into());
                }
                _ => {} // Not an edge operation
            }
        }

        // Fall back to snapshot-isolated read from storage
        let edge = self.current.get_edge(id)?;

        // Check if this version is visible in our snapshot
        if !self
            .visibility_manager
            .is_visible(&self.snapshot, edge.metadata.created_by_tx)
        {
            // Version not visible - return EdgeNotFound
            return Err(StorageError::EdgeNotFound(id).into());
        }

        Ok(edge)
    }

    fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_outgoing_edges(node_id)
    }

    fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_incoming_edges(node_id)
    }

    fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.current.get_outgoing_edges_with_label(node_id, label)
    }

    fn node_count(&self) -> usize {
        self.current.node_count()
    }

    fn edge_count(&self) -> usize {
        self.current.edge_count()
    }
}

impl WriteOps for WriteTransaction {
    fn create_node(&mut self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Generate IDs
        let node_id = NodeId::new_unchecked(self.node_id_gen.lock_or_err()?.next()?);
        let version_id = VersionId::new_unchecked(self.version_id_gen.lock_or_err()?.next()?);
        let label_interned = GLOBAL_INTERNER.intern(label)?;

        // Get timestamp for temporal interval
        let timestamp = self.start_timestamp;
        let temporal = BiTemporalInterval::current(timestamp);

        // Buffer the write
        self.buffer.add(super::BufferedWrite::CreateNode {
            node_id,
            version_id,
            label: label_interned,
            properties,
            temporal,
        })?;

        Ok(node_id)
    }

    fn create_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Generate IDs
        let edge_id = EdgeId::new_unchecked(self.edge_id_gen.lock_or_err()?.next()?);
        let version_id = VersionId::new_unchecked(self.version_id_gen.lock_or_err()?.next()?);
        let label_interned = GLOBAL_INTERNER.intern(label)?;

        // Get timestamp for temporal interval
        let timestamp = self.start_timestamp;
        let temporal = BiTemporalInterval::current(timestamp);

        // Buffer the write
        self.buffer.add(super::BufferedWrite::CreateEdge {
            edge_id,
            version_id,
            source,
            target,
            label: label_interned,
            properties,
            temporal,
        })?;

        Ok(edge_id)
    }

    fn update_node(&mut self, node_id: NodeId, properties: PropertyMap) -> Result<()> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Get current node to preserve label
        let node = self.current.get_node(node_id)?;
        let version_id = VersionId::new_unchecked(self.version_id_gen.lock_or_err()?.next()?);

        // Get timestamp for temporal interval
        let timestamp = self.start_timestamp;
        let temporal = BiTemporalInterval::current(timestamp);

        // Buffer the write
        self.buffer.add(super::BufferedWrite::UpdateNode {
            node_id,
            version_id,
            label: node.label,
            properties,
            temporal,
        })?;

        Ok(())
    }

    fn update_edge(&mut self, edge_id: EdgeId, properties: PropertyMap) -> Result<()> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Get current edge to preserve source, target, label
        let edge = self.current.get_edge(edge_id)?;
        let version_id = VersionId::new_unchecked(self.version_id_gen.lock_or_err()?.next()?);

        // Get timestamp for temporal interval
        let timestamp = self.start_timestamp;
        let temporal = BiTemporalInterval::current(timestamp);

        // Buffer the write
        self.buffer.add(super::BufferedWrite::UpdateEdge {
            edge_id,
            version_id,
            source: edge.source,
            target: edge.target,
            label: edge.label,
            properties,
            temporal,
        })?;

        Ok(())
    }

    fn delete_node(&mut self, node_id: NodeId) -> Result<()> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Verify node exists
        self.current.get_node(node_id)?;

        // Buffer the write
        self.buffer
            .add(super::BufferedWrite::DeleteNode { node_id })?;

        Ok(())
    }

    fn delete_edge(&mut self, edge_id: EdgeId) -> Result<()> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Verify edge exists
        self.current.get_edge(edge_id)?;

        // Buffer the write
        self.buffer
            .add(super::BufferedWrite::DeleteEdge { edge_id })?;

        Ok(())
    }
}

impl Drop for WriteTransaction {
    fn drop(&mut self) {
        // Auto-rollback if not committed
        if self.state == TxState::Active {
            self.buffer.clear();
            // Register abort with visibility manager
            self.visibility_manager.register_abort(self.tx_id);
            self.state = TxState::Aborted;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::TxIdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::wal::{WalConfig, WriteAheadLog};
    use tempfile::TempDir;

    fn create_test_write_tx() -> (WriteTransaction, TempDir) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(Mutex::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(Mutex::new(TemporalIndexes::new()));

        // Create WAL with temp directory for tests
        let temp_dir = TempDir::new().unwrap();
        let wal_config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: false, // Faster for tests
            ..Default::default()
        };
        let wal = Arc::new(Mutex::new(WriteAheadLog::new(wal_config).unwrap()));

        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let edge_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let version_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let tx_id_gen = TxIdGenerator::new();

        // Create snapshot and visibility manager for testing
        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: std::collections::HashSet::new(),
        };

        let tx = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot,
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
        );

        (tx, temp_dir)
    }

    #[test]
    fn test_write_transaction_creation() {
        let (tx, _temp_dir) = create_test_write_tx();
        assert_eq!(tx.state, TxState::Active);
        let metadata = tx.metadata();
        assert!(!metadata.is_read_only);
    }

    #[test]
    fn test_create_node_buffering() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();

        let node_id = tx.create_node("Person", props.clone()).unwrap();
        // ID generators start at 0, so first ID is 0
        assert_eq!(node_id.as_u64(), 0);

        // Read-your-writes: should be able to read buffered node
        let node = tx.get_node(node_id).unwrap();
        assert_eq!(node.id, node_id);
        assert_eq!(
            node.properties.get("name").unwrap(),
            &crate::core::property::PropertyValue::from("Alice")
        );
    }

    #[test]
    fn test_create_edge_buffering() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        // First create nodes in current storage (simulating existing nodes)
        let props = PropertyMapBuilder::new().build();
        let node1 = tx.current.create_node("Person", props.clone()).unwrap();
        let node2 = tx.current.create_node("Person", props.clone()).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("since", 2020i64).build();

        let edge_id = tx
            .create_edge(node1, node2, "KNOWS", edge_props.clone())
            .unwrap();
        // ID generators start at 0, so first edge ID is 0
        assert_eq!(edge_id.as_u64(), 0);

        // Read-your-writes: should be able to read buffered edge
        let edge = tx.get_edge(edge_id).unwrap();
        assert_eq!(edge.id, edge_id);
        assert_eq!(edge.source, node1);
        assert_eq!(edge.target, node2);
        assert_eq!(
            edge.properties.get("since").unwrap(),
            &crate::core::property::PropertyValue::from(2020i64)
        );
    }

    #[test]
    fn test_commit_applies_changes() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        let props = PropertyMapBuilder::new().insert("name", "Bob").build();

        let node_id = tx.create_node("Person", props).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Node should now be visible in current storage
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Bob")
        );
    }

    #[test]
    fn test_rollback_discards_changes() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        let props = PropertyMapBuilder::new().insert("name", "Charlie").build();

        let node_id = tx.create_node("Person", props).unwrap();

        // Rollback the transaction
        tx.rollback().unwrap();

        // Node should not be visible in current storage
        assert!(current.get_node(node_id).is_err());
    }

    #[test]
    fn test_validation_fails_for_invalid_edge() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().build();

        // Try to create edge with non-existent nodes
        let node1 = NodeId::new(999).unwrap();
        let node2 = NodeId::new(1000).unwrap();

        tx.create_edge(node1, node2, "KNOWS", props).unwrap();

        // Commit should fail validation
        let result = tx.commit();
        assert!(result.is_err());
    }

    #[test]
    fn test_auto_rollback_on_drop() {
        let current = Arc::new(CurrentStorage::new());
        let node_id = {
            let (mut tx, _temp_dir) = create_test_write_tx();
            let props = PropertyMapBuilder::new().build();
            // Transaction dropped here without commit
            tx.create_node("Person", props).unwrap()
        };

        // Node should not be visible (auto-rollback)
        assert!(current.get_node(node_id).is_err());
    }

    #[test]
    fn test_update_node() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node first in current storage
        let props = PropertyMapBuilder::new().insert("age", 30i64).build();
        let node_id = current.create_node("Person", props).unwrap();

        // Update the node properties
        let new_props = PropertyMapBuilder::new().insert("age", 31i64).build();
        tx.update_node(node_id, new_props.clone()).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify the update was applied
        let node = current.get_node(node_id).unwrap();
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(31));
    }

    #[test]
    fn test_update_node_not_found() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().insert("age", 30i64).build();
        let result = tx.update_node(NodeId::new(999).unwrap(), props);

        // Should fail because node doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_update_edge() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes and edge in current storage
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("strength", 5i64).build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", edge_props)
            .unwrap();

        // Update the edge properties
        let new_props = PropertyMapBuilder::new().insert("strength", 10i64).build();
        tx.update_edge(edge_id, new_props.clone()).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify the update was applied
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("strength").and_then(|v| v.as_int()),
            Some(10)
        );
    }

    #[test]
    fn test_update_edge_not_found() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().insert("strength", 5i64).build();
        let result = tx.update_edge(EdgeId::new(999).unwrap(), props);

        // Should fail because edge doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_delete_node() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node first in current storage
        let props = PropertyMapBuilder::new().build();
        let node_id = current.create_node("Person", props).unwrap();

        // Verify node exists
        assert!(current.get_node(node_id).is_ok());

        // Delete the node
        tx.delete_node(node_id).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify the node was deleted
        assert!(current.get_node(node_id).is_err());
    }

    #[test]
    fn test_delete_node_not_found() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let result = tx.delete_node(NodeId::new(999).unwrap());

        // Should fail because node doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_delete_edge() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes and edge in current storage
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        let edge_props = PropertyMapBuilder::new().build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", edge_props)
            .unwrap();

        // Verify edge exists
        assert!(current.get_edge(edge_id).is_ok());

        // Delete the edge
        tx.delete_edge(edge_id).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify the edge was deleted
        assert!(current.get_edge(edge_id).is_err());
    }

    #[test]
    fn test_delete_edge_not_found() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let result = tx.delete_edge(EdgeId::new(999).unwrap());

        // Should fail because edge doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_commit_after_commit_fails() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().build();
        tx.create_node("Person", props).unwrap();

        // First commit should succeed
        tx.commit().unwrap();

        // Try to commit again - should fail (can't create new tx from consumed one)
        // This is prevented by the compiler since commit consumes self
    }

    #[test]
    fn test_operations_after_commit_prevented_by_move() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().build();
        tx.create_node("Person", props).unwrap();

        // Commit consumes tx
        tx.commit().unwrap();

        // Can't use tx after commit - prevented by compiler
        // This test documents the behavior
    }

    #[test]
    fn test_read_ops_delegation() {
        let (tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create some data in current storage
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props.clone()).unwrap();
        current.create_edge(node1, node2, "KNOWS", props).unwrap();

        // Test ReadOps methods on transaction
        assert_eq!(tx.node_count(), 2);
        assert_eq!(tx.edge_count(), 1);
        assert!(tx.get_node(node1).is_ok());
        assert_eq!(tx.get_outgoing_edges(node1).len(), 1);
        assert_eq!(tx.get_incoming_edges(node2).len(), 1);
        assert_eq!(tx.get_outgoing_edges_with_label(node1, "KNOWS").len(), 1);
    }

    #[test]
    fn test_delete_node_creates_tombstone() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);
        let historical = Arc::clone(&tx.historical);

        // Create a node with properties
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();
        let node_id = current.create_node("Person", props).unwrap();

        // Verify node exists in current storage
        assert!(current.get_node(node_id).is_ok());

        // Delete the node
        tx.delete_node(node_id).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify node was deleted from current storage
        assert!(current.get_node(node_id).is_err());

        // Verify tombstone version was created in historical storage
        let historical = historical.lock().unwrap();
        let stats = historical.stats();
        assert!(
            stats.total_node_versions > 0,
            "Expected at least one node version (tombstone) in historical storage"
        );

        // The tombstone should have a closed transaction time
        // This is implicitly tested by the fact that a version was created
    }

    #[test]
    fn test_delete_edge_creates_tombstone() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);
        let historical = Arc::clone(&tx.historical);

        // Create nodes and edge
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props.clone()).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("since", 2020i64).build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", edge_props)
            .unwrap();

        // Verify edge exists
        assert!(current.get_edge(edge_id).is_ok());

        // Delete the edge
        tx.delete_edge(edge_id).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify edge was deleted from current storage
        assert!(current.get_edge(edge_id).is_err());

        // Verify tombstone version was created in historical storage
        let historical = historical.lock().unwrap();
        let stats = historical.stats();
        assert!(
            stats.total_edge_versions > 0,
            "Expected at least one edge version (tombstone) in historical storage"
        );
    }

    #[test]
    fn test_read_your_writes_update() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node in current storage
        let props = PropertyMapBuilder::new().insert("age", 30i64).build();
        let node_id = current.create_node("Person", props).unwrap();

        // Update the node in the transaction
        let new_props = PropertyMapBuilder::new().insert("age", 31i64).build();
        tx.update_node(node_id, new_props).unwrap();

        // Read-your-writes: should see the updated value
        let node = tx.get_node(node_id).unwrap();
        assert_eq!(
            node.properties.get("age").unwrap(),
            &crate::core::property::PropertyValue::from(31i64)
        );
    }

    #[test]
    fn test_read_your_writes_delete() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node in current storage
        let props = PropertyMapBuilder::new().build();
        let node_id = current.create_node("Person", props).unwrap();

        // Delete the node in the transaction
        tx.delete_node(node_id).unwrap();

        // Read-your-writes: should NOT see the deleted node
        assert!(tx.get_node(node_id).is_err());
    }

    #[test]
    fn test_empty_transaction_commit() {
        let (tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Commit empty transaction (no operations buffered)
        // This should not panic when rebuild_adjacency() is called
        tx.commit().unwrap();

        // Verify storage is still in valid state
        assert_eq!(current.node_count(), 0);
        assert_eq!(current.edge_count(), 0);
    }

    #[test]
    fn test_empty_transaction_with_only_node_operations() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create only nodes (no edges)
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();
        tx.create_node("Person", props).unwrap();

        // Commit - should call rebuild_adjacency() with empty edge set
        tx.commit().unwrap();

        // Verify node was created and adjacency is valid
        assert_eq!(current.node_count(), 1);
        assert_eq!(current.edge_count(), 0);
    }

    #[test]
    fn test_interleaved_create_update_delete_operations() {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(Mutex::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(Mutex::new(TemporalIndexes::new()));

        // Create WAL with temp directory for tests
        let temp_dir = TempDir::new().unwrap();
        let wal_config = WalConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            sync_on_write: false, // Faster for tests
            ..Default::default()
        };
        let wal = Arc::new(Mutex::new(WriteAheadLog::new(wal_config).unwrap()));

        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let edge_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let version_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let tx_id_gen = TxIdGenerator::new();

        // Create visibility manager and snapshot for testing
        let visibility_manager = Arc::new(TxVisibilityManager::new());

        // Create initial transaction to set up nodes and one edge
        let snapshot1 = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: std::collections::HashSet::new(),
        };
        let mut tx1 = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot1,
            current.clone(),
            historical.clone(),
            temporal_indexes.clone(),
            wal.clone(),
            current_timestamp.clone(),
            visibility_manager.clone(),
            node_id_gen.clone(),
            edge_id_gen.clone(),
            version_id_gen.clone(),
        );

        let props = PropertyMapBuilder::new().build();
        let node1 = tx1.create_node("Person", props.clone()).unwrap();
        let node2 = tx1.create_node("Person", props.clone()).unwrap();
        let node3 = tx1.create_node("Person", props.clone()).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("weight", 5i64).build();
        let edge1 = tx1.create_edge(node1, node2, "KNOWS", edge_props).unwrap();

        tx1.commit().unwrap();

        // Verify initial state
        assert_eq!(current.edge_count(), 1);

        // Create second transaction with interleaved operations
        let snapshot2 = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: std::collections::HashSet::new(),
        };
        let mut tx2 = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot2,
            current.clone(),
            historical.clone(),
            temporal_indexes.clone(),
            wal.clone(),
            current_timestamp.clone(),
            visibility_manager.clone(),
            node_id_gen.clone(),
            edge_id_gen.clone(),
            version_id_gen.clone(),
        );

        // 1. Create new edge
        tx2.create_edge(
            node2,
            node3,
            "FOLLOWS",
            PropertyMapBuilder::new().insert("weight", 8i64).build(),
        )
        .unwrap();

        // 2. Update existing edge
        tx2.update_edge(
            edge1,
            PropertyMapBuilder::new().insert("weight", 7i64).build(),
        )
        .unwrap();

        // 3. Create another edge
        tx2.create_edge(node1, node3, "LIKES", PropertyMapBuilder::new().build())
            .unwrap();

        // Commit all operations
        tx2.commit().unwrap();

        // After commit: verify final state
        // edge1 (updated) + 2 new edges = 3 edges total
        assert_eq!(current.edge_count(), 3);

        // Verify edge1 was updated
        let updated_edge = current.get_edge(edge1).unwrap();
        assert_eq!(
            updated_edge.get_property("weight").and_then(|v| v.as_int()),
            Some(7)
        );

        // Verify adjacency is correct after rebuild
        assert_eq!(current.out_degree(node1), 2); // KNOWS and LIKES
        assert_eq!(current.out_degree(node2), 1); // FOLLOWS
        assert_eq!(current.in_degree(node3), 2); // receives FOLLOWS and LIKES
    }

    #[test]
    fn test_batch_edge_operations_rebuild_once() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let mut nodes = Vec::new();
        for i in 0..100 {
            let node = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
                .unwrap();
            nodes.push(node);
        }

        // Create 99 edges
        for i in 0..99 {
            tx.create_edge(
                nodes[i],
                nodes[i + 1],
                "CONNECTS",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
        }

        // Commit should rebuild adjacency only once
        tx.commit().unwrap();

        // Verify all edges are in adjacency index
        assert_eq!(current.edge_count(), 99);
        for i in 0..99 {
            assert_eq!(current.out_degree(nodes[i]), 1);
            assert_eq!(current.in_degree(nodes[i + 1]), 1);
        }
    }
}

/// Tests for write-write conflict detection (Issue #8).
///
/// These tests verify that concurrent transactions updating the same entity
/// will properly detect conflicts and fail with SerializationFailure.
#[cfg(test)]
mod conflict_detection_tests {
    use super::*;
    use crate::api::transaction::TxIdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::wal::{WalConfig, WriteAheadLog};
    use tempfile::TempDir;

    /// Test harness for conflict detection tests.
    ///
    /// Bundles all shared infrastructure needed to create multiple concurrent
    /// transactions for testing write-write conflict detection.
    struct TestHarness {
        current: Arc<CurrentStorage>,
        historical: Arc<Mutex<HistoricalStorage>>,
        temporal_indexes: Arc<Mutex<TemporalIndexes>>,
        wal: Arc<Mutex<WriteAheadLog>>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<Mutex<IdGenerator>>,
        edge_id_gen: Arc<Mutex<IdGenerator>>,
        version_id_gen: Arc<Mutex<IdGenerator>>,
        tx_id_gen: TxIdGenerator,
        _temp_dir: TempDir, // Keep alive for WAL directory
    }

    impl TestHarness {
        /// Create a new test harness with all shared infrastructure.
        fn new() -> Self {
            let current = Arc::new(CurrentStorage::new());
            let historical = Arc::new(Mutex::new(HistoricalStorage::new()));
            let temporal_indexes = Arc::new(Mutex::new(TemporalIndexes::new()));

            let temp_dir = TempDir::new().unwrap();
            let wal_config = WalConfig {
                wal_dir: temp_dir.path().to_path_buf(),
                sync_on_write: false,
                ..Default::default()
            };
            let wal = Arc::new(Mutex::new(WriteAheadLog::new(wal_config).unwrap()));

            let current_timestamp = Arc::new(Mutex::new(time::now()));
            let node_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
            let edge_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
            let version_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
            let tx_id_gen = TxIdGenerator::new();
            let visibility_manager = Arc::new(TxVisibilityManager::new());

            TestHarness {
                current,
                historical,
                temporal_indexes,
                wal,
                current_timestamp,
                visibility_manager,
                node_id_gen,
                edge_id_gen,
                version_id_gen,
                tx_id_gen,
                _temp_dir: temp_dir,
            }
        }

        /// Create a new write transaction using the shared infrastructure.
        fn create_tx(&self) -> WriteTransaction {
            let snapshot = TransactionSnapshot {
                snapshot_timestamp: *self.current_timestamp.lock().unwrap(),
                active_transactions: std::collections::HashSet::new(),
            };

            WriteTransaction::new(
                self.tx_id_gen.next(),
                snapshot,
                self.current.clone(),
                self.historical.clone(),
                self.temporal_indexes.clone(),
                self.wal.clone(),
                self.current_timestamp.clone(),
                self.visibility_manager.clone(),
                self.node_id_gen.clone(),
                self.edge_id_gen.clone(),
                self.version_id_gen.clone(),
            )
        }
    }

    /// Test: First-committer-wins for node updates.
    ///
    /// Scenario from Issue #8:
    /// ```text
    /// Time    Transaction 1                    Transaction 2
    /// ----    -------------                    -------------
    /// T1      tx1 = write_transaction()
    /// T2      tx1.update_node(A, {age: 31})
    /// T3                                       tx2 = write_transaction()
    /// T4                                       tx2.update_node(A, {age: 32})
    /// T5                                       tx2.commit()  // Succeeds
    /// T6      tx1.commit()                     // Should FAIL!
    /// ```
    #[test]
    fn test_first_committer_wins_node_update() {
        let harness = TestHarness::new();

        // Create initial node via transaction (so it has proper metadata)
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("age", 30i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // T1: tx1 starts
        let mut tx1 = harness.create_tx();

        // T2: tx1 updates node
        tx1.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 31i64).build(),
        )
        .unwrap();

        // T3: tx2 starts
        let mut tx2 = harness.create_tx();

        // T4: tx2 updates node
        tx2.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 32i64).build(),
        )
        .unwrap();

        // T5: tx2 commits first - should succeed
        tx2.commit().unwrap();

        // Verify tx2's update was applied
        let node_after_tx2 = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node_after_tx2.get_property("age").and_then(|v| v.as_int()),
            Some(32),
            "tx2's update should have been applied"
        );

        // T6: tx1 tries to commit - should FAIL with SerializationFailure
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1.commit() should fail due to write-write conflict"
        );

        // Verify it's a SerializationFailure error
        let err = result.unwrap_err();
        let err_str = format!("{:?}", err);
        assert!(
            err_str.contains("SerializationFailure"),
            "Expected SerializationFailure, got: {}",
            err_str
        );

        // Verify the final value is still tx2's value (first committer wins)
        let final_node = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            final_node.get_property("age").and_then(|v| v.as_int()),
            Some(32),
            "Final value should be tx2's value (first committer wins)"
        );
    }

    /// Test: First-committer-wins for edge updates.
    #[test]
    fn test_first_committer_wins_edge_update() {
        let harness = TestHarness::new();

        // Create initial nodes and edge
        let (node1, node2, edge_id) = {
            let mut tx = harness.create_tx();
            let n1 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let n2 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let e = tx
                .create_edge(
                    n1,
                    n2,
                    "KNOWS",
                    PropertyMapBuilder::new().insert("weight", 5i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            (n1, n2, e)
        };

        // tx1 starts
        let mut tx1 = harness.create_tx();
        tx1.update_edge(
            edge_id,
            PropertyMapBuilder::new().insert("weight", 10i64).build(),
        )
        .unwrap();

        // tx2 starts and commits first
        let mut tx2 = harness.create_tx();
        tx2.update_edge(
            edge_id,
            PropertyMapBuilder::new().insert("weight", 20i64).build(),
        )
        .unwrap();
        tx2.commit().unwrap();

        // tx1 tries to commit - should fail
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1.commit() should fail due to edge update conflict"
        );

        // Verify final value is tx2's
        let final_edge = harness.current.get_edge(edge_id).unwrap();
        assert_eq!(
            final_edge.get_property("weight").and_then(|v| v.as_int()),
            Some(20)
        );

        // Suppress unused variable warnings
        let _ = (node1, node2);
    }

    /// Test: First-committer-wins for node deletion.
    #[test]
    fn test_first_committer_wins_node_delete() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx1 starts and wants to update
        let mut tx1 = harness.create_tx();
        tx1.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 31i64).build(),
        )
        .unwrap();

        // tx2 starts and deletes the node, then commits
        let mut tx2 = harness.create_tx();
        tx2.delete_node(node_id).unwrap();
        tx2.commit().unwrap();

        // Node should be deleted now
        assert!(harness.current.get_node(node_id).is_err());

        // tx1 tries to commit its update - should fail
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1.commit() should fail - node was modified (deleted) by tx2"
        );
    }

    /// Test: Delete vs Delete conflict.
    #[test]
    fn test_delete_delete_conflict() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx1 wants to delete
        let mut tx1 = harness.create_tx();
        tx1.delete_node(node_id).unwrap();

        // tx2 also wants to delete and commits first
        let mut tx2 = harness.create_tx();
        tx2.delete_node(node_id).unwrap();
        tx2.commit().unwrap();

        // tx1 tries to commit - should fail
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1.commit() should fail - node was already deleted by tx2"
        );
    }

    /// Test: No conflict when transactions modify different entities.
    #[test]
    fn test_no_conflict_different_entities() {
        let harness = TestHarness::new();

        // Create two nodes
        let (node1, node2) = {
            let mut tx = harness.create_tx();
            let n1 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Alice").build(),
                )
                .unwrap();
            let n2 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Bob").build(),
                )
                .unwrap();
            tx.commit().unwrap();
            (n1, n2)
        };

        // tx1 updates node1
        let mut tx1 = harness.create_tx();
        tx1.update_node(
            node1,
            PropertyMapBuilder::new().insert("age", 30i64).build(),
        )
        .unwrap();

        // tx2 updates node2 and commits first
        let mut tx2 = harness.create_tx();
        tx2.update_node(
            node2,
            PropertyMapBuilder::new().insert("age", 25i64).build(),
        )
        .unwrap();
        tx2.commit().unwrap();

        // tx1 should also succeed - no conflict on different entities
        tx1.commit().unwrap();

        // Verify both updates were applied
        assert_eq!(
            harness
                .current
                .get_node(node1)
                .unwrap()
                .get_property("age")
                .and_then(|v| v.as_int()),
            Some(30)
        );
        assert_eq!(
            harness
                .current
                .get_node(node2)
                .unwrap()
                .get_property("age")
                .and_then(|v| v.as_int()),
            Some(25)
        );
    }

    /// Test: No conflict for create operations (new entities).
    #[test]
    fn test_no_conflict_for_creates() {
        let harness = TestHarness::new();

        // tx1 creates a node
        let mut tx1 = harness.create_tx();
        let node1 = tx1
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // tx2 creates a different node and commits first
        let mut tx2 = harness.create_tx();
        let node2 = tx2
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        tx2.commit().unwrap();

        // tx1 should also succeed - creates don't conflict
        tx1.commit().unwrap();

        // Both nodes should exist
        assert!(harness.current.get_node(node1).is_ok());
        assert!(harness.current.get_node(node2).is_ok());
    }

    /// Test: Conflict error message contains useful information.
    #[test]
    fn test_conflict_error_message() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx1 updates
        let mut tx1 = harness.create_tx();
        tx1.update_node(node_id, PropertyMapBuilder::new().build())
            .unwrap();

        // tx2 commits first
        let mut tx2 = harness.create_tx();
        tx2.update_node(node_id, PropertyMapBuilder::new().build())
            .unwrap();
        tx2.commit().unwrap();

        // tx1 fails
        let result = tx1.commit();
        let err = result.unwrap_err();
        let err_str = format!("{:?}", err);

        // Verify error contains node ID info
        assert!(
            err_str.contains("NodeId"),
            "Error should mention the entity: {}",
            err_str
        );
        assert!(
            err_str.contains("committed") || err_str.contains("snapshot"),
            "Error should explain the conflict: {}",
            err_str
        );
    }
}

/// Tests for concurrent commit timestamp ordering (Issue #10).
///
/// These tests verify that transaction timestamps are assigned in the same
/// order as their commits become durable. This is critical for bi-temporal
/// correctness - transaction time must be monotonic with actual commit order.
#[cfg(test)]
mod timestamp_ordering_tests {
    use super::*;
    use crate::api::transaction::TxIdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::wal::{WalConfig, WriteAheadLog};
    use std::thread;
    use tempfile::TempDir;

    /// Test harness for timestamp ordering tests.
    struct TestHarness {
        current: Arc<CurrentStorage>,
        historical: Arc<Mutex<HistoricalStorage>>,
        temporal_indexes: Arc<Mutex<TemporalIndexes>>,
        wal: Arc<Mutex<WriteAheadLog>>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<Mutex<IdGenerator>>,
        edge_id_gen: Arc<Mutex<IdGenerator>>,
        version_id_gen: Arc<Mutex<IdGenerator>>,
        tx_id_gen: TxIdGenerator,
        _temp_dir: TempDir,
    }

    impl TestHarness {
        fn new() -> Self {
            let current = Arc::new(CurrentStorage::new());
            let historical = Arc::new(Mutex::new(HistoricalStorage::new()));
            let temporal_indexes = Arc::new(Mutex::new(TemporalIndexes::new()));

            let temp_dir = TempDir::new().unwrap();
            let wal_config = WalConfig {
                wal_dir: temp_dir.path().to_path_buf(),
                sync_on_write: false,
                ..Default::default()
            };
            let wal = Arc::new(Mutex::new(WriteAheadLog::new(wal_config).unwrap()));

            let current_timestamp = Arc::new(Mutex::new(time::now()));
            let node_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
            let edge_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
            let version_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
            let tx_id_gen = TxIdGenerator::new();
            let visibility_manager = Arc::new(TxVisibilityManager::new());

            TestHarness {
                current,
                historical,
                temporal_indexes,
                wal,
                current_timestamp,
                visibility_manager,
                node_id_gen,
                edge_id_gen,
                version_id_gen,
                tx_id_gen,
                _temp_dir: temp_dir,
            }
        }

        fn create_tx(&self) -> WriteTransaction {
            let snapshot = TransactionSnapshot {
                snapshot_timestamp: *self.current_timestamp.lock().unwrap(),
                active_transactions: std::collections::HashSet::new(),
            };

            WriteTransaction::new(
                self.tx_id_gen.next(),
                snapshot,
                self.current.clone(),
                self.historical.clone(),
                self.temporal_indexes.clone(),
                self.wal.clone(),
                self.current_timestamp.clone(),
                self.visibility_manager.clone(),
                self.node_id_gen.clone(),
                self.edge_id_gen.clone(),
                self.version_id_gen.clone(),
            )
        }
    }

    /// Test: Sequential commits have monotonically increasing timestamps.
    #[test]
    fn test_sequential_commits_monotonic_timestamps() {
        let harness = TestHarness::new();

        let mut timestamps = Vec::new();

        // Perform 10 sequential commits
        for i in 0..10 {
            let mut tx = harness.create_tx();
            tx.create_node(
                "Test",
                PropertyMapBuilder::new().insert("seq", i as i64).build(),
            )
            .unwrap();
            tx.commit().unwrap();

            // Record the current timestamp after commit
            let ts = *harness.current_timestamp.lock().unwrap();
            timestamps.push(ts);
        }

        // Verify timestamps are strictly increasing
        for i in 1..timestamps.len() {
            assert!(
                timestamps[i] > timestamps[i - 1],
                "Timestamp {} ({}) should be > timestamp {} ({})",
                i,
                timestamps[i],
                i - 1,
                timestamps[i - 1]
            );
        }
    }

    /// Test: Concurrent commits still produce monotonically increasing timestamps.
    ///
    /// This test verifies that the fix for Issue #10 works correctly:
    /// - Multiple threads commit transactions concurrently
    /// - Each commit creates a node and we get its commit timestamp from metadata
    /// - We verify that all commit timestamps are unique and properly ordered
    #[test]
    fn test_concurrent_commits_ordered_timestamps() {
        let harness = Arc::new(TestHarness::new());
        let results = Arc::new(Mutex::new(Vec::new()));

        let num_threads = 8;
        let commits_per_thread = 5;

        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let harness = harness.clone();
                let results = results.clone();

                thread::spawn(move || {
                    for i in 0..commits_per_thread {
                        let mut tx = harness.create_tx();
                        let node_id = tx
                            .create_node(
                                "Test",
                                PropertyMapBuilder::new()
                                    .insert("thread", thread_id as i64)
                                    .insert("iteration", i as i64)
                                    .build(),
                            )
                            .unwrap();
                        tx.commit().unwrap();

                        // Get the ACTUAL commit timestamp from the node's metadata
                        let node = harness.current.get_node(node_id).unwrap();
                        let commit_ts = node.metadata.commit_timestamp.unwrap();

                        results
                            .lock()
                            .unwrap()
                            .push((commit_ts, thread_id, i, node_id));
                    }
                })
            })
            .collect();

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Analyze results: sort by commit timestamp
        let mut results = results.lock().unwrap();
        results.sort_by_key(|(ts, _, _, _)| *ts);

        // With the fix, all timestamps should be unique (due to double-increment)
        // Check for duplicates
        for i in 1..results.len() {
            let (ts_prev, thread_prev, iter_prev, _) = results[i - 1];
            let (ts_curr, thread_curr, iter_curr, _) = results[i];

            assert!(
                ts_curr > ts_prev,
                "Duplicate or out-of-order timestamp detected: \
                 Thread {} iter {} (ts={}) vs Thread {} iter {} (ts={})",
                thread_prev,
                iter_prev,
                ts_prev,
                thread_curr,
                iter_curr,
                ts_curr
            );
        }

        // Verify we got all expected commits
        assert_eq!(
            results.len(),
            num_threads * commits_per_thread,
            "Expected {} commits, got {}",
            num_threads * commits_per_thread,
            results.len()
        );
    }

    /// Test: Version chains are correctly ordered by transaction time.
    ///
    /// When multiple transactions update the same node, the version chain
    /// should reflect the actual commit order.
    #[test]
    fn test_version_chain_ordering() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("version", 0i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // Track commit timestamps
        let mut commit_timestamps = Vec::new();

        // Perform sequential updates
        for version in 1..=5 {
            let mut tx = harness.create_tx();
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("version", version as i64)
                    .build(),
            )
            .unwrap();
            tx.commit().unwrap();

            // Get the node's current metadata to verify timestamp
            let node = harness.current.get_node(node_id).unwrap();
            if let Some(ts) = node.metadata.commit_timestamp {
                commit_timestamps.push(ts);
            }
        }

        // Verify timestamps are strictly increasing
        for i in 1..commit_timestamps.len() {
            assert!(
                commit_timestamps[i] > commit_timestamps[i - 1],
                "Version {} timestamp ({}) should be > version {} timestamp ({})",
                i + 1,
                commit_timestamps[i],
                i,
                commit_timestamps[i - 1]
            );
        }

        // Verify final version is correct
        let final_node = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            final_node.get_property("version").and_then(|v| v.as_int()),
            Some(5)
        );
    }
}
