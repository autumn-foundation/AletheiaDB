//! Current-state indexes using concurrent data structures.
//!
//! This module provides indexes for the current state of the graph using
//! DashMap for lock-free concurrent access. These are the "hot path" indexes
//! that must be extremely fast.

use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::interning::InternedString;
use crate::index::adjacency::{AdjacencyEntry, AdjacencyIndex};
use dashmap::DashMap;
use std::sync::Arc;

/// Concurrent indexes for current-state graph queries.
///
/// These indexes provide O(1) lookups for nodes and edges, plus efficient
/// graph traversal through CSR adjacency indexes.
pub struct CurrentIndexes {
    /// Node ID → Node (O(1) lookup)
    nodes: DashMap<NodeId, Node>,
    /// Edge ID → Edge (O(1) lookup)
    edges: DashMap<EdgeId, Edge>,
    /// Outgoing edges: source node → adjacency list
    outgoing: Arc<AdjacencyIndex>,
    /// Incoming edges: target node → adjacency list
    incoming: Arc<AdjacencyIndex>,
}

impl CurrentIndexes {
    /// Create new empty indexes.
    pub fn new() -> Self {
        CurrentIndexes {
            nodes: DashMap::new(),
            edges: DashMap::new(),
            outgoing: Arc::new(AdjacencyIndex::new()),
            incoming: Arc::new(AdjacencyIndex::new()),
        }
    }

    /// Insert a node into the indexes.
    pub fn insert_node(&self, node: Node) {
        self.nodes.insert(node.id, node);
    }

    /// Insert an edge into the indexes.
    ///
    /// Note: This only updates the edge map. Adjacency indexes are rebuilt
    /// separately for efficiency (batch updates).
    pub fn insert_edge(&self, edge: Edge) {
        self.edges.insert(edge.id, edge);
    }

    /// Get a node by ID.
    pub fn get_node(&self, id: NodeId) -> Option<Node> {
        self.nodes.get(&id).map(|entry| entry.value().clone())
    }

    /// Get an edge by ID.
    pub fn get_edge(&self, id: EdgeId) -> Option<Edge> {
        self.edges.get(&id).map(|entry| entry.value().clone())
    }

    /// Remove a node from the indexes.
    pub fn remove_node(&self, id: NodeId) -> Option<Node> {
        self.nodes.remove(&id).map(|(_, node)| node)
    }

    /// Remove an edge from the indexes.
    pub fn remove_edge(&self, id: EdgeId) -> Option<Edge> {
        self.edges.remove(&id).map(|(_, edge)| edge)
    }

    /// Get the number of nodes.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Get the number of edges.
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Check if a node exists.
    #[inline]
    pub fn contains_node(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    /// Check if an edge exists.
    #[inline]
    pub fn contains_edge(&self, id: EdgeId) -> bool {
        self.edges.contains_key(&id)
    }

    /// Get outgoing edges for a node.
    ///
    /// Returns the adjacency list for efficient traversal.
    #[inline]
    pub fn get_outgoing(&self, source: NodeId) -> &[AdjacencyEntry] {
        self.outgoing.get_adjacency(source)
    }

    /// Get incoming edges for a node.
    ///
    /// Returns the adjacency list for reverse traversal.
    #[inline]
    pub fn get_incoming(&self, target: NodeId) -> &[AdjacencyEntry] {
        self.incoming.get_adjacency(target)
    }

    /// Get outgoing edges with a specific label.
    pub fn get_outgoing_with_label(
        &self,
        source: NodeId,
        label: InternedString,
    ) -> impl Iterator<Item = &AdjacencyEntry> {
        self.outgoing.get_adjacency_with_label(source, label)
    }

    /// Get incoming edges with a specific label.
    pub fn get_incoming_with_label(
        &self,
        target: NodeId,
        label: InternedString,
    ) -> impl Iterator<Item = &AdjacencyEntry> {
        self.incoming.get_adjacency_with_label(target, label)
    }

    /// Get the out-degree of a node (number of outgoing edges).
    #[inline]
    pub fn out_degree(&self, node: NodeId) -> usize {
        self.outgoing.degree(node)
    }

    /// Get the in-degree of a node (number of incoming edges).
    #[inline]
    pub fn in_degree(&self, node: NodeId) -> usize {
        self.incoming.degree(node)
    }

    /// Rebuild adjacency indexes from current edges.
    ///
    /// This should be called after batch edge insertions/deletions.
    /// It's more efficient to rebuild than to incrementally update for large changes.
    pub fn rebuild_adjacency(&mut self) {
        let mut outgoing_edges = Vec::new();
        let mut incoming_edges = Vec::new();

        // Collect all edges
        for entry in self.edges.iter() {
            let edge = entry.value();
            outgoing_edges.push((edge.source, edge.target, edge.id, edge.label));
            incoming_edges.push((edge.target, edge.source, edge.id, edge.label));
        }

        // Rebuild indexes
        self.outgoing = Arc::new(AdjacencyIndex::build(outgoing_edges));
        self.incoming = Arc::new(AdjacencyIndex::build(incoming_edges));
    }

    /// Iterate over all nodes.
    pub fn iter_nodes(&self) -> impl Iterator<Item = Node> + '_ {
        self.nodes.iter().map(|entry| entry.value().clone())
    }

