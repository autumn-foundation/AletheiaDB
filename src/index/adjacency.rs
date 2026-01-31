//! CSR (Compressed Sparse Row) adjacency index for graph traversals.
//!
//! This module implements a cache-friendly adjacency list representation using
//! the Compressed Sparse Row format. This enables fast sequential access during
//! graph traversals with minimal cache misses.
//!
//! # CSR Format
//!
//! The CSR format stores the graph as two arrays:
//! - `offsets[i]`: Starting position in `edges` for node i's adjacency list
//! - `edges`: Flat array of (target_node, edge_id, edge_label) tuples
//!
//! This layout is cache-friendly because traversing from a node requires
//! sequential access to a contiguous region of memory.

use crate::core::id::{EdgeId, NodeId};
use crate::core::interning::InternedString;

/// A single entry in the adjacency list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdjacencyEntry {
    /// Target node ID.
    pub target: NodeId,
    /// Edge ID connecting source to target.
    pub edge_id: EdgeId,
    /// Edge label (interned for memory efficiency).
    pub label: InternedString,
}

impl AdjacencyEntry {
    /// Create a new adjacency entry.
    #[inline]
    pub const fn new(target: NodeId, edge_id: EdgeId, label: InternedString) -> Self {
        AdjacencyEntry {
            target,
            edge_id,
            label,
        }
    }
}

/// Compressed Sparse Row adjacency index.
///
/// This structure provides O(log n) access to a node's adjacency list with
/// excellent cache locality and memory efficiency for sparse node IDs.
///
/// The index uses a sparse representation where only nodes with outgoing edges
/// are stored, making it efficient even with large gaps in node IDs (e.g., after deletions).
#[derive(Debug, Clone)]
pub struct AdjacencyIndex {
    /// Sorted list of node IDs that have outgoing edges.
    /// Used for binary search to map node_id -> index in offsets array.
    node_ids: Vec<NodeId>,
    /// Offsets into the edges array for each node in node_ids.
    /// offsets[i] = start index in edges array for node_ids[i]
    /// offsets[i + 1] = end index (exclusive)
    offsets: Vec<usize>,
    /// Flat array of adjacency entries, sorted by source node.
    edges: Vec<AdjacencyEntry>,
    /// Maximum node ID (for bounds checking).
    max_node_id: u64,
}

impl AdjacencyIndex {
    /// Export CSR data for persistence.
    ///
    /// Returns (node_ids, offsets, edge_ids) where:
    /// - node_ids: sorted array of node IDs that have outgoing edges
    /// - offsets: offset array for CSR format
    /// - edge_ids: flat array of edge IDs
    pub fn export_csr(&self) -> (Vec<u64>, Vec<u64>, Vec<u64>) {
        let node_ids = self.node_ids.iter().map(|n| n.as_u64()).collect();
        let offsets = self.offsets.iter().map(|&x| x as u64).collect();
        let edge_ids = self.edges.iter().map(|e| e.edge_id.as_u64()).collect();
        (node_ids, offsets, edge_ids)
    }

    /// Import CSR data from persistence, reconstructing adjacency entries from edges.
    ///
    /// # Arguments
    /// * `node_ids` - Sorted array of node IDs that have outgoing edges
    /// * `offsets` - CSR offset array
    /// * `edge_ids` - Flat array of edge IDs
    /// * `edges_map` - Map from edge ID to (target, label) for reconstruction
    pub fn import_csr(
        node_ids: Vec<u64>,
        offsets: Vec<u64>,
        edge_ids: Vec<u64>,
        edges_map: &std::collections::HashMap<EdgeId, (NodeId, InternedString)>,
    ) -> Self {
        if offsets.is_empty() || edge_ids.is_empty() {
            return Self::new();
        }

        let max_node_id = node_ids.iter().max().copied().unwrap_or(0);

        let node_ids_typed: Vec<NodeId> = node_ids
            .iter()
            .map(|&id| NodeId::new_unchecked(id))
            .collect();
        let offsets_usize: Vec<usize> = offsets.iter().map(|&x| x as usize).collect();
        let mut adjacency_entries = Vec::with_capacity(edge_ids.len());

        for &edge_id_u64 in &edge_ids {
            let edge_id = EdgeId::new_unchecked(edge_id_u64);
            if let Some((target, label)) = edges_map.get(&edge_id) {
                adjacency_entries.push(AdjacencyEntry::new(*target, edge_id, *label));
            } else {
                // Edge not found - this shouldn't happen with valid data
                // Use a placeholder to maintain CSR structure integrity
                adjacency_entries.push(AdjacencyEntry::new(
                    NodeId::new_unchecked(0),
                    edge_id,
                    InternedString::from_raw(0),
                ));
            }
        }

        Self {
            node_ids: node_ids_typed,
            offsets: offsets_usize,
            edges: adjacency_entries,
            max_node_id,
        }
    }
}

