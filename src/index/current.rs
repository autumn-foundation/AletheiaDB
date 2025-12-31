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
use std::sync::{Arc, RwLock};

/// Concurrent indexes for current-state graph queries.
///
/// These indexes provide O(1) lookups for nodes and edges, plus efficient
/// graph traversal through CSR adjacency indexes.
pub struct CurrentIndexes {
    /// Node ID → Node (O(1) lookup)
    nodes: DashMap<NodeId, Node>,
    /// Edge ID → Edge (O(1) lookup)
    edges: DashMap<EdgeId, Edge>,
    /// Outgoing edges: source node → adjacency list (RwLock for rebuild)
    outgoing: Arc<RwLock<AdjacencyIndex>>,
    /// Incoming edges: target node → adjacency list (RwLock for rebuild)
    incoming: Arc<RwLock<AdjacencyIndex>>,
}

impl CurrentIndexes {
    /// Create new empty indexes.
    pub fn new() -> Self {
        CurrentIndexes {
            nodes: DashMap::new(),
            edges: DashMap::new(),
            outgoing: Arc::new(RwLock::new(AdjacencyIndex::new())),
            incoming: Arc::new(RwLock::new(AdjacencyIndex::new())),
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
    /// Returns a copy of the adjacency list for traversal.
    #[inline]
    pub fn get_outgoing(&self, source: NodeId) -> Vec<AdjacencyEntry> {
        let outgoing = self.outgoing.read().unwrap();
        outgoing.get_adjacency(source).to_vec()
    }

    /// Get incoming edges for a node.
    ///
    /// Returns a copy of the adjacency list for reverse traversal.
    #[inline]
    pub fn get_incoming(&self, target: NodeId) -> Vec<AdjacencyEntry> {
        let incoming = self.incoming.read().unwrap();
        incoming.get_adjacency(target).to_vec()
    }

    /// Get outgoing edges with a specific label.
    pub fn get_outgoing_with_label(
        &self,
        source: NodeId,
        label: InternedString,
    ) -> Vec<AdjacencyEntry> {
        let outgoing = self.outgoing.read().unwrap();
        outgoing
            .get_adjacency_with_label(source, label)
            .copied()
            .collect()
    }

    /// Get incoming edges with a specific label.
    pub fn get_incoming_with_label(
        &self,
        target: NodeId,
        label: InternedString,
    ) -> Vec<AdjacencyEntry> {
        let incoming = self.incoming.read().unwrap();
        incoming
            .get_adjacency_with_label(target, label)
            .copied()
            .collect()
    }

    /// Get the out-degree of a node (number of outgoing edges).
    #[inline]
    pub fn out_degree(&self, node: NodeId) -> usize {
        let outgoing = self.outgoing.read().unwrap();
        outgoing.degree(node)
    }

    /// Get the in-degree of a node (number of incoming edges).
    #[inline]
    pub fn in_degree(&self, node: NodeId) -> usize {
        let incoming = self.incoming.read().unwrap();
        incoming.degree(node)
    }

    /// Rebuild adjacency indexes from current edges.
    ///
    /// This should be called after batch edge insertions/deletions.
    /// It's more efficient to rebuild than to incrementally update for large changes.
    ///
    /// # Performance
    ///
    /// **Current Implementation:**
    /// - Complexity: O(E log E) where E is total edges
    /// - Always rebuilds complete index from scratch
    /// - Acquires write lock, blocking concurrent readers
    ///
    /// **Future Optimization Opportunities:**
    ///
    /// 1. **Partial/Incremental Rebuild:**
    ///    - Track "dirty" nodes that had edge changes
    ///    - Only rebuild adjacency lists for affected nodes
    ///    - Potential speedup: 10-100x for localized changes
    ///    - Trade-off: Memory overhead for tracking dirty set
    ///
    /// 2. **Concurrent Rebuild with RCU:**
    ///    - Build new index while readers use old index
    ///    - Atomic pointer swap when complete
    ///    - Eliminates read blocking during rebuild
    ///    - Trade-off: Double memory usage during rebuild
    ///
    /// 3. **Lock-Free Adjacency Updates:**
    ///    - Use lock-free CSR representation
    ///    - Incremental updates without global rebuild
    ///    - Potential speedup: 100-1000x for small batches
    ///    - Trade-off: Complex concurrent data structure
    ///
    /// For now, full rebuild is simple, correct, and fast enough for
    /// batch operations (1-10ms for 10K edges).
    pub fn rebuild_adjacency(&self) {
        let mut outgoing_edges = Vec::new();
        let mut incoming_edges = Vec::new();

        // Collect all edges
        for entry in self.edges.iter() {
            let edge = entry.value();
            outgoing_edges.push((edge.source, edge.target, edge.id, edge.label));
            incoming_edges.push((edge.target, edge.source, edge.id, edge.label));
        }

        // Rebuild indexes with write locks
        *self.outgoing.write().unwrap() = AdjacencyIndex::build(outgoing_edges);
        *self.incoming.write().unwrap() = AdjacencyIndex::build(incoming_edges);
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

    pub(super) fn create_test_node(id: u64, label: &str) -> Node {
        Node::new(
            NodeId::new(id),
            GLOBAL_INTERNER.intern(label),
            PropertyMapBuilder::new().build(),
            VersionId::new(1),
        )
    }

    pub(super) fn create_test_edge(id: u64, source: u64, target: u64, label: &str) -> Edge {
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
        let indexes = CurrentIndexes::new();

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
        let indexes = CurrentIndexes::new();

        let knows = GLOBAL_INTERNER.intern("KNOWS");
        let follows = GLOBAL_INTERNER.intern("FOLLOWS");

        // Add edges with different labels
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 0, 2, "FOLLOWS"));
        indexes.insert_edge(create_test_edge(2, 0, 3, "KNOWS"));

        indexes.rebuild_adjacency();

        // Get only KNOWS edges
        let knows_edges = indexes.get_outgoing_with_label(NodeId::new(0), knows);
        assert_eq!(knows_edges.len(), 2);

        // Get only FOLLOWS edges
        let follows_edges = indexes.get_outgoing_with_label(NodeId::new(0), follows);
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

    #[test]
    fn test_rebuild_idempotent() {
        let indexes = CurrentIndexes::new();

        // Add edges
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 0, 2, "KNOWS"));

        // Rebuild once
        indexes.rebuild_adjacency();
        let first_out = indexes.get_outgoing(NodeId::new(0));
        let first_in = indexes.get_incoming(NodeId::new(1));

        // Rebuild again
        indexes.rebuild_adjacency();
        let second_out = indexes.get_outgoing(NodeId::new(0));
        let second_in = indexes.get_incoming(NodeId::new(1));

        // Results should be identical
        assert_eq!(first_out.len(), second_out.len());
        assert_eq!(first_in.len(), second_in.len());
        assert_eq!(first_out, second_out);
        assert_eq!(first_in, second_in);
    }

