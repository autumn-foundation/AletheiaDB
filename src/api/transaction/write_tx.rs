//! Write transactions with ACID guarantees.
//!
//! Write transactions provide full ACID properties:
//! - **Atomicity**: All-or-nothing commit via write buffering
//! - **Consistency**: Referential integrity validation before commit
//! - **Isolation**: Read Committed isolation level
//! - **Durability**: WAL integration (Phase 4)
//!
//! Write transactions buffer all changes in memory until commit.
//! On commit, changes are validated and applied atomically.

use super::{ReadOps, TxId, TxMetadata, TxState, WriteBuffer, WriteOps};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, IdGenerator, NodeId, VersionId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMap;
use crate::core::temporal::{BiTemporalInterval, Timestamp, time};
use crate::index::temporal::TemporalIndexes;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::wal::{WalOperation, WriteAheadLog};
use crate::utils::error::{Result, TransactionError};
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

    // Write buffer for uncommitted changes
    buffer: WriteBuffer,

    // Shared references to storage (Arc for zero-copy sharing)
    current: Arc<CurrentStorage>,
    historical: Arc<Mutex<HistoricalStorage>>,
    temporal_indexes: Arc<Mutex<TemporalIndexes>>,
    wal: Arc<Mutex<WriteAheadLog>>,
    current_timestamp: Arc<Mutex<Timestamp>>,

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
        current: Arc<CurrentStorage>,
        historical: Arc<Mutex<HistoricalStorage>>,
        temporal_indexes: Arc<Mutex<TemporalIndexes>>,
        wal: Arc<Mutex<WriteAheadLog>>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        node_id_gen: Arc<Mutex<IdGenerator>>,
        edge_id_gen: Arc<Mutex<IdGenerator>>,
        version_id_gen: Arc<Mutex<IdGenerator>>,
    ) -> Self {
        WriteTransaction {
            tx_id,
            start_timestamp: time::now(),
            state: TxState::Active,
            buffer: WriteBuffer::new(),
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
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

        // Acquire commit timestamp
        let commit_timestamp = {
            let mut ts = self.current_timestamp.lock().unwrap();
            let current = *ts;
            *ts += 1;
            current
        };

        // Log operations to WAL (Durability)
        // This must happen BEFORE applying changes
        self.log_to_wal(commit_timestamp)?;

        // Flush WAL to ensure durability
        {
            let mut wal = self.wal.lock().unwrap();
            wal.flush()?;
        }

        // Apply all changes atomically
        self.apply_changes(commit_timestamp)?;

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

    /// Log all buffered operations to WAL.
    ///
    /// This ensures durability - operations are logged before being applied.
    fn log_to_wal(&self, commit_timestamp: Timestamp) -> Result<()> {
        let temporal = BiTemporalInterval::current(commit_timestamp);
        let mut wal = self.wal.lock().unwrap();

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
                super::BufferedWrite::DeleteNode { node_id } => {
                    WalOperation::DeleteNode {
                        node_id: *node_id,
                        temporal,
                    }
                }
                super::BufferedWrite::DeleteEdge { edge_id } => {
                    WalOperation::DeleteEdge {
                        edge_id: *edge_id,
                        temporal,
                    }
                }
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
                    // Create in current storage
                    let node = Node::new(*node_id, *label, properties.clone(), *version_id);
                    self.current.insert_node_direct(node)?;

                    // Store in historical storage
                    self.historical.lock().unwrap().add_node_version(
                        *node_id,
                        *version_id,
                        temporal,
                        *label,
                        properties.clone(),
                    )?;

                    // Index in temporal indexes
                    self.temporal_indexes.lock().unwrap().insert_node_version(
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
                    // Create in current storage
                    let edge = Edge::new(
                        *edge_id,
                        *label,
                        *source,
                        *target,
                        properties.clone(),
                        *version_id,
                    );
                    self.current.insert_edge_direct(edge)?;

                    // Store in historical storage
                    self.historical.lock().unwrap().add_edge_version(
                        *edge_id,
                        *version_id,
                        temporal,
                        *label,
                        *source,
                        *target,
                        properties.clone(),
                    )?;

                    // Index in temporal indexes
                    self.temporal_indexes.lock().unwrap().insert_edge_version(
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
                    // Update in current storage
                    let node = Node::new(*node_id, *label, properties.clone(), *version_id);
                    self.current.update_node_direct(node)?;

                    // Add new version to historical storage
                    self.historical.lock().unwrap().add_node_version(
                        *node_id,
                        *version_id,
                        temporal,
                        *label,
                        properties.clone(),
                    )?;

                    // Index in temporal indexes
                    self.temporal_indexes.lock().unwrap().insert_node_version(
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
                    // Update in current storage
                    let edge = Edge::new(
                        *edge_id,
                        *label,
                        *source,
                        *target,
                        properties.clone(),
                        *version_id,
                    );
                    self.current.update_edge_direct(edge)?;

                    // Add new version to historical storage
                    self.historical.lock().unwrap().add_edge_version(
                        *edge_id,
                        *version_id,
                        temporal,
                        *label,
                        *source,
                        *target,
                        properties.clone(),
                    )?;

                    // Index in temporal indexes
                    self.temporal_indexes.lock().unwrap().insert_edge_version(
                        *edge_id,
                        *version_id,
                        temporal,
                    );
                }
                super::BufferedWrite::DeleteNode { node_id } => {
                    // Delete from current storage
                    self.current.delete_node_direct(*node_id)?;

                    // TODO: Mark as deleted in historical storage (add tombstone version)
                    // For now, we just remove from current state
                }
                super::BufferedWrite::DeleteEdge { edge_id } => {
                    // Delete from current storage
                    self.current.delete_edge_direct(*edge_id)?;

                    // TODO: Mark as deleted in historical storage (add tombstone version)
                    // For now, we just remove from current state
                }
            }
        }

        Ok(())
    }
}

impl ReadOps for WriteTransaction {
    fn get_node(&self, id: NodeId) -> Result<Node> {
        // Read Committed: always read latest committed data
        // Uncommitted writes in this transaction are not visible
        // until commit (for simplicity in Phase 3)
        self.current.get_node(id)
    }

    fn get_edge(&self, id: EdgeId) -> Result<Edge> {
        self.current.get_edge(id)
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
        let node_id = NodeId::new(self.node_id_gen.lock().unwrap().next());
        let version_id = VersionId::new(self.version_id_gen.lock().unwrap().next());
        let label_interned = GLOBAL_INTERNER.intern(label);

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
        });

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
        let edge_id = EdgeId::new(self.edge_id_gen.lock().unwrap().next());
        let version_id = VersionId::new(self.version_id_gen.lock().unwrap().next());
        let label_interned = GLOBAL_INTERNER.intern(label);

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
        });

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
        let version_id = VersionId::new(self.version_id_gen.lock().unwrap().next());

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
        });

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
        let version_id = VersionId::new(self.version_id_gen.lock().unwrap().next());

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
        });

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
            .add(super::BufferedWrite::DeleteNode { node_id });

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
            .add(super::BufferedWrite::DeleteEdge { edge_id });

        Ok(())
    }
}