impl AdjacencyIndex {
    /// Create a new empty adjacency index.
    pub fn new() -> Self {
        AdjacencyIndex {
            node_ids: Vec::new(),
            offsets: vec![0],
            edges: Vec::new(),
            max_node_id: 0,
        }
    }

    /// Build an adjacency index from a list of edges.
    ///
    /// Edges should be provided as (source, target, edge_id, label) tuples.
    ///
    /// This uses a sparse representation: only nodes with outgoing edges are stored,
    /// making it efficient even with large gaps in node IDs (O(num_nodes) instead of O(max_node_id)).
    pub fn build(mut edges: Vec<(NodeId, NodeId, EdgeId, InternedString)>) -> Self {
        if edges.is_empty() {
            return Self::new();
        }

        let edge_count = edges.len();

        // Sort by source node, then target node for deterministic ordering.
        // This avoids using a HashMap for grouping, reducing memory allocations and hashing overhead.
        edges.sort_by_key(|(src, target, _, _)| (*src, *target));

        // Max node id is the maximum of all source and target IDs
        let max_node_id = edges
            .iter()
            .flat_map(|(src, target, _, _)| [src.as_u64(), target.as_u64()])
            .max()
            .unwrap_or(0);

        // Pre-allocate assuming some average degree > 1 to avoid resizing
        let estimated_nodes = (edge_count / 4).max(16);
        let mut node_ids = Vec::with_capacity(estimated_nodes);
        let mut offsets = Vec::with_capacity(estimated_nodes + 1);
        let mut flat_edges = Vec::with_capacity(edge_count);

        offsets.push(0);

        if !edges.is_empty() {
            let mut current_source = edges[0].0;
            node_ids.push(current_source);

            for (source, target, edge_id, label) in edges {
                if source != current_source {
                    offsets.push(flat_edges.len());
                    current_source = source;
                    node_ids.push(current_source);
                }
                flat_edges.push(AdjacencyEntry::new(target, edge_id, label));
            }
            offsets.push(flat_edges.len());
        }

        AdjacencyIndex {
            node_ids,
            offsets,
            edges: flat_edges,
            max_node_id,
        }
    }

    /// Get the adjacency list for a node.
    ///
    /// Returns a slice of adjacency entries for the given node.
    /// Returns an empty slice if the node has no outgoing edges.
    ///
    /// This uses binary search to locate the node, providing O(log n) lookup
    /// where n is the number of nodes with outgoing edges.
    #[inline]
    pub fn get_adjacency(&self, node: NodeId) -> &[AdjacencyEntry] {
        // Binary search to find the node's index in node_ids
        match self.node_ids.binary_search(&node) {
            Ok(idx) => {
                let start = self.offsets[idx];
                let end = self.offsets[idx + 1];
                &self.edges[start..end]
            }
            Err(_) => {
                // Node not found (no outgoing edges)
                &[]
            }
        }
    }

    /// Get outgoing edges for a node with a specific label.
    ///
    /// Returns an iterator over adjacency entries that match the label.
    pub fn get_adjacency_with_label(
        &self,
        node: NodeId,
        label: InternedString,
    ) -> impl Iterator<Item = &AdjacencyEntry> {
        self.get_adjacency(node)
            .iter()
            .filter(move |entry| entry.label == label)
    }

    /// Get the number of outgoing edges for a node (out-degree).
    #[inline]
    pub fn degree(&self, node: NodeId) -> usize {
        self.get_adjacency(node).len()
    }

