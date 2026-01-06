//! Read-only transactions
//!
//! Read-only transactions are lightweight:
//! - No write buffer
//! - No WAL logging
//! - Snapshot-based reads for consistency
//! - No commit overhead

use super::{ReadOps, TransactionSnapshot, TxId, TxMetadata, TxState, TxVisibilityManager};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::temporal::time;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::version::VersionMetadata;
use crate::utils::error::{Result, StorageError};
use crate::utils::lock::RwLockExt;
use std::sync::{Arc, RwLock};

/// Read-only transaction
///
/// Read-only transactions are lightweight:
/// - No write buffer
/// - No WAL logging
/// - Snapshot-based reads for consistency
/// - No commit overhead
///
/// # Example
///
/// ```ignore
/// let tx = db.read_transaction();
/// let node = tx.get_node(node_id)?;
/// // No commit needed - transaction is read-only
/// ```
pub struct ReadTransaction {
    tx_id: TxId,
    start_timestamp: i64,
    snapshot: TransactionSnapshot,
    current: Arc<CurrentStorage>,
    visibility_manager: Arc<TxVisibilityManager>,
    historical: Arc<RwLock<HistoricalStorage>>,
}

impl ReadTransaction {
    /// Create a new read-only transaction
    pub(crate) fn new(
        tx_id: TxId,
        snapshot: TransactionSnapshot,
        current: Arc<CurrentStorage>,
        visibility_manager: Arc<TxVisibilityManager>,
        historical: Arc<RwLock<HistoricalStorage>>,
    ) -> Self {
        ReadTransaction {
            tx_id,
            start_timestamp: time::now(),
            snapshot,
            current,
            visibility_manager,
            historical,
        }
    }

    /// Get transaction metadata
    pub fn metadata(&self) -> TxMetadata {
        TxMetadata {
            tx_id: self.tx_id,
            start_timestamp: self.start_timestamp,
            commit_timestamp: None,
            state: TxState::Active,
            is_read_only: true,
        }
    }

    /// Get transaction ID
    pub fn tx_id(&self) -> TxId {
        self.tx_id
    }
}

impl ReadOps for ReadTransaction {
    fn get_node(&self, id: NodeId) -> Result<Node> {
        // FAST PATH: Try current storage first
        let current_node = self.current.get_node(id)?;

        // Check if current version is visible in our snapshot
        if self
            .visibility_manager
            .is_visible(&self.snapshot, current_node.metadata.created_by_tx)
        {
            return Ok(current_node);
        }

        // SLOW PATH: Current version not visible - query historical storage
        let historical = self.historical.read_or_err()?;

        // Find version visible at our snapshot timestamp
        let version_id = historical.find_node_version_at_time(
            id,
            self.snapshot.snapshot_timestamp, // valid_time
            self.snapshot.snapshot_timestamp, // transaction_time
        );

        match version_id {
            Some(vid) => {
                // Found a visible version - reconstruct it
                let version = historical
                    .get_node_version(vid)
                    .ok_or(StorageError::VersionNotFound(vid))?;

                // Reconstruct properties from anchor+delta
                let properties = historical.reconstruct_node_properties(vid)?;

                // Build Node from historical version
                // Use default metadata since visibility already validated
                Ok(Node::with_metadata(
                    id,
                    version.label,
                    properties,
                    vid,
                    VersionMetadata::default_for_existing(),
                ))
            }
            None => {
                // No version visible at snapshot time
                Err(StorageError::NodeNotFound(id).into())
            }
        }
    }

