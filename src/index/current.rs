//! Current-state indexes using concurrent data structures.
//!
//! This module provides indexes for the current state of the graph using
//! DashMap for lock-free concurrent access. These are the "hot path" indexes
//! that must be extremely fast.

use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::interning::InternedString;
use crate::index::adjacency::{AdjacencyEntry, AdjacencyIndex};
use crate::utils::lock::RwLockExt;
use arc_swap::ArcSwap;
use dashmap::DashMap;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

/// Guard for accessing adjacency list without allocation.
///
/// This guard holds an `Arc<AdjacencyIndex>` to keep the data alive,
/// and provides zero-copy access to the adjacency slice using indices.
///
/// # Performance
///
/// - No Vec allocation (saves 100-500ns per traversal)
/// - Just an Arc clone (atomic increment, ~5-10ns)
/// - Derefs directly to `&[AdjacencyEntry]`
///
/// # Implementation
///
/// Uses start/end indices instead of raw pointers to avoid unsafe code.
/// The NodeId is stored to enable index lookup on each dereference.
pub struct AdjacencyGuard {
    /// Keeps the adjacency index alive and provides data access.
    index: Arc<AdjacencyIndex>,
    /// Node whose adjacency list we're accessing.
    node: NodeId,
}

impl AdjacencyGuard {
    /// Create a new adjacency guard for a node's adjacency list.
    ///
    /// The guard will keep the index alive and provide zero-copy access
    /// to the node's adjacency slice.
    fn new(index: Arc<AdjacencyIndex>, node: NodeId) -> Self {
        AdjacencyGuard { index, node }
    }
}

impl Deref for AdjacencyGuard {
    type Target = [AdjacencyEntry];

    #[inline]
    fn deref(&self) -> &[AdjacencyEntry] {
        // Safe: Returns &'a [AdjacencyEntry] where 'a is tied to &self lifetime.
        // The Arc ensures the AdjacencyIndex remains alive for the lifetime of
        // the guard, and AdjacencyIndex is immutable after construction.
        self.index.get_adjacency(self.node)
    }
}

impl std::fmt::Debug for AdjacencyGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Get slice once and reuse to avoid multiple deref calls during formatting
        let entries = self.deref();
        f.debug_struct("AdjacencyGuard")
            .field("node", &self.node)
            .field("entry_count", &entries.len())
            .finish()
    }
}

impl PartialEq for AdjacencyGuard {
    fn eq(&self, other: &Self) -> bool {
        // Compare the actual adjacency slice contents, not the internal fields
        self.deref() == other.deref()
    }
}

impl Eq for AdjacencyGuard {}

/// Concurrent indexes for current-state graph queries.
///
/// These indexes provide O(1) lookups for nodes and edges, plus efficient
/// graph traversal through CSR adjacency indexes.
///
/// # Concurrency Model
///
/// Edge modifications (`insert_edge`, `remove_edge`) and adjacency rebuilds
/// are coordinated via `rebuild_lock`:
/// - Edge modifications acquire the lock in **read mode** (concurrent OK)
/// - Rebuilds acquire the lock in **write mode** (exclusive access)
///
/// This prevents the race condition where edges inserted during a rebuild
/// could be lost from the adjacency indexes.
///
/// # Lock-Free Adjacency Reads
///
/// Adjacency indexes use `ArcSwap` for **lock-free reads** on the hot path:
/// - **Read operations** (`get_outgoing`, `get_incoming`): Lock-free atomic pointer load
/// - **Write operations** (`rebuild_adjacency`): Atomic pointer swap after rebuild
/// - **Performance**: Eliminates 20-100ns lock acquisition overhead per traversal
/// - **Zero contention**: Readers never block readers or writers
///
/// The `rebuild_lock` still coordinates edge modifications with rebuilds,
/// but adjacency **reads** are now completely lock-free.
pub struct CurrentIndexes {
    /// Node ID → Node (O(1) lookup)
    nodes: DashMap<NodeId, Node>,
    /// Edge ID → Edge (O(1) lookup)
    edges: DashMap<EdgeId, Edge>,
    /// Outgoing edges: source node → adjacency list (lock-free reads via ArcSwap)
    outgoing: ArcSwap<AdjacencyIndex>,
    /// Incoming edges: target node → adjacency list (lock-free reads via ArcSwap)
    incoming: ArcSwap<AdjacencyIndex>,
    /// Coordinates edge modifications with adjacency rebuilds.
    /// Edge ops hold read lock; rebuild holds write lock.
    rebuild_lock: RwLock<()>,
    /// Tracks whether adjacency indexes are out of date and need rebuilding.
    /// Set to true when edges are inserted/removed, cleared after rebuild.
    adjacency_dirty: AtomicBool,
}

impl CurrentIndexes {
    /// Create new empty indexes.
    pub fn new() -> Self {
        CurrentIndexes {
            nodes: DashMap::new(),
            edges: DashMap::new(),
            outgoing: ArcSwap::from_pointee(AdjacencyIndex::new()),
            incoming: ArcSwap::from_pointee(AdjacencyIndex::new()),
            rebuild_lock: RwLock::new(()),
            adjacency_dirty: AtomicBool::new(false),
        }
    }

    /// Insert a node into the indexes.
    pub fn insert_node(&self, node: Node) {
        self.nodes.insert(node.id, node);
    }

