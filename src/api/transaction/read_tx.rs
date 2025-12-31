//! Read-only transactions
//!
//! Read-only transactions are lightweight and have zero overhead:
//! - No write buffer
//! - No WAL logging
//! - Direct reads from CurrentStorage
//! - No commit overhead

use super::{ReadOps, TxId, TxMetadata, TxState};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::temporal::time;
use crate::storage::current::CurrentStorage;
use crate::utils::error::Result;
use std::sync::Arc;

/// Read-only transaction
///
/// Read-only transactions are lightweight:
/// - No write buffer
/// - No WAL logging
/// - Direct reads from CurrentStorage
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
    current: Arc<CurrentStorage>,
}

impl ReadTransaction {
    /// Create a new read-only transaction
    pub(crate) fn new(tx_id: TxId, current: Arc<CurrentStorage>) -> Self {
        ReadTransaction {
            tx_id,
            start_timestamp: time::now(),
            current,
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
        // Read Committed: always read latest committed data
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_read_transaction_creation() {
        let current = Arc::new(CurrentStorage::new());
        let tx = ReadTransaction::new(TxId::new(1), current);

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
        let tx = ReadTransaction::new(TxId::new(1), Arc::clone(&current));
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
        let tx = ReadTransaction::new(TxId::new(1), current);

        let result = tx.get_node(NodeId::new(999));
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

        let tx = ReadTransaction::new(TxId::new(1), current);
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

        let tx = ReadTransaction::new(TxId::new(1), current);

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

        let tx = ReadTransaction::new(TxId::new(1), current);

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
                let tx = ReadTransaction::new(TxId::new(i), current_clone);
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
}