    #[test]
    fn test_rebuild_after_modifications() {
        let indexes = CurrentIndexes::new();

        // Add initial edges
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 1, 2, "KNOWS"));
        indexes.rebuild_adjacency();

        let initial_count = indexes.edge_count();
        assert_eq!(initial_count, 2);

        // Add more edges
        indexes.insert_edge(create_test_edge(2, 0, 2, "LIKES"));
        indexes.rebuild_adjacency();

        // Verify adjacency reflects all edges
        assert_eq!(indexes.out_degree(NodeId::new(0)), 2); // KNOWS and LIKES
        assert_eq!(indexes.in_degree(NodeId::new(2)), 2); // from 1 and 0

        // Remove an edge
        indexes.remove_edge(EdgeId::new(1));
        indexes.rebuild_adjacency();

        // Verify adjacency updated correctly
        assert_eq!(indexes.out_degree(NodeId::new(1)), 0);
        assert_eq!(indexes.in_degree(NodeId::new(2)), 1); // only from 0 now
    }
}

// Property-based tests for rebuild safety
#[cfg(test)]
mod proptests {
    use super::tests::create_test_edge;
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum EdgeOp {
        Insert(u64, u64, u64), // edge_id, source, target
        Remove(u64),           // edge_id
    }

    // Generate random sequences of edge operations
    fn edge_op_strategy() -> impl Strategy<Value = Vec<EdgeOp>> {
        prop::collection::vec(
            prop_oneof![
                (0u64..100, 0u64..10, 0u64..10)
                    .prop_map(|(id, src, tgt)| EdgeOp::Insert(id, src, tgt)),
                (0u64..100).prop_map(EdgeOp::Remove),
            ],
            1..50,
        )
    }

    proptest! {
        #[test]
        fn rebuild_maintains_edge_count(ops in edge_op_strategy()) {
            let indexes = CurrentIndexes::new();

            // Apply operations
            for op in &ops {
                match op {
                    EdgeOp::Insert(id, src, tgt) => {
                        indexes.insert_edge(create_test_edge(*id, *src, *tgt, "TEST"));
                    }
                    EdgeOp::Remove(id) => {
                        let _ = indexes.remove_edge(EdgeId::new(*id));
                    }
                }
            }

            // Rebuild adjacency
            indexes.rebuild_adjacency();

            // Verify: total degree should equal 2 * edge_count (each edge contributes to out and in)
            let edge_count = indexes.edge_count();
            let mut total_out_degree = 0;
            let mut total_in_degree = 0;

            for node_id in 0..10 {
                total_out_degree += indexes.out_degree(NodeId::new(node_id));
                total_in_degree += indexes.in_degree(NodeId::new(node_id));
            }

            // Each edge appears once in outgoing and once in incoming
            assert_eq!(total_out_degree, edge_count);
            assert_eq!(total_in_degree, edge_count);
        }

        #[test]
        fn rebuild_is_deterministic(ops in edge_op_strategy()) {
            let indexes = CurrentIndexes::new();

            // Apply operations
            for op in &ops {
                match op {
                    EdgeOp::Insert(id, src, tgt) => {
                        indexes.insert_edge(create_test_edge(*id, *src, *tgt, "TEST"));
                    }
                    EdgeOp::Remove(id) => {
                        let _ = indexes.remove_edge(EdgeId::new(*id));
                    }
                }
            }

            // Rebuild twice
            indexes.rebuild_adjacency();
            let first_results: Vec<_> = (0..10)
                .map(|i| {
                    let node = NodeId::new(i);
                    (indexes.get_outgoing(node), indexes.get_incoming(node))
                })
                .collect();

            indexes.rebuild_adjacency();
            let second_results: Vec<_> = (0..10)
                .map(|i| {
                    let node = NodeId::new(i);
                    (indexes.get_outgoing(node), indexes.get_incoming(node))
                })
                .collect();

            // Results should be identical
            assert_eq!(first_results, second_results);
        }
    }
}