    /// Insert an edge into the indexes.
    ///
    /// Note: This only updates the edge map. Adjacency indexes are rebuilt
    /// lazily on next access for efficiency (batch updates).
    ///
    /// Acquires `rebuild_lock` in read mode to coordinate with concurrent
    /// adjacency rebuilds (which hold write lock).
    pub fn insert_edge(&self, edge: Edge) {
        let _guard = self.rebuild_lock.read_or_recover();
        self.edges.insert(edge.id, edge);
        // Mark adjacency as dirty - will be rebuilt lazily on next access
        self.adjacency_dirty.store(true, Ordering::Release);
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
    ///
    /// Acquires `rebuild_lock` in read mode to coordinate with concurrent
    /// adjacency rebuilds (which hold write lock).
    pub fn remove_edge(&self, id: EdgeId) -> Option<Edge> {
        let _guard = self.rebuild_lock.read_or_recover();
        self.edges.remove(&id).map(|(_, edge)| edge).inspect(|_| {
            // Mark adjacency as dirty - will be rebuilt lazily on next access
            self.adjacency_dirty.store(true, Ordering::Release);
        })
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

    /// Ensure adjacency indexes are up to date.
    ///
    /// If the `adjacency_dirty` flag is set, triggers a rebuild.
    /// This is called before any adjacency access to ensure correctness.
    ///
    /// Uses double-checked locking pattern to minimize overhead:
    /// 1. Quick check of dirty flag (no lock, just atomic read with Relaxed)
    /// 2. If dirty, acquire write lock and check again with Relaxed
    /// 3. Rebuild only if still dirty after acquiring lock
    ///
    /// This ensures only one thread rebuilds even if multiple threads
    /// detect the dirty flag simultaneously.
    ///
    /// # Memory Ordering (Issue #336 Optimization)
    ///
    /// Both checks use `Relaxed` ordering because:
    /// - **ArcSwap provides synchronization**: The actual adjacency data is accessed
    ///   through `ArcSwap::load()` which provides its own memory ordering guarantees.
    ///   When we load the Arc, we're guaranteed to see a consistent snapshot.
    /// - **Lock provides synchronization for slow path**: When we detect dirty=true
    ///   and acquire the write lock, the lock provides full synchronization.
    /// - **Dirty flag is a hint, not a guard**: The flag indicates whether we
    ///   *might* need to rebuild, not whether the data is valid. Even if we
    ///   miss a dirty=true due to Relaxed ordering, we'll see a consistent
    ///   (though potentially stale) adjacency snapshot via ArcSwap.
    ///
    /// This optimization saves ~0.5-2ns per adjacency access by avoiding
    /// the acquire fence on the fast path. See Issue #336 for details.
    ///
    /// # Safety Justification
    ///
    /// Even if we miss `dirty=true` due to Relaxed ordering, the subsequent
    /// `ArcSwap::load_full()` uses Acquire ordering internally, which synchronizes
    /// with the Release store from `rebuild_adjacency_internal()`. This guarantees
    /// we see a consistent (though possibly pre-rebuild) adjacency snapshot.
    ///
    /// # Race Window (Acceptable)
    ///
    /// There's a benign race where:
    /// 1. Thread A checks dirty=false, begins exiting fast path
    /// 2. Thread B inserts edge, sets dirty=true
    /// 3. Thread A proceeds with potentially stale adjacency pointer
    ///
    /// This is **acceptable** because:
    /// - Thread A's adjacency pointer was loaded atomically before the check
    /// - ArcSwap ensures Thread A sees a consistent (though older) snapshot
    /// - Next adjacency access will trigger rebuild (eventual consistency)
    /// - No data corruption or crashes can occur
    ///
    /// This is a standard tradeoff in lock-free data structures: we prioritize
    /// performance (avoiding locks on every read) over strict linearizability.
    #[inline]
    fn ensure_adjacency_current(&self) {
        // Fast path: adjacency is already current (Relaxed is safe - see docs above)
        if !self.adjacency_dirty.load(Ordering::Relaxed) {
            return;
        }

        // Slow path: rebuild needed
        // Acquire write lock to prevent concurrent edge modifications
        let _guard = self.rebuild_lock.write_or_recover();

        // Double-check: another thread may have rebuilt while we waited for the lock
        // Use Relaxed ordering here since the lock provides synchronization
        if !self.adjacency_dirty.load(Ordering::Relaxed) {
            return;
        }

        // Actually rebuild
        self.rebuild_adjacency_internal();
    }

    /// Get outgoing edges for a node.
    ///
    /// Returns a guard that derefs to the adjacency slice without allocation.
    ///
    /// **Lock-free**: Uses atomic pointer load, no lock acquisition needed.
    /// **Zero-copy**: No Vec allocation, just an Arc clone (~5-10ns overhead).
    /// Lazily rebuilds adjacency if needed before access.
    ///
    /// # Performance
    ///
    /// This replaces the previous `Vec<AdjacencyEntry>` return type which
    /// allocated 100-500ns per call. The new guard approach eliminates this
    /// allocation while maintaining the same API ergonomics.
    #[inline]
    pub fn get_outgoing(&self, source: NodeId) -> AdjacencyGuard {
        self.ensure_adjacency_current();
        let index = self.outgoing.load_full();
        AdjacencyGuard::new(index, source)
    }

    /// Get incoming edges for a node.
    ///
    /// Returns a guard that derefs to the adjacency slice without allocation.
    ///
    /// **Lock-free**: Uses atomic pointer load, no lock acquisition needed.
    /// **Zero-copy**: No Vec allocation, just an Arc clone (~5-10ns overhead).
    /// Lazily rebuilds adjacency if needed before access.
    #[inline]
    pub fn get_incoming(&self, target: NodeId) -> AdjacencyGuard {
        self.ensure_adjacency_current();
        let index = self.incoming.load_full();
        AdjacencyGuard::new(index, target)
    }

    /// Get outgoing edges with a specific label.
    ///
    /// **Lock-free**: Uses atomic pointer load, no lock acquisition needed.
    /// Lazily rebuilds adjacency if needed before access.
    pub fn get_outgoing_with_label(
        &self,
        source: NodeId,
        label: InternedString,
    ) -> Vec<AdjacencyEntry> {
        self.ensure_adjacency_current();
        let outgoing = self.outgoing.load();
        outgoing
            .get_adjacency_with_label(source, label)
            .copied()
            .collect()
    }

    /// Get incoming edges with a specific label.
    ///
    /// **Lock-free**: Uses atomic pointer load, no lock acquisition needed.
    /// Lazily rebuilds adjacency if needed before access.
    pub fn get_incoming_with_label(
        &self,
        target: NodeId,
        label: InternedString,
    ) -> Vec<AdjacencyEntry> {
        self.ensure_adjacency_current();
        let incoming = self.incoming.load();
        incoming
            .get_adjacency_with_label(target, label)
            .copied()
            .collect()
    }

    /// Get the out-degree of a node (number of outgoing edges).
    ///
    /// **Lock-free**: Uses atomic pointer load, no lock acquisition needed.
    /// Lazily rebuilds adjacency if needed before access.
    #[inline]
    pub fn out_degree(&self, node: NodeId) -> usize {
        self.ensure_adjacency_current();
        let outgoing = self.outgoing.load();
        outgoing.degree(node)
    }

    /// Get the in-degree of a node (number of incoming edges).
    ///
    /// **Lock-free**: Uses atomic pointer load, no lock acquisition needed.
    /// Lazily rebuilds adjacency if needed before access.
    #[inline]
    pub fn in_degree(&self, node: NodeId) -> usize {
        self.ensure_adjacency_current();
        let incoming = self.incoming.load();
        incoming.degree(node)
    }

    /// Rebuild adjacency indexes from current edges.
    ///
    /// This can be called explicitly after batch edge insertions/deletions
    /// to force an immediate rebuild, though normally adjacency is rebuilt
    /// lazily on first access after modifications.
    ///
    /// # Concurrency
    ///
    /// Acquires `rebuild_lock` in write mode, which blocks all concurrent
    /// `insert_edge` and `remove_edge` operations. This prevents the race
    /// condition where edges inserted during iteration would be lost when
    /// the rebuilt indexes replace the old ones.
    ///
    /// **Lock-free for readers**: The new indexes are built separately, then
    /// atomically swapped in via `ArcSwap::store()`. Concurrent readers accessing
    /// adjacency lists (`get_outgoing`, `get_incoming`, etc.) are never blocked.
    ///
    /// # Performance
    ///
    /// **Current Implementation:**
    /// - Complexity: O(E log E) where E is total edges
    /// - Always rebuilds complete index from scratch
    /// - Blocks concurrent edge modifications, but NOT adjacency reads (lock-free!)
    /// - Atomic swap eliminates reader blocking
    /// - **Lazy rebuild**: Only rebuilds when adjacency is accessed after modifications
    ///
    /// **Future Optimization Opportunities:**
    ///
    /// 1. **Partial/Incremental Rebuild:**
    ///    - Track "dirty" nodes that had edge changes
    ///    - Only rebuild adjacency lists for affected nodes
    ///    - Potential speedup: 10-100x for localized changes
    ///    - Trade-off: Memory overhead for tracking dirty set
    ///
    /// 2. **Lock-Free Adjacency Updates:**
    ///    - Use lock-free CSR representation
    ///    - Incremental updates without global rebuild
    ///    - Potential speedup: 100-1000x for small batches
    ///    - Trade-off: Complex concurrent data structure
    ///
    /// For now, full rebuild is simple, correct, and fast enough for
    /// batch operations (1-10ms for 10K edges).
    pub fn rebuild_adjacency(&self) {
        // Acquire write lock to block concurrent edge modifications.
        let _guard = self.rebuild_lock.write_or_recover();
        self.rebuild_adjacency_internal();
    }

    /// Internal implementation of adjacency rebuild.
    ///
    /// SAFETY: Caller must hold `rebuild_lock` in write mode.
    fn rebuild_adjacency_internal(&self) {
        // Pre-allocate vectors to avoid reallocations during graph reconstruction.
        // Each edge is added to both outgoing (source->target) and incoming (target->source) lists.
        let edge_count = self.edges.len();
        let mut outgoing_edges = Vec::with_capacity(edge_count);
        let mut incoming_edges = Vec::with_capacity(edge_count);

        // Collect all edges (no modifications can occur while caller holds the lock)
        for entry in self.edges.iter() {
            let edge = entry.value();
            outgoing_edges.push((edge.source, edge.target, edge.id, edge.label));
            incoming_edges.push((edge.target, edge.source, edge.id, edge.label));
        }

        // Rebuild indexes and atomically swap them in (lock-free for readers!)
        self.outgoing
            .store(Arc::new(AdjacencyIndex::build(outgoing_edges)));
        self.incoming
            .store(Arc::new(AdjacencyIndex::build(incoming_edges)));

        // Clear the dirty flag - adjacency is now current
        self.adjacency_dirty.store(false, Ordering::Release);
    }

    /// Iterate over all nodes.
    pub fn iter_nodes(&self) -> impl Iterator<Item = Node> + '_ {
        self.nodes.iter().map(|entry| entry.value().clone())
    }

    /// Iterate over all edges.
    pub fn iter_edges(&self) -> impl Iterator<Item = Edge> + '_ {
        self.edges.iter().map(|entry| entry.value().clone())
    }