    fn get_edge(&self, id: EdgeId) -> Result<Edge> {
        // FAST PATH: Try current storage first
        let current_edge = self.current.get_edge(id)?;

        // Check if current version is visible in our snapshot
        if self
            .visibility_manager
            .is_visible(&self.snapshot, current_edge.metadata.created_by_tx)
        {
            return Ok(current_edge);
        }

        // SLOW PATH: Current version not visible - query historical storage
        let historical = self.historical.read_or_err()?;

        // Find version visible at our snapshot timestamp
        let version_id = historical.find_edge_version_at_time(
            id,
            self.snapshot.snapshot_timestamp, // valid_time
            self.snapshot.snapshot_timestamp, // transaction_time
        );

        match version_id {
            Some(vid) => {
                // Found a visible version - reconstruct it
                let version = historical
                    .get_edge_version(vid)
                    .ok_or(StorageError::VersionNotFound(vid))?;

                // Reconstruct properties from anchor+delta
                let properties = historical.reconstruct_edge_properties(vid)?;

                // Build Edge from historical version
                // Use default metadata since visibility already validated
                Ok(Edge::with_metadata(
                    id,
                    version.label,
                    version.source,
                    version.target,
                    properties,
                    vid,
                    VersionMetadata::default_for_existing(),
                ))
            }
            None => {
                // No version visible at snapshot time
                Err(StorageError::EdgeNotFound(id).into())
            }
        }
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

impl Drop for ReadTransaction {
    fn drop(&mut self) {
        // Register abort to remove from active set
        // This prevents memory leak in active transactions set
        self.visibility_manager.register_abort(self.tx_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;
    use std::collections::HashSet;
    use std::sync::Arc;

    // Helper to create a test ReadTransaction with snapshot
    fn create_test_read_tx(tx_id: TxId, current: Arc<CurrentStorage>) -> ReadTransaction {
        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(HashSet::new()),
        };
        ReadTransaction::new(tx_id, snapshot, current, visibility_manager, historical)
    }

    #[test]
    fn test_read_transaction_creation() {
        let current = Arc::new(CurrentStorage::new());
        let tx = create_test_read_tx(TxId::new(1), current);

        assert_eq!(tx.tx_id(), TxId::new(1));
        let metadata = tx.metadata();
        assert_eq!(metadata.tx_id, TxId::new(1));
        assert!(metadata.is_read_only);
        assert_eq!(metadata.state, TxState::Active);
        assert_eq!(metadata.commit_timestamp, None);
    }

    #[test]
    fn test_read_transaction_get_node() {
        let current = Arc::new(CurrentStorage::new());

        // Create a node in the storage
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();
        let node_id = current.create_node("Person", props.clone()).unwrap();

        // Read through transaction
        let tx = create_test_read_tx(TxId::new(1), Arc::clone(&current));
        let node = tx.get_node(node_id).unwrap();

        assert_eq!(node.id, node_id);
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(30));
    }

    #[test]
    fn test_read_transaction_get_node_not_found() {
        let current = Arc::new(CurrentStorage::new());
        let tx = create_test_read_tx(TxId::new(1), current);

        let result = tx.get_node(NodeId::new(999).unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn test_read_transaction_node_count() {
        let current = Arc::new(CurrentStorage::new());

        // Create some nodes
        let props = PropertyMapBuilder::new().build();
        current.create_node("Person", props.clone()).unwrap();
        current.create_node("Person", props.clone()).unwrap();
        current.create_node("Person", props).unwrap();

        let tx = create_test_read_tx(TxId::new(1), current);
        assert_eq!(tx.node_count(), 3);
    }

    #[test]
    fn test_read_transaction_get_edges() {
        let current = Arc::new(CurrentStorage::new());

        // Create nodes and edge
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props.clone()).unwrap();
        let edge_id = current.create_edge(node1, node2, "KNOWS", props).unwrap();

        let tx = create_test_read_tx(TxId::new(1), current);

        // Get edge
        let edge = tx.get_edge(edge_id).unwrap();
        assert_eq!(edge.id, edge_id);
        assert_eq!(edge.source, node1);
        assert_eq!(edge.target, node2);

        // Get outgoing edges
        let outgoing = tx.get_outgoing_edges(node1);
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0], edge_id);

        // Get incoming edges
        let incoming = tx.get_incoming_edges(node2);
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0], edge_id);
    }

    #[test]
    fn test_read_transaction_get_outgoing_edges_with_label() {
        let current = Arc::new(CurrentStorage::new());

        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props.clone()).unwrap();
        let node3 = current.create_node("Person", props.clone()).unwrap();

        // Create edges with different labels
        let edge1 = current
            .create_edge(node1, node2, "KNOWS", props.clone())
            .unwrap();
        let _edge2 = current.create_edge(node1, node3, "FOLLOWS", props).unwrap();

        let tx = create_test_read_tx(TxId::new(1), current);

        // Get only KNOWS edges
        let knows_edges = tx.get_outgoing_edges_with_label(node1, "KNOWS");
        assert_eq!(knows_edges.len(), 1);
        assert_eq!(knows_edges[0], edge1);

        // Get only FOLLOWS edges
        let follows_edges = tx.get_outgoing_edges_with_label(node1, "FOLLOWS");
        assert_eq!(follows_edges.len(), 1);
    }

    #[test]
    fn test_read_transaction_concurrent_access() {
        use std::thread;

        let current = Arc::new(CurrentStorage::new());

        // Pre-populate with data
        let props = PropertyMapBuilder::new().insert("value", 42i64).build();
        let node_id = current.create_node("Test", props).unwrap();

        // Spawn multiple reader threads
        let mut handles = vec![];
        for i in 0..10 {
            let current_clone = Arc::clone(&current);
            let handle = thread::spawn(move || {
                let tx = create_test_read_tx(TxId::new(i), current_clone);
                let node = tx.get_node(node_id).unwrap();
                assert_eq!(
                    node.get_property("value").and_then(|v| v.as_int()),
                    Some(42)
                );
            });
            handles.push(handle);
        }

        // Wait for all readers
        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_read_transaction_drop_cleanup() {
        // Test that ReadTransaction properly cleans up when dropped
        let current = Arc::new(CurrentStorage::new());
        let visibility_manager = Arc::new(TxVisibilityManager::new());

        let tx_id = TxId::new(42);

        // Register transaction as active
        visibility_manager.register_active(tx_id);
        assert_eq!(visibility_manager.active_count(), 1);

        {
            // Create read transaction
            let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
            let snapshot = TransactionSnapshot {
                snapshot_timestamp: time::now(),
                active_transactions: Arc::new(HashSet::new()),
            };
            let _tx = ReadTransaction::new(
                tx_id,
                snapshot,
                Arc::clone(&current),
                Arc::clone(&visibility_manager),
                Arc::clone(&historical),
            );

            // Transaction should still be active while in scope
            assert_eq!(visibility_manager.active_count(), 1);
        } // tx dropped here - should call register_abort

        // After drop, transaction should be removed from active set
        assert_eq!(
            visibility_manager.active_count(),
            0,
            "ReadTransaction should remove itself from active set on drop"
        );
    }
}