    /// Iterate over all edges.
    pub fn iter_edges(&self) -> impl Iterator<Item = Edge> + '_ {
        self.edges.iter().map(|entry| entry.value().clone())
    }
}

impl Default for CurrentIndexes {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::VersionId;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;

    fn create_test_node(id: u64, label: &str) -> Node {
        Node::new(
            NodeId::new(id),
            GLOBAL_INTERNER.intern(label),
            PropertyMapBuilder::new().build(),
            VersionId::new(1),
        )
    }

    fn create_test_edge(id: u64, source: u64, target: u64, label: &str) -> Edge {
        Edge::new(
            EdgeId::new(id),
            GLOBAL_INTERNER.intern(label),
            NodeId::new(source),
            NodeId::new(target),
            PropertyMapBuilder::new().build(),
            VersionId::new(1),
        )
    }

    #[test]
    fn test_node_operations() {
        let indexes = CurrentIndexes::new();

        // Initially empty
        assert_eq!(indexes.node_count(), 0);
        assert!(!indexes.contains_node(NodeId::new(1)));

        // Insert node
        let node = create_test_node(1, "Person");
        indexes.insert_node(node.clone());

        assert_eq!(indexes.node_count(), 1);
        assert!(indexes.contains_node(NodeId::new(1)));

        // Get node
        let retrieved = indexes.get_node(NodeId::new(1)).unwrap();
        assert_eq!(retrieved.id, node.id);
        assert_eq!(retrieved.label, node.label);

        // Remove node
        let removed = indexes.remove_node(NodeId::new(1)).unwrap();
        assert_eq!(removed.id, node.id);
        assert_eq!(indexes.node_count(), 0);
    }

    #[test]
    fn test_edge_operations() {
        let indexes = CurrentIndexes::new();

        // Insert edge
        let edge = create_test_edge(1, 0, 1, "KNOWS");
        indexes.insert_edge(edge.clone());

        assert_eq!(indexes.edge_count(), 1);
        assert!(indexes.contains_edge(EdgeId::new(1)));

        // Get edge
        let retrieved = indexes.get_edge(EdgeId::new(1)).unwrap();
        assert_eq!(retrieved.id, edge.id);
        assert_eq!(retrieved.source, edge.source);
        assert_eq!(retrieved.target, edge.target);

        // Remove edge
        let removed = indexes.remove_edge(EdgeId::new(1)).unwrap();
        assert_eq!(removed.id, edge.id);
        assert_eq!(indexes.edge_count(), 0);
    }

    #[test]
    fn test_adjacency_rebuild() {
        let mut indexes = CurrentIndexes::new();

        // Add nodes
        indexes.insert_node(create_test_node(0, "Person"));
        indexes.insert_node(create_test_node(1, "Person"));
        indexes.insert_node(create_test_node(2, "Person"));

        // Add edges
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 0, 2, "KNOWS"));
        indexes.insert_edge(create_test_edge(2, 1, 2, "KNOWS"));

        // Rebuild adjacency indexes
        indexes.rebuild_adjacency();

        // Test outgoing edges
        assert_eq!(indexes.out_degree(NodeId::new(0)), 2);
        assert_eq!(indexes.out_degree(NodeId::new(1)), 1);
        assert_eq!(indexes.out_degree(NodeId::new(2)), 0);

        let outgoing = indexes.get_outgoing(NodeId::new(0));
        assert_eq!(outgoing.len(), 2);

        // Test incoming edges
        assert_eq!(indexes.in_degree(NodeId::new(0)), 0);
        assert_eq!(indexes.in_degree(NodeId::new(1)), 1);
        assert_eq!(indexes.in_degree(NodeId::new(2)), 2);
    }

    #[test]
    fn test_labeled_traversal() {
        let mut indexes = CurrentIndexes::new();

        let knows = GLOBAL_INTERNER.intern("KNOWS");
        let follows = GLOBAL_INTERNER.intern("FOLLOWS");

        // Add edges with different labels
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 0, 2, "FOLLOWS"));
        indexes.insert_edge(create_test_edge(2, 0, 3, "KNOWS"));

        indexes.rebuild_adjacency();

        // Get only KNOWS edges
        let knows_edges: Vec<_> = indexes
            .get_outgoing_with_label(NodeId::new(0), knows)
            .collect();
        assert_eq!(knows_edges.len(), 2);

        // Get only FOLLOWS edges
        let follows_edges: Vec<_> = indexes
            .get_outgoing_with_label(NodeId::new(0), follows)
            .collect();
        assert_eq!(follows_edges.len(), 1);
    }

    #[test]
    fn test_iteration() {
        let indexes = CurrentIndexes::new();

        // Add some nodes and edges
        indexes.insert_node(create_test_node(0, "Person"));
        indexes.insert_node(create_test_node(1, "Person"));
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));

        // Test iteration
        let nodes: Vec<_> = indexes.iter_nodes().collect();
        assert_eq!(nodes.len(), 2);

        let edges: Vec<_> = indexes.iter_edges().collect();
        assert_eq!(edges.len(), 1);
    }
}