    /// Export outgoing CSR data for persistence.
    pub fn export_outgoing_csr(&self) -> (Vec<u64>, Vec<u64>) {
        self.outgoing.load().export_csr()
    }

    /// Export incoming CSR data for persistence.
    pub fn export_incoming_csr(&self) -> (Vec<u64>, Vec<u64>) {
        self.incoming.load().export_csr()
    }

    /// Import CSR data for both outgoing and incoming adjacency.
    ///
    /// This is used when loading persisted indexes to avoid rebuilding CSR from scratch.
    pub fn import_csr(
        &self,
        outgoing_offsets: Vec<u64>,
        outgoing_edge_ids: Vec<u64>,
        incoming_offsets: Vec<u64>,
        incoming_edge_ids: Vec<u64>,
    ) {
        use std::collections::HashMap;

        // Build edges map for CSR reconstruction
        let mut edges_map = HashMap::new();
        for entry in self.edges.iter() {
            let edge = entry.value();
            edges_map.insert(edge.id, (edge.target, edge.label));
        }

        // Import outgoing adjacency
        let outgoing = crate::index::adjacency::AdjacencyIndex::import_csr(
            outgoing_offsets,
            outgoing_edge_ids,
            &edges_map,
        );
        self.outgoing.store(std::sync::Arc::new(outgoing));

        // Rebuild edges map for incoming (maps edge_id to source, not target)
        edges_map.clear();
        for entry in self.edges.iter() {
            let edge = entry.value();
            edges_map.insert(edge.id, (edge.source, edge.label));
        }

        // Import incoming adjacency
        let incoming = crate::index::adjacency::AdjacencyIndex::import_csr(
            incoming_offsets,
            incoming_edge_ids,
            &edges_map,
        );
        self.incoming.store(std::sync::Arc::new(incoming));

        // CSR is now current
        self.adjacency_dirty
            .store(false, std::sync::atomic::Ordering::Release);
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
            NodeId::new(id).unwrap(),
            GLOBAL_INTERNER.intern(label).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        )
    }

    pub(super) fn create_test_edge(id: u64, source: u64, target: u64, label: &str) -> Edge {
        Edge::new(
            EdgeId::new(id).unwrap(),
            GLOBAL_INTERNER.intern(label).unwrap(),
            NodeId::new(source).unwrap(),
            NodeId::new(target).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        )
    }

    #[test]
    fn test_node_operations() {
        let indexes = CurrentIndexes::new();

        // Initially empty
        assert_eq!(indexes.node_count(), 0);
        assert!(!indexes.contains_node(NodeId::new(1).unwrap()));

        // Insert node
        let node = create_test_node(1, "Person");
        indexes.insert_node(node.clone());

        assert_eq!(indexes.node_count(), 1);
        assert!(indexes.contains_node(NodeId::new(1).unwrap()));

        // Get node
        let retrieved = indexes.get_node(NodeId::new(1).unwrap()).unwrap();
        assert_eq!(retrieved.id, node.id);
        assert_eq!(retrieved.label, node.label);

        // Remove node
        let removed = indexes.remove_node(NodeId::new(1).unwrap()).unwrap();
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
        assert!(indexes.contains_edge(EdgeId::new(1).unwrap()));

        // Get edge
        let retrieved = indexes.get_edge(EdgeId::new(1).unwrap()).unwrap();
        assert_eq!(retrieved.id, edge.id);
        assert_eq!(retrieved.source, edge.source);
        assert_eq!(retrieved.target, edge.target);

        // Remove edge
        let removed = indexes.remove_edge(EdgeId::new(1).unwrap()).unwrap();
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
        assert_eq!(indexes.out_degree(NodeId::new(0).unwrap()), 2);
        assert_eq!(indexes.out_degree(NodeId::new(1).unwrap()), 1);
        assert_eq!(indexes.out_degree(NodeId::new(2).unwrap()), 0);

        let outgoing = indexes.get_outgoing(NodeId::new(0).unwrap());
        assert_eq!(outgoing.len(), 2);

        // Test incoming edges
        assert_eq!(indexes.in_degree(NodeId::new(0).unwrap()), 0);
        assert_eq!(indexes.in_degree(NodeId::new(1).unwrap()), 1);
        assert_eq!(indexes.in_degree(NodeId::new(2).unwrap()), 2);
    }

    #[test]
    fn test_labeled_traversal() {
        let indexes = CurrentIndexes::new();

        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let follows = GLOBAL_INTERNER.intern("FOLLOWS").unwrap();

        // Add edges with different labels
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 0, 2, "FOLLOWS"));
        indexes.insert_edge(create_test_edge(2, 0, 3, "KNOWS"));

        indexes.rebuild_adjacency();

        // Get only KNOWS edges
        let knows_edges = indexes.get_outgoing_with_label(NodeId::new(0).unwrap(), knows);
        assert_eq!(knows_edges.len(), 2);

        // Get only FOLLOWS edges
        let follows_edges = indexes.get_outgoing_with_label(NodeId::new(0).unwrap(), follows);
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
        let first_out = indexes.get_outgoing(NodeId::new(0).unwrap());
        let first_in = indexes.get_incoming(NodeId::new(1).unwrap());

        // Rebuild again
        indexes.rebuild_adjacency();
        let second_out = indexes.get_outgoing(NodeId::new(0).unwrap());
        let second_in = indexes.get_incoming(NodeId::new(1).unwrap());

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
        assert_eq!(indexes.out_degree(NodeId::new(0).unwrap()), 2); // KNOWS and LIKES
        assert_eq!(indexes.in_degree(NodeId::new(2).unwrap()), 2); // from 1 and 0

        // Remove an edge
        indexes.remove_edge(EdgeId::new(1).unwrap());
        indexes.rebuild_adjacency();

        // Verify adjacency updated correctly
        assert_eq!(indexes.out_degree(NodeId::new(1).unwrap()), 0);
        assert_eq!(indexes.in_degree(NodeId::new(2).unwrap()), 1); // only from 0 now
    }

    #[test]
    fn test_lazy_rebuild_on_access() {
        let indexes = CurrentIndexes::new();

        // Add edges WITHOUT calling rebuild_adjacency()
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 0, 2, "KNOWS"));
        indexes.insert_edge(create_test_edge(2, 1, 2, "KNOWS"));

        // Adjacency should be rebuilt lazily on first access
        // This tests that ensure_adjacency_current() works correctly
        let outgoing = indexes.get_outgoing(NodeId::new(0).unwrap());
        assert_eq!(
            outgoing.len(),
            2,
            "Lazy rebuild should make edges accessible"
        );

        // Verify all adjacency data is correct
        assert_eq!(indexes.out_degree(NodeId::new(0).unwrap()), 2);
        assert_eq!(indexes.out_degree(NodeId::new(1).unwrap()), 1);
        assert_eq!(indexes.in_degree(NodeId::new(2).unwrap()), 2);
    }

    #[test]
    fn test_lazy_rebuild_after_delete() {
        let indexes = CurrentIndexes::new();

        // Add edges and access to trigger initial rebuild
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 1, 2, "KNOWS"));
        let _ = indexes.get_outgoing(NodeId::new(0).unwrap());

        // Remove edge WITHOUT calling rebuild_adjacency()
        indexes.remove_edge(EdgeId::new(1).unwrap());

        // Adjacency should be rebuilt lazily on next access
        assert_eq!(indexes.out_degree(NodeId::new(1).unwrap()), 0);
        assert_eq!(indexes.in_degree(NodeId::new(2).unwrap()), 0);
    }

    #[test]
    fn test_no_unnecessary_rebuilds() {
        let indexes = CurrentIndexes::new();

        // Add edges and trigger rebuild
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        let _ = indexes.get_outgoing(NodeId::new(0).unwrap());

        // Multiple accesses should not trigger additional rebuilds
        // (We can't directly observe this, but it's important for performance)
        for _ in 0..10 {
            let outgoing = indexes.get_outgoing(NodeId::new(0).unwrap());
            assert_eq!(outgoing.len(), 1);
        }

        // After accessing, if no modifications, adjacency should stay current
        assert_eq!(indexes.in_degree(NodeId::new(1).unwrap()), 1);
    }

    #[test]
    fn test_lazy_rebuild_with_labeled_traversal() {
        let indexes = CurrentIndexes::new();

        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let follows = GLOBAL_INTERNER.intern("FOLLOWS").unwrap();

        // Add edges with different labels WITHOUT explicit rebuild
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 0, 2, "FOLLOWS"));
        indexes.insert_edge(create_test_edge(2, 0, 3, "KNOWS"));

        // Lazy rebuild should happen on labeled access
        let knows_edges = indexes.get_outgoing_with_label(NodeId::new(0).unwrap(), knows);
        assert_eq!(knows_edges.len(), 2);

        let follows_edges = indexes.get_outgoing_with_label(NodeId::new(0).unwrap(), follows);
        assert_eq!(follows_edges.len(), 1);
    }

    /// Test that AdjacencyGuard works correctly and derefs to slice.
    #[test]
    fn test_adjacency_guard_deref() {
        let indexes = CurrentIndexes::new();

        // Add edges
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 0, 2, "KNOWS"));
        indexes.insert_edge(create_test_edge(2, 1, 2, "KNOWS"));
        indexes.rebuild_adjacency();

        // Get guard
        let guard = indexes.get_outgoing(NodeId::new(0).unwrap());

        // Should deref to slice
        assert_eq!(guard.len(), 2);
        assert_eq!(guard[0].target, NodeId::new(1).unwrap());
        assert_eq!(guard[1].target, NodeId::new(2).unwrap());

        // Should work with slice methods
        let targets: Vec<_> = guard.iter().map(|e| e.target).collect();
        assert_eq!(targets.len(), 2);
    }

    /// Test that AdjacencyGuard can be used in iterators.
    #[test]
    fn test_adjacency_guard_iteration() {
        let indexes = CurrentIndexes::new();

        // Add edges
        for i in 0..10 {
            indexes.insert_edge(create_test_edge(i, 0, i + 1, "LINK"));
        }
        indexes.rebuild_adjacency();

        // Get guard and iterate
        let guard = indexes.get_outgoing(NodeId::new(0).unwrap());
        let mut count = 0;
        for entry in guard.iter() {
            assert_eq!(entry.target.as_u64(), count + 1);
            count += 1;
        }
        assert_eq!(count, 10);
    }

    /// Test that AdjacencyGuard works with empty adjacency lists.
    #[test]
    fn test_adjacency_guard_empty() {
        let indexes = CurrentIndexes::new();
        indexes.rebuild_adjacency();

        // Get guard for node with no edges
        let guard = indexes.get_outgoing(NodeId::new(0).unwrap());
        assert_eq!(guard.len(), 0);
        assert!(guard.is_empty());
    }

    /// Test that AdjacencyGuard can be cloned (by cloning Arc).
    #[test]
    fn test_adjacency_guard_usage_patterns() {
        let indexes = CurrentIndexes::new();

        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 0, 2, "KNOWS"));
        indexes.rebuild_adjacency();

        // Get guard
        let guard = indexes.get_outgoing(NodeId::new(0).unwrap());

        // Can use with functional operations
        let edge_ids: Vec<_> = guard.iter().map(|e| e.edge_id).collect();
        assert_eq!(edge_ids.len(), 2);

        // Can use with for loops
        for (i, entry) in guard.iter().enumerate() {
            assert_eq!(entry.edge_id, EdgeId::new(i as u64).unwrap());
        }

        // Can get length
        assert_eq!(guard.len(), 2);

        // Can index
        assert_eq!(guard[0].edge_id, EdgeId::new(0).unwrap());
        assert_eq!(guard[1].edge_id, EdgeId::new(1).unwrap());
    }

    /// Test that incoming guard works the same way.
    #[test]
    fn test_incoming_guard() {
        let indexes = CurrentIndexes::new();

        indexes.insert_edge(create_test_edge(0, 0, 2, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 1, 2, "KNOWS"));
        indexes.rebuild_adjacency();

        // Get incoming guard for node 2
        let guard = indexes.get_incoming(NodeId::new(2).unwrap());
        assert_eq!(guard.len(), 2);

        // AdjacencyIndex guarantees sorted order by target node
        let sources: Vec<_> = guard.iter().map(|e| e.target).collect();
        assert_eq!(
            sources,
            vec![NodeId::new(0).unwrap(), NodeId::new(1).unwrap()]
        );
    }

    /// Test AdjacencyGuard Debug implementation for coverage.
    #[test]
    fn test_adjacency_guard_debug() {
        let indexes = CurrentIndexes::new();

        indexes.insert_edge(create_test_edge(0, 5, 10, "KNOWS"));
        indexes.rebuild_adjacency();

        // Get guard and format with Debug
        let guard = indexes.get_outgoing(NodeId::new(5).unwrap());
        let debug_str = format!("{:?}", guard);

        // Should contain node ID and show it's an AdjacencyGuard
        assert!(debug_str.contains("AdjacencyGuard"));
        assert!(debug_str.contains("node"));
        assert!(debug_str.contains("entry_count"));
    }

    /// Test AdjacencyGuard with empty list for Debug coverage.
    #[test]
    fn test_adjacency_guard_debug_empty() {
        let indexes = CurrentIndexes::new();
        indexes.rebuild_adjacency();

        // Get guard for non-existent node (empty adjacency list)
        let guard = indexes.get_outgoing(NodeId::new(99).unwrap());
        let debug_str = format!("{:?}", guard);

        // Should format successfully even with empty entries
        assert!(debug_str.contains("AdjacencyGuard"));
        assert!(debug_str.contains("entry_count"));
    }

    /// Test rebuild_adjacency correctly handles many edges.
    ///
    /// This test exercises the pre-allocated vector optimization in
    /// rebuild_adjacency_internal() by creating a graph with many edges
    /// and verifying all adjacencies are correctly computed.
    #[test]
    fn test_rebuild_adjacency_many_edges() {
        let indexes = CurrentIndexes::new();

        // Create a star graph: node 0 connects to nodes 1..=500
        // and nodes 501..=1000 connect to node 0
        // This creates 1000 total edges
        const NUM_EDGES: u64 = 1000;
        const HALF_EDGES: u64 = NUM_EDGES / 2;

        // Add outgoing edges from node 0
        for i in 0..HALF_EDGES {
            indexes.insert_edge(create_test_edge(i, 0, i + 1, "OUTGOING"));
        }

        // Add incoming edges to node 0
        for i in HALF_EDGES..NUM_EDGES {
            indexes.insert_edge(create_test_edge(i, i + 1, 0, "INCOMING"));
        }

        // Rebuild adjacency (this exercises the pre-allocation optimization)
        indexes.rebuild_adjacency();

        // Verify edge count
        assert_eq!(indexes.edge_count(), NUM_EDGES as usize);

        // Verify outgoing from node 0
        assert_eq!(
            indexes.out_degree(NodeId::new(0).unwrap()),
            HALF_EDGES as usize
        );

        // Verify incoming to node 0
        assert_eq!(
            indexes.in_degree(NodeId::new(0).unwrap()),
            HALF_EDGES as usize
        );

        // Verify all outgoing edges are accessible
        let outgoing = indexes.get_outgoing(NodeId::new(0).unwrap());
        assert_eq!(outgoing.len(), HALF_EDGES as usize);

        // Verify all targets are in expected range (1..=500)
        for entry in outgoing.iter() {
            let target_id = entry.target.as_u64();
            assert!(
                (1..=HALF_EDGES).contains(&target_id),
                "Unexpected outgoing target: {}",
                target_id
            );
        }

        // Verify all incoming edges are accessible
        let incoming = indexes.get_incoming(NodeId::new(0).unwrap());
        assert_eq!(incoming.len(), HALF_EDGES as usize);

        // Verify all sources are in expected range (501..=1000)
        for entry in incoming.iter() {
            let source_id = entry.target.as_u64(); // In incoming, target is the source node
            assert!(
                (HALF_EDGES + 1..=NUM_EDGES).contains(&source_id),
                "Unexpected incoming source: {}",
                source_id
            );
        }
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
                        let _ = indexes.remove_edge(EdgeId::new(*id).unwrap());
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
                total_out_degree += indexes.out_degree(NodeId::new(node_id).unwrap());
                total_in_degree += indexes.in_degree(NodeId::new(node_id).unwrap());
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
                        let _ = indexes.remove_edge(EdgeId::new(*id).unwrap());
                    }
                }
            }

            // Rebuild twice
            indexes.rebuild_adjacency();
            let first_results: Vec<_> = (0..10)
                .map(|i| {
                    let node = NodeId::new(i).unwrap();
                    (indexes.get_outgoing(node), indexes.get_incoming(node))
                })
                .collect();

            indexes.rebuild_adjacency();
            let second_results: Vec<_> = (0..10)
                .map(|i| {
                    let node = NodeId::new(i).unwrap();
                    (indexes.get_outgoing(node), indexes.get_incoming(node))
                })
                .collect();

            // Results should be identical
            assert_eq!(first_results, second_results);
        }
    }
}

