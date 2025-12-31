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
use std::collections::HashMap;

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
/// This structure provides O(1) access to a node's adjacency list with
/// excellent cache locality.
#[derive(Debug, Clone)]
pub struct AdjacencyIndex {
    /// Offsets into the edges array for each node.
    /// offsets[node_id] = start index in edges array
    /// offsets[node_id + 1] = end index (exclusive)
    offsets: Vec<usize>,
    /// Flat array of adjacency entries, sorted by source node.
    edges: Vec<AdjacencyEntry>,
    /// Maximum node ID (for bounds checking).
    max_node_id: u64,
}

impl AdjacencyIndex {
    /// Create a new empty adjacency index.
    pub fn new() -> Self {
        AdjacencyIndex {
            offsets: vec![0],
            edges: Vec::new(),
            max_node_id: 0,
        }
    }

    /// Build an adjacency index from a list of edges.
    ///
    /// Edges should be provided as (source, target, edge_id, label) tuples.
    pub fn build(edges: Vec<(NodeId, NodeId, EdgeId, InternedString)>) -> Self {
        if edges.is_empty() {
            return Self::new();
        }

        // Find maximum node ID to size the offsets array
        let max_node_id = edges
            .iter()
            .map(|(src, _, _, _)| src.as_u64())
            .max()
            .unwrap_or(0);

        // Group edges by source node
        let mut adjacency_map: HashMap<NodeId, Vec<AdjacencyEntry>> = HashMap::new();
        for (source, target, edge_id, label) in edges {
            adjacency_map
                .entry(source)
                .or_insert_with(Vec::new)
                .push(AdjacencyEntry::new(target, edge_id, label));
        }

        // Build CSR format
        let mut offsets = Vec::with_capacity((max_node_id + 2) as usize);
        let mut flat_edges = Vec::new();

        offsets.push(0);

        // Iterate through all node IDs in order
        for node_id in 0..=max_node_id {
            let node = NodeId::new(node_id);
            if let Some(mut adj_list) = adjacency_map.remove(&node) {
                // Sort by target for deterministic ordering
                adj_list.sort_by_key(|e| e.target);
                flat_edges.extend(adj_list);
            }
            offsets.push(flat_edges.len());
        }

        AdjacencyIndex {
            offsets,
            edges: flat_edges,
            max_node_id,
        }
    }

    /// Get the adjacency list for a node.
    ///
    /// Returns a slice of adjacency entries for the given node.
    /// Returns an empty slice if the node has no outgoing edges.
    #[inline]
    pub fn get_adjacency(&self, node: NodeId) -> &[AdjacencyEntry] {
        let node_id = node.as_u64() as usize;

        // Check bounds
        if node_id + 1 >= self.offsets.len() {
            return &[];
        }

        let start = self.offsets[node_id];
        let end = self.offsets[node_id + 1];
        &self.edges[start..end]
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
        assert_eq!(index.degree(NodeId::new(0)), 0);
        assert_eq!(index.get_adjacency(NodeId::new(0)).len(), 0);
    }

    #[test]
    fn test_build_simple_graph() {
        let knows = GLOBAL_INTERNER.intern("KNOWS");

        let edges = vec![
            (NodeId::new(0), NodeId::new(1), EdgeId::new(0), knows),
            (NodeId::new(0), NodeId::new(2), EdgeId::new(1), knows),
            (NodeId::new(1), NodeId::new(2), EdgeId::new(2), knows),
        ];

        let index = AdjacencyIndex::build(edges);

        // Node 0 has 2 outgoing edges
        assert_eq!(index.degree(NodeId::new(0)), 2);
        let adj0 = index.get_adjacency(NodeId::new(0));
        assert_eq!(adj0.len(), 2);
        assert_eq!(adj0[0].target, NodeId::new(1));
        assert_eq!(adj0[1].target, NodeId::new(2));

        // Node 1 has 1 outgoing edge
        assert_eq!(index.degree(NodeId::new(1)), 1);
        let adj1 = index.get_adjacency(NodeId::new(1));
        assert_eq!(adj1.len(), 1);
        assert_eq!(adj1[0].target, NodeId::new(2));

        // Node 2 has no outgoing edges
        assert_eq!(index.degree(NodeId::new(2)), 0);

        // Total edges
        assert_eq!(index.edge_count(), 3);
    }

    #[test]
    fn test_multiple_edge_labels() {
        let knows = GLOBAL_INTERNER.intern("KNOWS");
        let follows = GLOBAL_INTERNER.intern("FOLLOWS");

        let edges = vec![
            (NodeId::new(0), NodeId::new(1), EdgeId::new(0), knows),
            (NodeId::new(0), NodeId::new(2), EdgeId::new(1), follows),
            (NodeId::new(0), NodeId::new(3), EdgeId::new(2), knows),
        ];

        let index = AdjacencyIndex::build(edges);

        // Get all edges from node 0
        assert_eq!(index.degree(NodeId::new(0)), 3);

        // Get only KNOWS edges from node 0
        let knows_edges: Vec<_> = index
            .get_adjacency_with_label(NodeId::new(0), knows)
            .collect();
        assert_eq!(knows_edges.len(), 2);

        // Get only FOLLOWS edges from node 0
        let follows_edges: Vec<_> = index
            .get_adjacency_with_label(NodeId::new(0), follows)
            .collect();
        assert_eq!(follows_edges.len(), 1);
        assert_eq!(follows_edges[0].target, NodeId::new(2));
    }

    #[test]
    fn test_node_without_edges() {
        let knows = GLOBAL_INTERNER.intern("KNOWS");

        let edges = vec![(NodeId::new(0), NodeId::new(1), EdgeId::new(0), knows)];

        let index = AdjacencyIndex::build(edges);

        // Node 5 doesn't exist
        assert_eq!(index.degree(NodeId::new(5)), 0);
        assert!(!index.has_edges(NodeId::new(5)));
        assert_eq!(index.get_adjacency(NodeId::new(5)).len(), 0);
    }

    #[test]
    fn test_adjacency_entry() {
        let label = GLOBAL_INTERNER.intern("TEST");
        let entry = AdjacencyEntry::new(NodeId::new(1), EdgeId::new(100), label);

        assert_eq!(entry.target, NodeId::new(1));
        assert_eq!(entry.edge_id, EdgeId::new(100));
        assert_eq!(entry.label, label);
    }

    #[test]
    fn test_sorted_adjacency() {
        // Edges deliberately out of order
        let knows = GLOBAL_INTERNER.intern("KNOWS");
        let edges = vec![
            (NodeId::new(0), NodeId::new(3), EdgeId::new(2), knows),
            (NodeId::new(0), NodeId::new(1), EdgeId::new(0), knows),
            (NodeId::new(0), NodeId::new(2), EdgeId::new(1), knows),
        ];

        let index = AdjacencyIndex::build(edges);
        let adj = index.get_adjacency(NodeId::new(0));

        // Should be sorted by target
        assert_eq!(adj[0].target, NodeId::new(1));
        assert_eq!(adj[1].target, NodeId::new(2));
        assert_eq!(adj[2].target, NodeId::new(3));
    }
}