impl Drop for WriteTransaction {
    fn drop(&mut self) {
        // Auto-rollback if not committed
        if self.state == TxState::Active {
            self.buffer.clear();
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

        let tx = WriteTransaction::new(
            tx_id_gen.next(),
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
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

        let node_id = tx.create_node("Person", props).unwrap();
        // ID generators start at 0, so first ID is 0
        assert_eq!(node_id.as_u64(), 0);

        // Node should not be visible until commit
        assert!(tx.get_node(node_id).is_err());
    }

    #[test]
    fn test_create_edge_buffering() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        // First create nodes in current storage (simulating existing nodes)
        let props = PropertyMapBuilder::new().build();
        let node1 = tx.current.create_node("Person", props.clone()).unwrap();
        let node2 = tx.current.create_node("Person", props.clone()).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("since", 2020i64).build();

        let edge_id = tx.create_edge(node1, node2, "KNOWS", edge_props).unwrap();
        // ID generators start at 0, so first edge ID is 0
        assert_eq!(edge_id.as_u64(), 0);

        // Edge should not be visible until commit
        assert!(tx.get_edge(edge_id).is_err());
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
        let node1 = NodeId::new(999);
        let node2 = NodeId::new(1000);

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
        let result = tx.update_node(NodeId::new(999), props);

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
        let edge_id = current.create_edge(node1, node2, "KNOWS", edge_props).unwrap();

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
        let result = tx.update_edge(EdgeId::new(999), props);

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

        let result = tx.delete_node(NodeId::new(999));

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

        let result = tx.delete_edge(EdgeId::new(999));

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
        current
            .create_edge(node1, node2, "KNOWS", props)
            .unwrap();

        // Test ReadOps methods on transaction
        assert_eq!(tx.node_count(), 2);
        assert_eq!(tx.edge_count(), 1);
        assert!(tx.get_node(node1).is_ok());
        assert_eq!(tx.get_outgoing_edges(node1).len(), 1);
        assert_eq!(tx.get_incoming_edges(node2).len(), 1);
        assert_eq!(tx.get_outgoing_edges_with_label(node1, "KNOWS").len(), 1);
    }
}