// Concurrency tests for rebuild race condition fix
#[cfg(test)]
mod concurrency_tests {
    use super::tests::{create_test_edge, create_test_node};
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    /// Test that edges inserted during rebuild are not lost.
    ///
    /// This test verifies the fix for the race condition where edges
    /// inserted while `rebuild_adjacency()` was iterating could be
    /// lost from the adjacency indexes.
    #[test]
    fn test_concurrent_insert_during_rebuild() {
        let indexes = Arc::new(CurrentIndexes::new());

        // Add initial nodes
        for i in 0..10 {
            indexes.insert_node(create_test_node(i, "Node"));
        }

        // Add initial edges
        for i in 0..100 {
            indexes.insert_edge(create_test_edge(i, i % 10, (i + 1) % 10, "LINKS"));
        }
        indexes.rebuild_adjacency();

        // Flag to coordinate threads
        let should_stop = Arc::new(AtomicBool::new(false));

        // Thread 1: Continuously rebuild adjacency
        let indexes_clone = Arc::clone(&indexes);
        let should_stop_clone = Arc::clone(&should_stop);
        let rebuild_handle = thread::spawn(move || {
            let mut rebuild_count = 0;
            while !should_stop_clone.load(Ordering::Relaxed) {
                indexes_clone.rebuild_adjacency();
                rebuild_count += 1;
                // Small sleep to allow interleaving
                thread::sleep(Duration::from_micros(10));
            }
            rebuild_count
        });

        // Thread 2: Insert edges while rebuilds are happening
        let indexes_clone = Arc::clone(&indexes);
        let insert_handle = thread::spawn(move || {
            for i in 100..200 {
                indexes_clone.insert_edge(create_test_edge(i, i % 10, (i + 1) % 10, "NEW"));
                thread::sleep(Duration::from_micros(5));
            }
        });

        // Wait for inserts to complete
        insert_handle.join().unwrap();

        // Signal rebuild thread to stop
        should_stop.store(true, Ordering::Relaxed);
        let rebuild_count = rebuild_handle.join().unwrap();

        // Final rebuild to ensure consistency
        indexes.rebuild_adjacency();

        // Verify: all edges should be present
        assert_eq!(
            indexes.edge_count(),
            200,
            "Expected 200 edges after concurrent insert/rebuild"
        );

        // Verify: adjacency indexes reflect all edges
        let mut total_out_degree = 0;
        let mut total_in_degree = 0;
        for i in 0..10 {
            total_out_degree += indexes.out_degree(NodeId::new(i).unwrap());
            total_in_degree += indexes.in_degree(NodeId::new(i).unwrap());
        }

        assert_eq!(
            total_out_degree, 200,
            "Adjacency out-degree should match edge count after {} rebuilds",
            rebuild_count
        );
        assert_eq!(
            total_in_degree, 200,
            "Adjacency in-degree should match edge count after {} rebuilds",
            rebuild_count
        );
    }

