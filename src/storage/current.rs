//! Current-state storage engine.
//!
//! This module implements the "hot path" storage for the current state of the
//! graph. It provides O(1) lookups and cache-friendly traversals optimized for
//! non-temporal queries.

use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, IdGenerator, NodeId, VersionId};
use crate::core::interning::StringInterner;
use crate::core::property::PropertyMap;
use crate::index::current::CurrentIndexes;
use crate::utils::error::{Result, StorageError};

/// Statistics about current storage
#[derive(Debug, Clone)]
pub struct CurrentStats {
    /// Number of nodes
    pub node_count: usize,
    /// Number of edges
    pub edge_count: usize,
}

/// Current-state storage engine.
///
/// This storage engine maintains the current version of all nodes and edges,
/// optimized for fast queries without temporal overhead. This is the "fast path"
/// that should achieve <1µs single-hop traversals.
pub struct CurrentStorage {
    /// Indexes for nodes and edges
    indexes: CurrentIndexes,
    /// ID generator for nodes
    node_id_gen: IdGenerator,
    /// ID generator for edges
    edge_id_gen: IdGenerator,
    /// ID generator for versions
    version_id_gen: IdGenerator,
    /// String interner for labels (could use global, but keeping local for now)
    interner: StringInterner,
}

impl CurrentStorage {
    /// Create a new empty current storage.
    pub fn new() -> Self {
        CurrentStorage {
            indexes: CurrentIndexes::new(),
            node_id_gen: IdGenerator::new(),
            edge_id_gen: IdGenerator::new(),
            version_id_gen: IdGenerator::new(),
            interner: StringInterner::new(),
        }
    }

    /// Create a node with the given label and properties.
    ///
    /// Returns the ID of the newly created node.
    pub fn create_node(&mut self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        let node_id = NodeId::new(self.node_id_gen.next());
        let version_id = VersionId::new(self.version_id_gen.next());
        let label_interned = self.interner.intern(label);

        let node = Node::new(node_id, label_interned, properties, version_id);
        self.indexes.insert_node(node);

        Ok(node_id)
    }

    /// Create an edge between two nodes.
    ///
    /// Returns the ID of the newly created edge.
    pub fn create_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        // Verify nodes exist
        if !self.indexes.contains_node(source) {
            return Err(StorageError::NodeNotFound(source).into());
        }
        if !self.indexes.contains_node(target) {
            return Err(StorageError::NodeNotFound(target).into());
        }

        let edge_id = EdgeId::new(self.edge_id_gen.next());
        let version_id = VersionId::new(self.version_id_gen.next());
        let label_interned = self.interner.intern(label);

        let edge = Edge::new(
            edge_id,
            label_interned,
            source,
            target,
            properties,
            version_id,
        );
        self.indexes.insert_edge(edge);

        // Rebuild adjacency indexes
        // TODO: For better performance, batch this or use incremental updates
        self.indexes.rebuild_adjacency();

