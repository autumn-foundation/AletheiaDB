//! CSR (Compressed Sparse Row) adjacency index for graph traversals.
//!
//! This module implements a cache-friendly adjacency list representation using
//! the Compressed Sparse Row format. This enables fast sequential access during
//! graph traversals with minimal cache misses.
//!
//! # CSR Format
//!
//! The CSR format stores the graph as two arrays:
//! - `offsets` where `offsets[i]` is the starting position in `edges` for node i's adjacency list
//! - `edges`: Flat array of (target_node, edge_id, edge_label) tuples
//!
//! This layout is cache-friendly because traversing from a node requires
//! sequential access to a contiguous region of memory.

use crate::core::hasher::IdentityHasher;
use crate::core::id::{EdgeId, NodeId};
use crate::core::interning::InternedString;
use rayon::prelude::*;
use std::hash::BuildHasherDefault;

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
    /// `offsets[i]` = start index in edges array for `node_ids[i]`
    /// `offsets[i + 1]` = end index (exclusive)
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
    ///
    /// # Errors
    /// Returns an error if the input data violates CSR invariants (e.g. unsorted nodes,
    /// non-monotonic offsets, or bounds violations).
    pub fn import_csr(
        node_ids: Vec<u64>,
        offsets: Vec<u64>,
        edge_ids: Vec<u64>,
        edges_map: &std::collections::HashMap<
            EdgeId,
            (NodeId, InternedString),
            BuildHasherDefault<IdentityHasher>,
        >,
    ) -> Result<Self, String> {
        if offsets.is_empty() || edge_ids.is_empty() {
            return Ok(Self::new());
        }

        // Validate CSR invariants strictly
        Self::validate_csr_invariants(&node_ids, &offsets, &edge_ids)?;

        let max_node_id = node_ids.iter().max().copied().unwrap_or(0);

        // Zero-copy conversion: NodeId(u64) has same layout as u64
        // SAFETY: NodeId is #[repr(transparent)] wrapper around u64.
        let node_ids_typed: Vec<NodeId> = unsafe { Self::transmute_vec(node_ids) };

        // Convert offsets (zero-copy on 64-bit, allocating on 32-bit)
        let offsets_usize = Self::convert_offsets(offsets);

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

        Ok(Self {
            node_ids: node_ids_typed,
            offsets: offsets_usize,
            edges: adjacency_entries,
            max_node_id,
        })
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
        // We use parallel sort for performance on large graphs.
        // We include edge_id for canonical deterministic ordering.
        edges.par_sort_unstable_by_key(|(src, target, edge_id, _)| (*src, *target, *edge_id));

        // Pre-allocate assuming some average degree > 1 to avoid resizing
        let estimated_nodes = (edge_count / 4).max(16);
        let mut node_ids = Vec::with_capacity(estimated_nodes);
        let mut offsets = Vec::with_capacity(estimated_nodes + 1);
        let mut flat_edges = Vec::with_capacity(edge_count);
        let mut max_node_id = 0;

        offsets.push(0);

        if !edges.is_empty() {
            let mut current_source = edges[0].0;
            node_ids.push(current_source);

            for (source, target, edge_id, label) in edges {
                let src_val = source.as_u64();
                let tgt_val = target.as_u64();
                if src_val > max_node_id {
                    max_node_id = src_val;
                }
                if tgt_val > max_node_id {
                    max_node_id = tgt_val;
                }

                if source != current_source {
                    offsets.push(flat_edges.len());
                    current_source = source;
                    node_ids.push(current_source);
                }
                flat_edges.push(AdjacencyEntry::new(target, edge_id, label));
            }
            offsets.push(flat_edges.len());
        }

        // Optimize memory usage by releasing unused capacity
        node_ids.shrink_to_fit();
        offsets.shrink_to_fit();

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

    /// Validate CSR invariants.
    fn validate_csr_invariants(
        node_ids: &[u64],
        offsets: &[u64],
        edge_ids: &[u64],
    ) -> Result<(), String> {
        if offsets.len() != node_ids.len() + 1 {
            return Err(format!(
                "CSR offsets length mismatch: expected {}, got {}",
                node_ids.len() + 1,
                offsets.len()
            ));
        }

        // Validate monotonicity and bounds of offsets
        // offsets[i] <= offsets[i+1] and offsets[i] <= edge_ids.len()
        for i in 0..offsets.len() - 1 {
            if offsets[i] > offsets[i + 1] {
                return Err(format!(
                    "CSR offsets not monotonic: offsets[{}]={} > offsets[{}]={}",
                    i,
                    offsets[i],
                    i + 1,
                    offsets[i + 1]
                ));
            }
        }

        #[allow(clippy::collapsible_if)]
        if let Some(&last_offset) = offsets.last() {
            if last_offset != edge_ids.len() as u64 {
                return Err(format!(
                    "CSR last offset mismatch: expected {}, got {}",
                    edge_ids.len(),
                    last_offset
                ));
            }
        }

        // Validate node_ids are sorted
        for i in 0..node_ids.len().saturating_sub(1) {
            if node_ids[i] >= node_ids[i + 1] {
                return Err(format!(
                    "CSR node_ids not sorted or duplicate: node_ids[{}]={} >= node_ids[{}]={}",
                    i,
                    node_ids[i],
                    i + 1,
                    node_ids[i + 1]
                ));
            }
        }

        Ok(())
    }

    /// Convert offsets to usize vector.
    ///
    /// On 64-bit systems, this is a zero-copy operation because usize == u64.
    /// On 32-bit systems, this allocates a new vector because usize == u32 != u64.
    fn convert_offsets(offsets: Vec<u64>) -> Vec<usize> {
        #[cfg(target_pointer_width = "64")]
        {
            // SAFETY: usize == u64 on 64-bit platforms, so layout is compatible.
            unsafe { Self::transmute_vec(offsets) }
        }

        #[cfg(not(target_pointer_width = "64"))]
        {
            offsets.iter().map(|&x| x as usize).collect()
        }
    }

    /// Helper for zero-copy Vec conversion
    ///
    /// # Safety
    /// T and U must have same layout, size, and alignment.
    unsafe fn transmute_vec<T, U>(v: Vec<T>) -> Vec<U> {
        let mut v = std::mem::ManuallyDrop::new(v);
        // SAFETY: Caller ensures T and U layout compatibility.
        // from_raw_parts is unsafe, but we are inside an unsafe function.
        // In Rust 2024 (and newer editions), unsafe blocks are required inside unsafe functions.
        unsafe { Vec::from_raw_parts(v.as_mut_ptr() as *mut U, v.len(), v.capacity()) }
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

    #[test]
    fn test_transmute_vec_correctness() {
        let original = vec![1u64, 2, 3];
        let ptr = original.as_ptr();
        let cap = original.capacity();

        // Use NodeId which is transparent wrapper around u64
        // SAFETY: NodeId is repr(transparent) and same size/align as u64
        let transmuted: Vec<NodeId> = unsafe { AdjacencyIndex::transmute_vec(original) };

        assert_eq!(transmuted.len(), 3);
        assert_eq!(transmuted.capacity(), cap);
        assert_eq!(transmuted[0], NodeId::new(1).unwrap());
        assert_eq!(transmuted[1], NodeId::new(2).unwrap());
        assert_eq!(transmuted[2], NodeId::new(3).unwrap());

        // Verify no copy happened (best effort check, pointers should match)
        assert_eq!(transmuted.as_ptr() as *const u64, ptr);
    }

    #[test]
    fn test_import_csr_integration() {
        // This test ensures import_csr works in the standard test module scope
        let node_ids = vec![1, 2];
        let offsets = vec![0, 1, 2];
        let edge_ids = vec![10, 20];
        let mut edges_map =
            std::collections::HashMap::with_hasher(std::hash::BuildHasherDefault::<
                crate::core::hasher::IdentityHasher,
            >::default());

        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("TEST")
            .unwrap();
        edges_map.insert(EdgeId::new(10).unwrap(), (NodeId::new(2).unwrap(), label));
        edges_map.insert(EdgeId::new(20).unwrap(), (NodeId::new(1).unwrap(), label));

        let index = AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map).unwrap();
        assert_eq!(index.node_count(), 2);
        assert_eq!(index.edge_count(), 2);
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;
    use std::collections::HashMap;
    use std::hash::BuildHasherDefault;

    #[test]
    fn test_validate_csr_invariants_logic() {
        // 1. Valid case
        let node_ids = vec![10, 20];
        let offsets = vec![0, 1, 2];
        let edge_ids = vec![100, 101];
        assert!(AdjacencyIndex::validate_csr_invariants(&node_ids, &offsets, &edge_ids).is_ok());

        // 2. Invalid offsets length
        let invalid_offsets_len = vec![0, 1]; // too short
        let err_len =
            AdjacencyIndex::validate_csr_invariants(&node_ids, &invalid_offsets_len, &edge_ids)
                .unwrap_err();
        assert!(err_len.contains("CSR offsets length mismatch"));

        // 3. Invalid last offset
        let invalid_offsets_val = vec![0, 1, 5]; // last is 5, but edges len is 2
        let err_val =
            AdjacencyIndex::validate_csr_invariants(&node_ids, &invalid_offsets_val, &edge_ids)
                .unwrap_err();
        assert!(err_val.contains("CSR last offset mismatch"));

        // 4. Non-monotonic offsets
        let non_monotonic = vec![0, 2, 1];
        let err_mono = AdjacencyIndex::validate_csr_invariants(&node_ids, &non_monotonic, &edge_ids)
            .unwrap_err();
        assert!(err_mono.contains("CSR offsets not monotonic"));

        // 5. Unsorted node_ids
        let unsorted_nodes = vec![20, 10];
        let err_unsorted =
            AdjacencyIndex::validate_csr_invariants(&unsorted_nodes, &offsets, &edge_ids)
                .unwrap_err();
        assert!(err_unsorted.contains("CSR node_ids not sorted"));
    }

    #[test]
    fn test_import_csr_errors_on_invalid() {
        // Integration check: ensure import_csr actually calls validate and returns error
        let node_ids = vec![10];
        let offsets = vec![0]; // invalid len (should be 2)
        let edge_ids = vec![100]; // Non-empty to bypass early return
        let edges_map = HashMap::with_hasher(BuildHasherDefault::<IdentityHasher>::default());
        let res = AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("CSR offsets length mismatch"));
    }

    #[test]
    fn test_import_csr_success() {
        // Multi-node case to fully exercise loop and max_node_id logic
        let node_ids: Vec<u64> = vec![10, 20];
        // Offsets: node 10 has 1 edge (0..1), node 20 has 1 edge (1..2)
        let offsets: Vec<u64> = vec![0, 1, 2];
        let edge_ids: Vec<u64> = vec![100, 101];

        let mut edges_map = HashMap::with_hasher(BuildHasherDefault::<IdentityHasher>::default());
        let target = NodeId::new(99).unwrap();
        let label = crate::core::interning::InternedString::from_raw(1);

        edges_map.insert(EdgeId::new(100).unwrap(), (target, label));
        edges_map.insert(EdgeId::new(101).unwrap(), (target, label));

        // Should not panic
        let index = AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map).unwrap();

        assert_eq!(index.edge_count(), 2);
        assert_eq!(index.node_count(), 2);
        assert_eq!(index.max_node_id(), 20);
    }
}