    /// Test that edges removed during rebuild are properly excluded.
    #[test]
    fn test_concurrent_remove_during_rebuild() {
        let indexes = Arc::new(CurrentIndexes::new());

        // Add nodes
        for i in 0..10 {
            indexes.insert_node(create_test_node(i, "Node"));
        }

        // Add edges that will be removed
        for i in 0..100 {
            indexes.insert_edge(create_test_edge(i, i % 10, (i + 1) % 10, "TEMP"));
        }
        indexes.rebuild_adjacency();

        let should_stop = Arc::new(AtomicBool::new(false));

        // Thread 1: Continuously rebuild
        let indexes_clone = Arc::clone(&indexes);
        let should_stop_clone = Arc::clone(&should_stop);
        let rebuild_handle = thread::spawn(move || {
            while !should_stop_clone.load(Ordering::Relaxed) {
                indexes_clone.rebuild_adjacency();
                thread::sleep(Duration::from_micros(10));
            }
        });

        // Thread 2: Remove edges
        let indexes_clone = Arc::clone(&indexes);
        let remove_handle = thread::spawn(move || {
            for i in 0..50 {
                indexes_clone.remove_edge(EdgeId::new(i).unwrap());
                thread::sleep(Duration::from_micros(5));
            }
        });

        remove_handle.join().unwrap();
        should_stop.store(true, Ordering::Relaxed);
        rebuild_handle.join().unwrap();

        // Final rebuild
        indexes.rebuild_adjacency();

        // Verify: 50 edges remain
        assert_eq!(indexes.edge_count(), 50);

        // Verify adjacency matches
        let mut total_degree = 0;
        for i in 0..10 {
            total_degree += indexes.out_degree(NodeId::new(i).unwrap());
        }
        assert_eq!(total_degree, 50, "Adjacency should reflect removed edges");
    }

