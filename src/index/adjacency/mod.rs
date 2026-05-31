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
    /// The CSR structure is decomposed into three raw arrays suitable for fast
    /// binary serialization to disk. This is heavily utilized by the persistence
    /// engine to avoid serializing rust-specific enum wrappers or iterating over
    /// complex graphs.
    ///
    /// Returns a tuple `(node_ids, offsets, edge_ids)` where:
    /// - `node_ids`: Sorted array of `NodeId`s that have outgoing edges.
    /// - `offsets`: The CSR offset array defining edge boundaries per node.
    /// - `edge_ids`: Flat array of all outgoing `EdgeId`s in traversal order.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::core::id::{NodeId, EdgeId};
    /// use aletheiadb::index::AdjacencyIndex;
    /// use aletheiadb::core::interning::GLOBAL_INTERNER;
    ///
    /// let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    /// let edges = vec![
    ///     (NodeId::new(1).unwrap(), NodeId::new(2).unwrap(), EdgeId::new(100).unwrap(), label)
    /// ];
    ///
    /// let index = AdjacencyIndex::build(edges);
    /// let (nodes, offsets, edges_out) = index.export_csr();
    ///
    /// assert_eq!(nodes, vec![1]);
    /// assert_eq!(offsets, vec![0, 1]);
    /// assert_eq!(edges_out, vec![100]);
    /// ```
    pub fn export_csr(&self) -> (Vec<u64>, Vec<u64>, Vec<u64>) {
        let node_ids = self.node_ids.iter().map(|n| n.as_u64()).collect();
        let offsets = self.offsets.iter().map(|&x| x as u64).collect();
        let edge_ids = self.edges.iter().map(|e| e.edge_id.as_u64()).collect();
        (node_ids, offsets, edge_ids)
    }

    /// Import CSR data from persistence, reconstructing adjacency entries from edges.
    ///
    /// Re-hydrates a CSR structure from its raw binary components. This reconstructs
    /// the full `AdjacencyEntry` data by looking up the edge metadata (target, label)
    /// in the provided `edges_map`.
    ///
    /// This method is highly optimized and performs zero-copy vector transmutations
    /// where possible (such as on 64-bit systems converting `u64` to `usize`).
    ///
    /// # Arguments
    /// * `node_ids` - Sorted array of node IDs that have outgoing edges.
    /// * `offsets` - CSR offset array defining edge boundaries per node.
    /// * `edge_ids` - Flat array of edge IDs corresponding to the offsets.
    /// * `edges_map` - Map from `EdgeId` to `(target, label)` for full reconstruction.
    ///
    /// ## Panics
    ///
    /// Panics if the provided CSR invariants are violated (e.g., offsets array length mismatch,
    /// non-monotonic sequences, or invalid bounds) to prevent corrupted database state.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::core::id::{NodeId, EdgeId};
    /// use aletheiadb::index::AdjacencyIndex;
    /// use aletheiadb::core::interning::GLOBAL_INTERNER;
    /// use aletheiadb::core::hasher::IdentityHasher;
    /// use std::collections::HashMap;
    /// use std::hash::BuildHasherDefault;
    ///
    /// let nodes = vec![1];
    /// let offsets = vec![0, 1];
    /// let edge_ids = vec![100];
    ///
    /// let mut edge_map = HashMap::with_hasher(BuildHasherDefault::<IdentityHasher>::default());
    /// let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    /// edge_map.insert(EdgeId::new(100).unwrap(), (NodeId::new(2).unwrap(), label));
    ///
    /// let index = AdjacencyIndex::import_csr(nodes, offsets, edge_ids, &edge_map);
    /// assert_eq!(index.edge_count(), 1);
    /// ```
    pub fn import_csr(
        node_ids: Vec<u64>,
        offsets: Vec<u64>,
        edge_ids: Vec<u64>,
        edges_map: &std::collections::HashMap<
            EdgeId,
            (NodeId, InternedString),
            BuildHasherDefault<IdentityHasher>,
        >,
    ) -> Self {
        if offsets.is_empty() || edge_ids.is_empty() {
            return Self::new();
        }

        // Validate CSR invariants
        Self::validate_csr_invariants(&node_ids, &offsets, &edge_ids).unwrap();

        let max_node_id = node_ids.iter().max().copied().unwrap_or(0);

        // Zero-copy conversion: NodeId(u64) has same layout as u64
        let node_ids_typed: Vec<NodeId> = bytemuck::cast_vec(node_ids);

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
                    NodeId::new(0).unwrap(),
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
    ///
    /// Initializes an empty CSR structure that allocates no heap memory until
    /// edges are explicitly added via building or importing.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::index::AdjacencyIndex;
    ///
    /// let index = AdjacencyIndex::new();
    /// assert_eq!(index.node_count(), 0);
    /// assert_eq!(index.edge_count(), 0);
    /// ```
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
    /// Accepts a flat list of edges and dynamically constructs the sparse CSR representation.
    /// The input is automatically sorted in parallel by `(source, target, edge_id)` to ensure
    /// deterministic adjacency lists and correct offset calculation.
    ///
    /// Edges should be provided as `(source, target, edge_id, label)` tuples.
    ///
    /// This uses a sparse representation: only nodes with outgoing edges are stored. This makes
    /// it extremely memory efficient even with large gaps in node IDs (O(num_nodes) instead of O(max_node_id)).
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::core::id::{NodeId, EdgeId};
    /// use aletheiadb::index::AdjacencyIndex;
    /// use aletheiadb::core::interning::GLOBAL_INTERNER;
    ///
    /// let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    /// let edges = vec![
    ///     (NodeId::new(1).unwrap(), NodeId::new(2).unwrap(), EdgeId::new(100).unwrap(), label),
    ///     (NodeId::new(1).unwrap(), NodeId::new(3).unwrap(), EdgeId::new(101).unwrap(), label),
    /// ];
    ///
    /// let index = AdjacencyIndex::build(edges);
    /// assert_eq!(index.degree(NodeId::new(1).unwrap()), 2);
    /// ```
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
    /// Returns a sequential, cache-friendly slice of all outgoing edges originating
    /// from the specified node. Because the CSR edges are stored contiguously, iterating
    /// through this slice ensures near-zero cache misses during graph traversal.
    ///
    /// Returns an empty slice if the node has no outgoing edges.
    ///
    /// This uses a fast binary search over the sparse `node_ids` array to locate the node,
    /// providing O(log n) lookup time where `n` is the number of nodes with outgoing edges.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::core::id::{NodeId, EdgeId};
    /// use aletheiadb::index::AdjacencyIndex;
    /// use aletheiadb::core::interning::GLOBAL_INTERNER;
    ///
    /// let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    /// let edges = vec![
    ///     (NodeId::new(1).unwrap(), NodeId::new(2).unwrap(), EdgeId::new(100).unwrap(), label)
    /// ];
    ///
    /// let index = AdjacencyIndex::build(edges);
    /// let adj = index.get_adjacency(NodeId::new(1).unwrap());
    ///
    /// assert_eq!(adj.len(), 1);
    /// assert_eq!(adj[0].target, NodeId::new(2).unwrap());
    /// ```
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
    /// Performs an `O(log N) + O(E)` traversal where `N` is the number of nodes with
    /// outgoing edges, and `E` is the degree of the given node. It yields an iterator
    /// over only the adjacency entries that possess the specified `InternedString` label.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::core::id::{NodeId, EdgeId};
    /// use aletheiadb::index::AdjacencyIndex;
    /// use aletheiadb::core::interning::GLOBAL_INTERNER;
    ///
    /// let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    /// let follows = GLOBAL_INTERNER.intern("FOLLOWS").unwrap();
    /// let edges = vec![
    ///     (NodeId::new(1).unwrap(), NodeId::new(2).unwrap(), EdgeId::new(100).unwrap(), knows),
    ///     (NodeId::new(1).unwrap(), NodeId::new(3).unwrap(), EdgeId::new(101).unwrap(), follows),
    /// ];
    ///
    /// let index = AdjacencyIndex::build(edges);
    /// let knows_edges: Vec<_> = index.get_adjacency_with_label(NodeId::new(1).unwrap(), knows).collect();
    ///
    /// assert_eq!(knows_edges.len(), 1);
    /// assert_eq!(knows_edges[0].target, NodeId::new(2).unwrap());
    /// ```
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
    ///
    /// Determines how many edges originate from this node. This relies on the
    /// same O(log N) binary search as `get_adjacency` to calculate the slice length.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::core::id::{NodeId, EdgeId};
    /// use aletheiadb::index::AdjacencyIndex;
    /// use aletheiadb::core::interning::GLOBAL_INTERNER;
    ///
    /// let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    /// let edges = vec![
    ///     (NodeId::new(1).unwrap(), NodeId::new(2).unwrap(), EdgeId::new(100).unwrap(), label)
    /// ];
    ///
    /// let index = AdjacencyIndex::build(edges);
    /// assert_eq!(index.degree(NodeId::new(1).unwrap()), 1);
    /// assert_eq!(index.degree(NodeId::new(99).unwrap()), 0);
    /// ```
    #[inline]
    pub fn degree(&self, node: NodeId) -> usize {
        self.get_adjacency(node).len()
    }

    /// Check if a node has any outgoing edges.
    ///
    /// Fast boolean check to see if traversing out of this node is possible.
    /// This is equivalent to `degree(node) > 0`.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::core::id::{NodeId, EdgeId};
    /// use aletheiadb::index::AdjacencyIndex;
    /// use aletheiadb::core::interning::GLOBAL_INTERNER;
    ///
    /// let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    /// let edges = vec![
    ///     (NodeId::new(1).unwrap(), NodeId::new(2).unwrap(), EdgeId::new(100).unwrap(), label)
    /// ];
    ///
    /// let index = AdjacencyIndex::build(edges);
    /// assert!(index.has_edges(NodeId::new(1).unwrap()));
    /// assert!(!index.has_edges(NodeId::new(99).unwrap()));
    /// ```
    #[inline]
    pub fn has_edges(&self, node: NodeId) -> bool {
        self.degree(node) > 0
    }

    /// Get total number of edges in the index.
    ///
    /// Returns the exact size of the underlying flat CSR edges array.
    /// Because the array is flat, this is an O(1) operation.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::index::AdjacencyIndex;
    ///
    /// let index = AdjacencyIndex::new();
    /// assert_eq!(index.edge_count(), 0);
    /// ```
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Get the maximum node ID in this index.
    ///
    /// Returns the highest numerical `NodeId` encountered across both edge
    /// sources and targets during construction. This is an O(1) operation
    /// heavily utilized by the execution engine for query bounds checking.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::core::id::{NodeId, EdgeId};
    /// use aletheiadb::index::AdjacencyIndex;
    /// use aletheiadb::core::interning::GLOBAL_INTERNER;
    ///
    /// let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    /// let edges = vec![
    ///     (NodeId::new(1).unwrap(), NodeId::new(500).unwrap(), EdgeId::new(100).unwrap(), label)
    /// ];
    ///
    /// let index = AdjacencyIndex::build(edges);
    /// assert_eq!(index.max_node_id(), 500);
    /// ```
    #[inline]
    pub fn max_node_id(&self) -> u64 {
        self.max_node_id
    }

    /// Iterate over all nodes that have outgoing edges.
    ///
    /// Yields a stream of `NodeId`s representing every unique source node in the graph.
    ///
    /// This is extremely efficient for sparse graphs as it only yields nodes
    /// that actually have outgoing edges, completely bypassing "gaps" or deleted nodes
    /// that would otherwise be traversed if checking `0..max_node_id`.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::core::id::{NodeId, EdgeId};
    /// use aletheiadb::index::AdjacencyIndex;
    /// use aletheiadb::core::interning::GLOBAL_INTERNER;
    ///
    /// let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    /// let edges = vec![
    ///     (NodeId::new(10).unwrap(), NodeId::new(20).unwrap(), EdgeId::new(100).unwrap(), label),
    ///     (NodeId::new(99).unwrap(), NodeId::new(20).unwrap(), EdgeId::new(101).unwrap(), label),
    /// ];
    ///
    /// let index = AdjacencyIndex::build(edges);
    /// let nodes: Vec<_> = index.iter_nodes().collect();
    ///
    /// // Only nodes with outgoing edges are yielded!
    /// assert_eq!(nodes, vec![NodeId::new(10).unwrap(), NodeId::new(99).unwrap()]);
    /// ```
    #[inline]
    pub fn iter_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.node_ids.iter().copied()
    }

    /// Get the number of nodes with outgoing edges.
    ///
    /// Returns the exact size of the underlying CSR `node_ids` array.
    /// Because the array only stores source nodes, this reflects the number
    /// of unique nodes with an out-degree > 0. This is an O(1) operation.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::core::id::{NodeId, EdgeId};
    /// use aletheiadb::index::AdjacencyIndex;
    /// use aletheiadb::core::interning::GLOBAL_INTERNER;
    ///
    /// let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    /// let edges = vec![
    ///     (NodeId::new(1).unwrap(), NodeId::new(2).unwrap(), EdgeId::new(100).unwrap(), label),
    ///     (NodeId::new(1).unwrap(), NodeId::new(3).unwrap(), EdgeId::new(101).unwrap(), label),
    /// ];
    ///
    /// let index = AdjacencyIndex::build(edges);
    /// // Even though there are 2 edges and 3 total unique nodes involved,
    /// // only 1 node is a source node!
    /// assert_eq!(index.node_count(), 1);
    /// ```
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

        #[allow(clippy::collapsible_if)]
        if let Some(&first_offset) = offsets.first() {
            if first_offset != 0 {
                return Err(format!(
                    "CSR first offset mismatch: expected 0, got {}",
                    first_offset
                ));
            }
        }

        for window in offsets.windows(2) {
            if window[0] > window[1] {
                return Err(format!(
                    "CSR offsets are not monotonically increasing: {} > {}",
                    window[0], window[1]
                ));
            }
        }

        for window in node_ids.windows(2) {
            if window[0] >= window[1] {
                return Err(format!(
                    "CSR node_ids are not strictly monotonically increasing: {} >= {}",
                    window[0], window[1]
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

        Ok(())
    }

    /// Convert offsets to usize vector.
    ///
    /// On 64-bit systems, this is a zero-copy operation because usize == u64.
    /// On 32-bit systems, this allocates a new vector because usize == u32 != u64.
    fn convert_offsets(offsets: Vec<u64>) -> Vec<usize> {
        #[cfg(target_pointer_width = "64")]
        {
            bytemuck::cast_vec(offsets)
        }

        #[cfg(not(target_pointer_width = "64"))]
        {
            offsets.iter().map(|&x| x as usize).collect()
        }
    }
}

impl Default for AdjacencyIndex {
    fn default() -> Self {
        Self::new()
    }
}


#[cfg(test)]
mod tests;

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

        // 4. Invalid first offset
        let invalid_first_offset = vec![1, 1, 2];
        let err_first =
            AdjacencyIndex::validate_csr_invariants(&node_ids, &invalid_first_offset, &edge_ids)
                .unwrap_err();
        assert!(err_first.contains("CSR first offset mismatch"));

        // 5. Non-monotonic offsets
        let non_monotonic_offsets = vec![0, 2, 1]; // 2 > 1
        // We need 3 edge ids to match the last offset 1, or wait, last offset is 1, so edge len = 1
        let err_monotonic =
            AdjacencyIndex::validate_csr_invariants(&node_ids, &non_monotonic_offsets, &[100])
                .unwrap_err();
        assert!(err_monotonic.contains("CSR offsets are not monotonically increasing"));

        // 6. Unsorted node ids
        let unsorted_node_ids = vec![20, 10]; // unsorted
        let err_unsorted =
            AdjacencyIndex::validate_csr_invariants(&unsorted_node_ids, &offsets, &edge_ids)
                .unwrap_err();
        assert!(err_unsorted.contains("CSR node_ids are not strictly monotonically increasing"));

        // 7. Duplicate node ids
        let duplicate_node_ids = vec![10, 10]; // duplicate
        let err_duplicate =
            AdjacencyIndex::validate_csr_invariants(&duplicate_node_ids, &offsets, &edge_ids)
                .unwrap_err();
        assert!(err_duplicate.contains("CSR node_ids are not strictly monotonically increasing"));
    }

    #[test]
    #[should_panic(expected = "CSR offsets length mismatch")]
    fn test_import_csr_panics_on_invalid() {
        // Integration check: ensure import_csr actually calls validate and panics
        let node_ids = vec![10];
        let offsets = vec![0]; // invalid len (should be 2)
        let edge_ids = vec![100]; // Non-empty to bypass early return
        let edges_map = HashMap::with_hasher(BuildHasherDefault::<IdentityHasher>::default());
        AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map);
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
        let index = AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map);

        assert_eq!(index.edge_count(), 2);
        assert_eq!(index.node_count(), 2);
        assert_eq!(index.max_node_id(), 20);
    }
}