    /// Check if a node has any outgoing edges.
    #[inline]
    pub fn has_edges(&self, node: NodeId) -> bool {
        self.degree(node) > 0
    }

    /// Get total number of edges in the index.
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Get the maximum node ID in this index.
    #[inline]
    pub fn max_node_id(&self) -> u64 {
        self.max_node_id
    }

    /// Iterate over all nodes that have outgoing edges.
    ///
    /// This is efficient for sparse graphs as it only yields nodes
    /// that actually have edges, not all possible node IDs.
    #[inline]
    pub fn iter_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.node_ids.iter().copied()
    }

    /// Get the number of nodes with outgoing edges.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.node_ids.len()
    }
}

impl Default for AdjacencyIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;

    #[test]
    fn test_empty_index() {
        let index = AdjacencyIndex::new();
        assert_eq!(index.edge_count(), 0);
        assert_eq!(index.degree(NodeId::new(0).unwrap()), 0);
        assert_eq!(index.get_adjacency(NodeId::new(0).unwrap()).len(), 0);
    }

    #[test]
    fn test_build_simple_graph() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        let edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(1).unwrap(),
                knows,
            ),
            (
                NodeId::new(1).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(2).unwrap(),
                knows,
            ),
        ];

        let index = AdjacencyIndex::build(edges);

        // Node 0 has 2 outgoing edges
        assert_eq!(index.degree(NodeId::new(0).unwrap()), 2);
        let adj0 = index.get_adjacency(NodeId::new(0).unwrap());
        assert_eq!(adj0.len(), 2);
        assert_eq!(adj0[0].target, NodeId::new(1).unwrap());
        assert_eq!(adj0[1].target, NodeId::new(2).unwrap());

        // Node 1 has 1 outgoing edge
        assert_eq!(index.degree(NodeId::new(1).unwrap()), 1);
        let adj1 = index.get_adjacency(NodeId::new(1).unwrap());
        assert_eq!(adj1.len(), 1);
        assert_eq!(adj1[0].target, NodeId::new(2).unwrap());

        // Node 2 has no outgoing edges
        assert_eq!(index.degree(NodeId::new(2).unwrap()), 0);

        // Total edges
        assert_eq!(index.edge_count(), 3);
    }

    #[test]
    fn test_multiple_edge_labels() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let follows = GLOBAL_INTERNER.intern("FOLLOWS").unwrap();

        let edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(1).unwrap(),
                follows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(3).unwrap(),
                EdgeId::new(2).unwrap(),
                knows,
            ),
        ];

        let index = AdjacencyIndex::build(edges);

        // Get all edges from node 0
        assert_eq!(index.degree(NodeId::new(0).unwrap()), 3);

        // Get only KNOWS edges from node 0
        let knows_edges: Vec<_> = index
            .get_adjacency_with_label(NodeId::new(0).unwrap(), knows)
            .collect();
        assert_eq!(knows_edges.len(), 2);

        // Get only FOLLOWS edges from node 0
        let follows_edges: Vec<_> = index
            .get_adjacency_with_label(NodeId::new(0).unwrap(), follows)
            .collect();
        assert_eq!(follows_edges.len(), 1);
        assert_eq!(follows_edges[0].target, NodeId::new(2).unwrap());
    }

    #[test]
    fn test_node_without_edges() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        let edges = vec![(
            NodeId::new(0).unwrap(),
            NodeId::new(1).unwrap(),
            EdgeId::new(0).unwrap(),
            knows,
        )];

        let index = AdjacencyIndex::build(edges);

        // Node 5 doesn't exist
        assert_eq!(index.degree(NodeId::new(5).unwrap()), 0);
        assert!(!index.has_edges(NodeId::new(5).unwrap()));
        assert_eq!(index.get_adjacency(NodeId::new(5).unwrap()).len(), 0);
    }

    #[test]
    fn test_adjacency_entry() {
        let label = GLOBAL_INTERNER.intern("TEST").unwrap();
        let entry = AdjacencyEntry::new(NodeId::new(1).unwrap(), EdgeId::new(100).unwrap(), label);

        assert_eq!(entry.target, NodeId::new(1).unwrap());
        assert_eq!(entry.edge_id, EdgeId::new(100).unwrap());
        assert_eq!(entry.label, label);
    }

    #[test]
    fn test_sorted_adjacency() {
        // Edges deliberately out of order
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(3).unwrap(),
                EdgeId::new(2).unwrap(),
                knows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(1).unwrap(),
                knows,
            ),
        ];

        let index = AdjacencyIndex::build(edges);
        let adj = index.get_adjacency(NodeId::new(0).unwrap());

        // Should be sorted by target
        assert_eq!(adj[0].target, NodeId::new(1).unwrap());
        assert_eq!(adj[1].target, NodeId::new(2).unwrap());
        assert_eq!(adj[2].target, NodeId::new(3).unwrap());
    }

    #[test]
    fn test_sparse_node_ids() {
        // Simulate scenario after deletions: only nodes 10, 1000, and 1_000_000 exist
        // This tests that we handle sparse IDs efficiently
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let edges = vec![
            (
                NodeId::new(10).unwrap(),
                NodeId::new(20).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(1000).unwrap(),
                NodeId::new(2000).unwrap(),
                EdgeId::new(1).unwrap(),
                knows,
            ),
            (
                NodeId::new(1_000_000).unwrap(),
                NodeId::new(2_000_000).unwrap(),
                EdgeId::new(2).unwrap(),
                knows,
            ),
        ];

        let index = AdjacencyIndex::build(edges);

        // Verify correctness for sparse nodes
        assert_eq!(index.degree(NodeId::new(10).unwrap()), 1);
        assert_eq!(index.degree(NodeId::new(1000).unwrap()), 1);
        assert_eq!(index.degree(NodeId::new(1_000_000).unwrap()), 1);

        // Verify intermediate non-existent nodes return empty adjacency
        assert_eq!(index.degree(NodeId::new(0).unwrap()), 0);
        assert_eq!(index.degree(NodeId::new(100).unwrap()), 0);
        assert_eq!(index.degree(NodeId::new(50000).unwrap()), 0);

        // Verify adjacency list content
        let adj10 = index.get_adjacency(NodeId::new(10).unwrap());
        assert_eq!(adj10.len(), 1);
        assert_eq!(adj10[0].target, NodeId::new(20).unwrap());

        let adj1000 = index.get_adjacency(NodeId::new(1000).unwrap());
        assert_eq!(adj1000.len(), 1);
        assert_eq!(adj1000[0].target, NodeId::new(2000).unwrap());

        let adj1m = index.get_adjacency(NodeId::new(1_000_000).unwrap());
        assert_eq!(adj1m.len(), 1);
        assert_eq!(adj1m[0].target, NodeId::new(2_000_000).unwrap());

        // Total edges should still be 3
        assert_eq!(index.edge_count(), 3);
    }

    #[test]
    fn test_sparse_ids_memory_efficiency() {
        // Test that sparse IDs don't cause excessive memory allocation
        // With old implementation: offsets would be Vec with 1_000_001 elements
        // With new implementation: offsets should only have entries for actual nodes
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(1_000_000).unwrap(),
                NodeId::new(1_000_001).unwrap(),
                EdgeId::new(1).unwrap(),
                knows,
            ),
        ];

        let index = AdjacencyIndex::build(edges);

        // After optimization, offsets should be proportional to number of nodes, not max_node_id
        // With 2 source nodes, we should have at most a few entries, not 1_000_001
        // Allow some overhead for implementation details
        assert!(
            index.offsets.len() < 100,
            "Offsets array should be compact for sparse IDs, got {} entries",
            index.offsets.len()
        );

        // Verify correctness
        assert_eq!(index.degree(NodeId::new(0).unwrap()), 1);
        assert_eq!(index.degree(NodeId::new(1_000_000).unwrap()), 1);
        assert_eq!(index.edge_count(), 2);
    }

    #[test]
    fn test_sparse_ids_with_multiple_edges_per_node() {
        // Test sparse IDs where some nodes have multiple outgoing edges
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let follows = GLOBAL_INTERNER.intern("FOLLOWS").unwrap();
        let edges = vec![
            (
                NodeId::new(100).unwrap(),
                NodeId::new(101).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(100).unwrap(),
                NodeId::new(102).unwrap(),
                EdgeId::new(1).unwrap(),
                follows,
            ),
            (
                NodeId::new(100).unwrap(),
                NodeId::new(103).unwrap(),
                EdgeId::new(2).unwrap(),
                knows,
            ),
            (
                NodeId::new(500_000).unwrap(),
                NodeId::new(500_001).unwrap(),
                EdgeId::new(3).unwrap(),
                knows,
            ),
            (
                NodeId::new(500_000).unwrap(),
                NodeId::new(500_002).unwrap(),
                EdgeId::new(4).unwrap(),
                follows,
            ),
        ];

        let index = AdjacencyIndex::build(edges);

        // Verify node 100 has 3 edges
        assert_eq!(index.degree(NodeId::new(100).unwrap()), 3);
        let adj100 = index.get_adjacency(NodeId::new(100).unwrap());
        assert_eq!(adj100.len(), 3);
        // Should be sorted by target
        assert_eq!(adj100[0].target, NodeId::new(101).unwrap());
        assert_eq!(adj100[1].target, NodeId::new(102).unwrap());
        assert_eq!(adj100[2].target, NodeId::new(103).unwrap());

        // Verify node 500_000 has 2 edges
        assert_eq!(index.degree(NodeId::new(500_000).unwrap()), 2);
        let adj500k = index.get_adjacency(NodeId::new(500_000).unwrap());
        assert_eq!(adj500k.len(), 2);
        assert_eq!(adj500k[0].target, NodeId::new(500_001).unwrap());
        assert_eq!(adj500k[1].target, NodeId::new(500_002).unwrap());

        // Verify intermediate nodes have no edges
        assert_eq!(index.degree(NodeId::new(200_000).unwrap()), 0);
        assert_eq!(index.degree(NodeId::new(300_000).unwrap()), 0);

        // Total edges
        assert_eq!(index.edge_count(), 5);
    }

    #[test]
    fn test_build_with_many_edges_preallocation() {
        // Test that building with many edges works correctly.
        // This test verifies the scenario mentioned in issue #193 where
        // pre-allocating the flat_edges Vec avoids ~14 reallocations for 10,000 edges.
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Create 10,000 edges across 1,000 nodes
        let edge_count = 10_000;
        let node_count = 1_000;

        let mut edges = Vec::with_capacity(edge_count);
        for i in 0..edge_count {
            let source = NodeId::new((i % node_count) as u64).unwrap();
            let target = NodeId::new(((i + 1) % node_count) as u64).unwrap();
            let edge_id = EdgeId::new(i as u64).unwrap();
            edges.push((source, target, edge_id, knows));
        }

        // Build the index (should pre-allocate to avoid reallocations)
        let index = AdjacencyIndex::build(edges);

        // Verify correctness
        assert_eq!(index.edge_count(), edge_count);

        // Verify that each node has the correct number of outgoing edges.
        // In this test setup, each node is a source for `edge_count / node_count` edges.
        let expected_degree = edge_count / node_count;
        for i in 0..node_count {
            let node = NodeId::new(i as u64).unwrap();
            let adj = index.get_adjacency(node);
            assert_eq!(
                adj.len(),
                expected_degree,
                "Node {} has an unexpected degree",
                i
            );
            // All adjacency entries should be valid
            for entry in adj {
                assert!(entry.edge_id.as_u64() < edge_count as u64);
                assert!(entry.target.as_u64() < node_count as u64);
            }
        }
    }

    #[test]
    fn test_max_node_id_from_target() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let edges = vec![
            (
                NodeId::new(1).unwrap(),
                NodeId::new(1000).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(2).unwrap(),
                NodeId::new(500).unwrap(),
                EdgeId::new(1).unwrap(),
                knows,
            ),
        ];
        let index = AdjacencyIndex::build(edges);
        assert_eq!(
            index.max_node_id(),
            1000,
            "max_node_id should consider target nodes"
        );
    }
}