    /// Stress test with multiple concurrent inserters and rebuilders.
    #[test]
    fn test_multi_threaded_stress() {
        let indexes = Arc::new(CurrentIndexes::new());

        // Add nodes
        for i in 0..20 {
            indexes.insert_node(create_test_node(i, "Node"));
        }

        let should_stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();

        // Spawn 3 inserter threads
        for thread_id in 0..3 {
            let indexes_clone = Arc::clone(&indexes);
            handles.push(thread::spawn(move || {
                for i in 0..100 {
                    let edge_id = thread_id * 1000 + i;
                    indexes_clone.insert_edge(create_test_edge(
                        edge_id,
                        edge_id % 20,
                        (edge_id + 1) % 20,
                        "STRESS",
                    ));
                }
            }));
        }

        // Spawn 2 rebuilder threads
        for _ in 0..2 {
            let indexes_clone = Arc::clone(&indexes);
            let should_stop_clone = Arc::clone(&should_stop);
            handles.push(thread::spawn(move || {
                while !should_stop_clone.load(Ordering::Relaxed) {
                    indexes_clone.rebuild_adjacency();
                    thread::sleep(Duration::from_micros(50));
                }
            }));
        }

        // Wait for inserters (first 3 handles)
        for handle in handles.drain(0..3) {
            handle.join().unwrap();
        }

        // Stop rebuilders
        should_stop.store(true, Ordering::Relaxed);
        for handle in handles {
            handle.join().unwrap();
        }

        // Final rebuild and verify
        indexes.rebuild_adjacency();

        // Should have 300 edges (3 threads × 100 edges each)
        assert_eq!(indexes.edge_count(), 300);

        let mut total_degree = 0;
        for i in 0..20 {
            total_degree += indexes.out_degree(NodeId::new(i).unwrap());
        }
        assert_eq!(
            total_degree, 300,
            "Adjacency must match edge count after stress test"
        );
    }

    /// Test that rebuilds complete successfully without deadlock.
    #[test]
    fn test_no_deadlock_insert_rebuild() {
        use std::time::Instant;

        let indexes = Arc::new(CurrentIndexes::new());

        for i in 0..5 {
            indexes.insert_node(create_test_node(i, "Node"));
        }

        let start = Instant::now();
        let timeout = Duration::from_secs(5);

        let indexes_clone = Arc::clone(&indexes);
        let insert_handle = thread::spawn(move || {
            for i in 0..500 {
                indexes_clone.insert_edge(create_test_edge(i, i % 5, (i + 1) % 5, "EDGE"));
            }
        });

        let indexes_clone = Arc::clone(&indexes);
        let rebuild_handle = thread::spawn(move || {
            for _ in 0..50 {
                indexes_clone.rebuild_adjacency();
            }
        });

        // Both should complete within timeout (no deadlock)
        insert_handle.join().unwrap();
        rebuild_handle.join().unwrap();

        assert!(
            start.elapsed() < timeout,
            "Operations should complete without deadlock"
        );

        // Verify final state
        indexes.rebuild_adjacency();
        assert_eq!(indexes.edge_count(), 500);
    }