        Ok(edge_id)
    }

    /// Get a node by ID.
    pub fn get_node(&self, id: NodeId) -> Result<Node> {
        self.indexes
            .get_node(id)
            .ok_or_else(|| StorageError::NodeNotFound(id).into())
    }

    /// Get an edge by ID.
    pub fn get_edge(&self, id: EdgeId) -> Result<Edge> {
        self.indexes
            .get_edge(id)
            .ok_or_else(|| StorageError::EdgeNotFound(id).into())
    }

    /// Delete a node.
    ///
    /// Note: This does not delete edges connected to the node.
    /// TODO: Add cascade delete option.
    pub fn delete_node(&mut self, id: NodeId) -> Result<Node> {
        self.indexes
            .remove_node(id)
            .ok_or_else(|| StorageError::NodeNotFound(id).into())
    }

    /// Delete an edge.
    pub fn delete_edge(&mut self, id: EdgeId) -> Result<Edge> {
        let edge = self
            .indexes
            .remove_edge(id)
            .ok_or_else(|| StorageError::EdgeNotFound(id))?;

        // Rebuild adjacency indexes
        self.indexes.rebuild_adjacency();

        Ok(edge)
    }

    /// Get all outgoing edges from a node.
    ///
    /// This is the critical "hot path" operation that must be fast.
    pub fn get_outgoing_edges(&self, source: NodeId) -> Vec<EdgeId> {
        self.indexes
            .get_outgoing(source)
            .iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get all incoming edges to a node.
    pub fn get_incoming_edges(&self, target: NodeId) -> Vec<EdgeId> {
        self.indexes
            .get_incoming(target)
            .iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get outgoing edges with a specific label.
    pub fn get_outgoing_edges_with_label(&self, source: NodeId, label: &str) -> Vec<EdgeId> {
        let label_id = match self.interner.get_id(label) {
            Some(id) => id,
            None => return Vec::new(), // Label doesn't exist
        };

        self.indexes
            .get_outgoing_with_label(source, label_id)
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get incoming edges with a specific label.
    pub fn get_incoming_edges_with_label(&self, target: NodeId, label: &str) -> Vec<EdgeId> {
        let label_id = match self.interner.get_id(label) {
            Some(id) => id,
            None => return Vec::new(),
        };

        self.indexes
            .get_incoming_with_label(target, label_id)
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get the number of nodes.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.indexes.node_count()
    }

    /// Get the number of edges.
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.indexes.edge_count()
    }

    /// Get the out-degree of a node.
    #[inline]
    pub fn out_degree(&self, node: NodeId) -> usize {
        self.indexes.out_degree(node)
    }

    /// Get the in-degree of a node.
    #[inline]
    pub fn in_degree(&self, node: NodeId) -> usize {
        self.indexes.in_degree(node)
    }

    /// Get statistics about the current storage
    pub fn stats(&self) -> CurrentStats {
        CurrentStats {
            node_count: self.node_count(),
            edge_count: self.edge_count(),
        }
    }
}

impl Default for CurrentStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_create_node() {
        let mut storage = CurrentStorage::new();

        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let node_id = storage.create_node("Person", props).unwrap();

        assert_eq!(storage.node_count(), 1);

        let node = storage.get_node(node_id).unwrap();
        assert_eq!(node.id, node_id);
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
    }

    #[test]
    fn test_create_edge() {
        let mut storage = CurrentStorage::new();

        let alice = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let bob = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let edge_id = storage
            .create_edge(
                alice,
                bob,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
            )
            .unwrap();

        assert_eq!(storage.edge_count(), 1);

        let edge = storage.get_edge(edge_id).unwrap();
        assert_eq!(edge.source, alice);
        assert_eq!(edge.target, bob);
        assert_eq!(
            edge.get_property("since").and_then(|v| v.as_int()),
            Some(2020)
        );
    }

    #[test]
    fn test_create_edge_invalid_nodes() {
        let mut storage = CurrentStorage::new();

        let result = storage.create_edge(
            NodeId::new(999),
            NodeId::new(1000),
            "KNOWS",
            PropertyMapBuilder::new().build(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_graph_traversal() {
        let mut storage = CurrentStorage::new();

        // Create nodes
        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edges
        storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        storage
            .create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        storage
            .create_edge(n1, n2, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Test outgoing edges
        let outgoing = storage.get_outgoing_edges(n0);
        assert_eq!(outgoing.len(), 2);

        // Test incoming edges
        let incoming = storage.get_incoming_edges(n2);
        assert_eq!(incoming.len(), 2);

        // Test degree
        assert_eq!(storage.out_degree(n0), 2);
        assert_eq!(storage.in_degree(n2), 2);
    }

    #[test]
    fn test_labeled_edges() {
        let mut storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        storage
            .create_edge(n0, n2, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Get only KNOWS edges
        let knows_edges = storage.get_outgoing_edges_with_label(n0, "KNOWS");
        assert_eq!(knows_edges.len(), 1);

        // Get only FOLLOWS edges
        let follows_edges = storage.get_outgoing_edges_with_label(n0, "FOLLOWS");
        assert_eq!(follows_edges.len(), 1);

        // Non-existent label
        let none_edges = storage.get_outgoing_edges_with_label(n0, "LOVES");
        assert_eq!(none_edges.len(), 0);
    }

    #[test]
    fn test_delete_node() {
        let mut storage = CurrentStorage::new();

        let node_id = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        assert_eq!(storage.node_count(), 1);

        let deleted = storage.delete_node(node_id).unwrap();
        assert_eq!(deleted.id, node_id);
        assert_eq!(storage.node_count(), 0);

        // Second delete should fail
        assert!(storage.delete_node(node_id).is_err());
    }

    #[test]
    fn test_delete_edge() {
        let mut storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let edge_id = storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        assert_eq!(storage.edge_count(), 1);
        assert_eq!(storage.out_degree(n0), 1);

        storage.delete_edge(edge_id).unwrap();

        assert_eq!(storage.edge_count(), 0);
        assert_eq!(storage.out_degree(n0), 0);
    }
}