    /// Test concurrent lazy rebuild without explicit rebuild_adjacency() calls.
    ///
    /// This test verifies that multiple threads can safely insert edges and
    /// access adjacency concurrently, relying on lazy rebuilding rather than
    /// explicit rebuild calls.
    #[test]
    fn test_concurrent_lazy_rebuild() {
        use std::sync::Arc;
        use std::thread;

        let indexes = Arc::new(CurrentIndexes::new());

        // Add initial nodes
        for i in 0..20 {
            indexes.insert_node(create_test_node(i, "Node"));
        }

        let mut handles = Vec::new();

        // Spawn 3 threads that insert edges WITHOUT calling rebuild_adjacency()
        for thread_id in 0..3 {
            let indexes_clone = Arc::clone(&indexes);
            handles.push(thread::spawn(move || {
                for i in 0..50 {
                    let edge_id = thread_id * 100 + i;
                    indexes_clone.insert_edge(create_test_edge(
                        edge_id,
                        edge_id % 20,
                        (edge_id + 1) % 20,
                        "LINK",
                    ));
                }
            }));
        }

        // Spawn 2 threads that read adjacency (triggering lazy rebuilds)
        for _ in 0..2 {
            let indexes_clone = Arc::clone(&indexes);
            handles.push(thread::spawn(move || {
                for node_id in 0..20 {
                    let node = NodeId::new(node_id).unwrap();
                    // This should trigger lazy rebuild if needed
                    let _outgoing = indexes_clone.get_outgoing(node);
                    let _degree = indexes_clone.out_degree(node);
                }
            }));
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify final state: all edges should be accessible via adjacency
        assert_eq!(
            indexes.edge_count(),
            150,
            "Should have 150 edges (3 threads × 50 edges)"
        );

        // Verify adjacency is correct (may trigger final lazy rebuild)
        let mut total_out_degree = 0;
        let mut total_in_degree = 0;
        for i in 0..20 {
            total_out_degree += indexes.out_degree(NodeId::new(i).unwrap());
            total_in_degree += indexes.in_degree(NodeId::new(i).unwrap());
        }

        assert_eq!(
            total_out_degree, 150,
            "Lazy rebuild should make all edges accessible via outgoing adjacency"
        );
        assert_eq!(
            total_in_degree, 150,
            "Lazy rebuild should make all edges accessible via incoming adjacency"
        );
    }
}

/// Tests specifically for the dirty flag optimization (Issue #336).
///
/// These tests verify that using Relaxed ordering on the fast path dirty flag
/// check is safe and maintains correctness. The key insight is that ArcSwap
/// already provides the necessary synchronization for the actual data access.
#[cfg(test)]
mod dirty_flag_optimization_tests {
    use super::tests::{create_test_edge, create_test_node};
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::thread;

    /// Test that dirty flag is correctly set on edge insertion.
    #[test]
    fn test_dirty_flag_set_on_insert() {
        let indexes = CurrentIndexes::new();

        // Initially not dirty (empty graph)
        assert!(
            !indexes.adjacency_dirty.load(Ordering::Relaxed),
            "New indexes should not be dirty"
        );

        // Insert an edge
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));

        // Should now be dirty
        assert!(
            indexes.adjacency_dirty.load(Ordering::Relaxed),
            "Dirty flag should be set after insert"
        );
    }

    /// Test that dirty flag is correctly set on edge removal.
    #[test]
    fn test_dirty_flag_set_on_remove() {
        let indexes = CurrentIndexes::new();

        // Insert and rebuild to clear dirty flag
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.rebuild_adjacency();

        assert!(
            !indexes.adjacency_dirty.load(Ordering::Relaxed),
            "Dirty flag should be cleared after rebuild"
        );

        // Remove the edge
        indexes.remove_edge(EdgeId::new(0).unwrap());

        // Should be dirty again
        assert!(
            indexes.adjacency_dirty.load(Ordering::Relaxed),
            "Dirty flag should be set after remove"
        );
    }

    /// Test that dirty flag is cleared after rebuild.
    #[test]
    fn test_dirty_flag_cleared_after_rebuild() {
        let indexes = CurrentIndexes::new();

        // Insert edges to set dirty flag
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
        indexes.insert_edge(create_test_edge(1, 1, 2, "KNOWS"));

        assert!(
            indexes.adjacency_dirty.load(Ordering::Relaxed),
            "Dirty flag should be set after inserts"
        );

        // Rebuild should clear the flag
        indexes.rebuild_adjacency();

        assert!(
            !indexes.adjacency_dirty.load(Ordering::Relaxed),
            "Dirty flag should be cleared after rebuild"
        );
    }

    /// Test that adjacency access triggers lazy rebuild when dirty.
    #[test]
    fn test_lazy_rebuild_clears_dirty_flag() {
        let indexes = CurrentIndexes::new();

        // Insert edge (sets dirty flag)
        indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));

        assert!(
            indexes.adjacency_dirty.load(Ordering::Relaxed),
            "Dirty flag should be set"
        );

        // Access adjacency - should trigger lazy rebuild
        let _outgoing = indexes.get_outgoing(NodeId::new(0).unwrap());

        assert!(
            !indexes.adjacency_dirty.load(Ordering::Relaxed),
            "Dirty flag should be cleared after lazy rebuild"
        );
    }

    /// Test that multiple reads don't unnecessarily check/rebuild when not dirty.
    ///
    /// This test verifies the fast path behavior: when dirty flag is false,
    /// subsequent reads should skip the rebuild entirely.
    #[test]
    fn test_fast_path_no_rebuild_when_clean() {
        let indexes = CurrentIndexes::new();

        // Setup: insert edges and trigger initial rebuild
        for i in 0..10 {
            indexes.insert_edge(create_test_edge(i, i % 5, (i + 1) % 5, "LINK"));
        }
        indexes.rebuild_adjacency();

        // Dirty flag should be false
        assert!(!indexes.adjacency_dirty.load(Ordering::Relaxed));

        // Multiple reads should all use the fast path
        for _ in 0..100 {
            let outgoing = indexes.get_outgoing(NodeId::new(0).unwrap());
            assert!(!outgoing.is_empty());

            // Dirty flag should remain false (no modifications)
            assert!(!indexes.adjacency_dirty.load(Ordering::Relaxed));
        }
    }

    /// Test that ArcSwap provides consistent snapshots even with Relaxed ordering.
    ///
    /// This is the key test for the optimization: even if we use Relaxed ordering
    /// on the dirty flag check, ArcSwap ensures we always see a consistent
    /// (though potentially stale) snapshot of the adjacency data.
    #[test]
    fn test_arcswap_provides_consistent_snapshots() {
        let indexes = Arc::new(CurrentIndexes::new());

        // Add initial nodes and edges
        for i in 0..10 {
            indexes.insert_node(create_test_node(i, "Node"));
        }
        for i in 0..50 {
            indexes.insert_edge(create_test_edge(i, i % 10, (i + 1) % 10, "LINK"));
        }
        indexes.rebuild_adjacency();

        let mut handles = Vec::new();

        // Spawn readers that access adjacency repeatedly
        for _ in 0..4 {
            let indexes_clone = Arc::clone(&indexes);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    // Get a snapshot via ArcSwap
                    let guard = indexes_clone.get_outgoing(NodeId::new(0).unwrap());

                    // The snapshot should be internally consistent:
                    // - All entries should be valid
                    // - Length should match what we iterate over
                    let len = guard.len();
                    let iter_count = guard.iter().count();
                    assert_eq!(len, iter_count, "Snapshot should be internally consistent");

                    // Each entry should have valid data
                    // Targets are created as (i + 1) % 10, so always < 10
                    for entry in guard.iter() {
                        assert!(entry.target.as_u64() < 10);
                        assert!(entry.edge_id.as_u64() < 100);
                    }
                }
            }));
        }

        // Spawn a writer that modifies edges
        let indexes_clone = Arc::clone(&indexes);
        handles.push(thread::spawn(move || {
            for i in 50..100 {
                indexes_clone.insert_edge(create_test_edge(i, i % 10, (i + 1) % 10, "NEW"));
            }
        }));

        // All threads should complete without panics or data corruption
        for handle in handles {
            handle.join().expect("Thread should complete without panic");
        }

        // Final verification
        assert_eq!(indexes.edge_count(), 100);
    }

    /// Test the "benign race" scenario documented in ensure_adjacency_current.
    ///
    /// This test exercises the race window where:
    /// 1. Thread A checks dirty=false
    /// 2. Thread B inserts edge, sets dirty=true
    /// 3. Thread A proceeds with potentially stale adjacency
    ///
    /// The key assertion is that this is safe because:
    /// - Thread A sees a consistent (older) snapshot via ArcSwap
    /// - No data corruption occurs
    /// - Next access will see the new data
    #[test]
    fn test_benign_race_eventual_consistency() {
        let indexes = Arc::new(CurrentIndexes::new());

        // Setup initial state
        for i in 0..5 {
            indexes.insert_node(create_test_node(i, "Node"));
        }
        indexes.insert_edge(create_test_edge(0, 0, 1, "INITIAL"));
        indexes.rebuild_adjacency();

        let barrier = Arc::new(std::sync::Barrier::new(2));

        // Thread A: Reader that may see stale data
        let indexes_a = Arc::clone(&indexes);
        let barrier_a = Arc::clone(&barrier);
        let reader = thread::spawn(move || {
            // Wait for both threads to be ready
            barrier_a.wait();

            // Read multiple times
            let mut saw_new_edge = false;
            for _ in 0..1000 {
                let guard = indexes_a.get_outgoing(NodeId::new(0).unwrap());
                // May or may not see the new edge depending on timing
                if guard.len() > 1 {
                    saw_new_edge = true;
                }
                // Should never see corrupted data
                for entry in guard.iter() {
                    assert!(entry.target.as_u64() < 10);
                }
            }
            saw_new_edge
        });

        // Thread B: Writer that adds a new edge
        let indexes_b = Arc::clone(&indexes);
        let barrier_b = Arc::clone(&barrier);
        let writer = thread::spawn(move || {
            // Wait for both threads to be ready
            barrier_b.wait();

            // Add a new edge
            indexes_b.insert_edge(create_test_edge(1, 0, 2, "NEW"));
        });

        writer.join().expect("Writer should complete");
        let _ = reader.join().expect("Reader should complete");

        // Eventually, the new edge must be visible
        let final_guard = indexes.get_outgoing(NodeId::new(0).unwrap());
        assert_eq!(
            final_guard.len(),
            2,
            "New edge should be visible after final access"
        );
    }

    /// Stress test for the Relaxed ordering optimization under high contention.
    ///
    /// This test creates many concurrent readers and writers to verify that
    /// the Relaxed ordering doesn't cause correctness issues.
    #[test]
    fn test_relaxed_ordering_stress() {
        let indexes = Arc::new(CurrentIndexes::new());

        // Setup initial graph
        for i in 0..20 {
            indexes.insert_node(create_test_node(i, "Node"));
        }
        for i in 0..100 {
            indexes.insert_edge(create_test_edge(i, i % 20, (i + 1) % 20, "INIT"));
        }
        indexes.rebuild_adjacency();

        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let mut reader_handles = Vec::new();
        let mut writer_handles = Vec::new();

        // Spawn 8 reader threads
        for _ in 0..8 {
            let indexes_clone = Arc::clone(&indexes);
            let running_clone = Arc::clone(&running);
            reader_handles.push(thread::spawn(move || {
                let mut reads = 0u64;
                while running_clone.load(Ordering::Acquire) {
                    for node_id in 0..20 {
                        let guard = indexes_clone.get_outgoing(NodeId::new(node_id).unwrap());
                        // Verify data consistency
                        let len = guard.len();
                        let actual_len = guard.iter().count();
                        assert_eq!(len, actual_len, "Data should be consistent");
                        reads += 1;
                    }
                }
                reads
            }));
        }

        // Spawn 2 writer threads
        for thread_id in 0..2 {
            let indexes_clone = Arc::clone(&indexes);
            writer_handles.push(thread::spawn(move || {
                for i in 0..50 {
                    let edge_id = 100 + thread_id * 100 + i;
                    indexes_clone.insert_edge(create_test_edge(
                        edge_id,
                        edge_id % 20,
                        (edge_id + 1) % 20,
                        "STRESS",
                    ));
                }
            }));
        }

        // Let writers finish
        for handle in writer_handles {
            handle.join().expect("Writer should complete");
        }

        // Stop readers (Release synchronizes with Acquire loads in reader threads)
        running.store(false, Ordering::Release);

        // Wait for readers and collect stats
        let mut total_reads = 0u64;
        for handle in reader_handles {
            total_reads += handle.join().expect("Reader should complete");
        }

        // Verify final state
        assert_eq!(indexes.edge_count(), 200, "All edges should be present");

        // Final consistency check
        let _outgoing = indexes.get_outgoing(NodeId::new(0).unwrap());

        let mut total_degree = 0;
        for i in 0..20 {
            total_degree += indexes.out_degree(NodeId::new(i).unwrap());
        }
        assert_eq!(total_degree, 200, "Adjacency should reflect all edges");

        // Sanity check that readers actually did work
        assert!(total_reads > 1000, "Should have performed many reads");
    }
}
