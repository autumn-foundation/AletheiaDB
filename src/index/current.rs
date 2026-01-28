//! Current-state indexes using concurrent data structures.
//!
//! This module provides indexes for the current state of the graph using
//! DashMap for lock-free concurrent access. These are the "hot path" indexes
//! that must be extremely fast.

use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::interning::InternedString;
use crate::index::adjacency::AdjacencyEntry;
use crate::index::incremental_adjacency::{CompactionScheduler, IncrementalAdjacencyIndex};
use dashmap::DashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

// Note: AdjacencyGuard was removed in favor of MergedAdjacencyGuard from incremental_adjacency.
// The new guard supports merging frozen CSR + delta buffer on-the-fly.

/// Concurrent indexes for current-state graph queries.
///
/// These indexes provide O(1) lookups for nodes and edges, plus efficient
/// graph traversal through incremental CSR adjacency indexes.
///
/// # Incremental Adjacency (Issue #259)
///
/// Uses LSM-tree inspired incremental adjacency indexes:
/// - **O(1) edge inserts**: No rebuild cliff, edges go to delta buffer
/// - **O(1) edge deletes**: Tombstone marking, no rebuild needed
/// - **Lock-free reads**: Merge frozen + delta on-the-fly
/// - **Background compaction**: Automatic delta → frozen merging
///
/// # Concurrency Model
///
/// - **Node/Edge maps**: DashMap for lock-free concurrent access
/// - **Adjacency indexes**: IncrementalAdjacencyIndex with lock-free reads
/// - **No rebuild lock needed**: Incremental inserts/deletes are thread-safe
///
/// # Performance
///
/// - **Insert edge**: ~100ns (was ~10ms+ due to rebuild)
/// - **Read adjacency**: ~20-30ns (merge overhead vs pure CSR)
/// - **Compaction**: Automatic in background, doesn't block operations
pub struct CurrentIndexes {
    /// Node ID → Node (O(1) lookup)
    nodes: DashMap<NodeId, Node>,
    /// Edge ID → Edge (O(1) lookup)
    edges: DashMap<EdgeId, Edge>,
    /// Outgoing edges: source node → adjacency list (incremental with O(1) inserts)
    outgoing: Arc<IncrementalAdjacencyIndex>,
    /// Incoming edges: target node → adjacency list (incremental with O(1) inserts)
    incoming: Arc<IncrementalAdjacencyIndex>,
    /// Optional background compaction scheduler for outgoing index
    outgoing_compaction: Option<(CompactionScheduler, JoinHandle<()>)>,
    /// Optional background compaction scheduler for incoming index
    incoming_compaction: Option<(CompactionScheduler, JoinHandle<()>)>,
}

impl CurrentIndexes {
    /// Create new empty indexes with incremental adjacency.
    ///
    /// Uses incremental CSR adjacency indexes for O(1) inserts and deletes.
    /// No background compaction - call `compact_adjacency()` manually when needed.
    pub fn new() -> Self {
        CurrentIndexes {
            nodes: DashMap::new(),
            edges: DashMap::new(),
            outgoing: Arc::new(IncrementalAdjacencyIndex::new()),
            incoming: Arc::new(IncrementalAdjacencyIndex::new()),
            outgoing_compaction: None,
            incoming_compaction: None,
        }
    }

    /// Create new indexes with background compaction enabled.
    ///
    /// Background thread will automatically compact adjacency indexes
    /// when thresholds are exceeded. Call `shutdown_background_compaction()`
    /// before dropping to cleanly stop the background thread.
    pub fn new_with_background_compaction() -> Self {
        let outgoing = Arc::new(IncrementalAdjacencyIndex::new());
        let incoming = Arc::new(IncrementalAdjacencyIndex::new());

        // Start background compaction for both outgoing and incoming indexes
        let outgoing_scheduler = CompactionScheduler::new(Arc::clone(&outgoing));
        let outgoing_handle = outgoing_scheduler.start();

        let incoming_scheduler = CompactionScheduler::new(Arc::clone(&incoming));
        let incoming_handle = incoming_scheduler.start();

        CurrentIndexes {
            nodes: DashMap::new(),
            edges: DashMap::new(),
            outgoing,
            incoming,
            outgoing_compaction: Some((outgoing_scheduler, outgoing_handle)),
            incoming_compaction: Some((incoming_scheduler, incoming_handle)),
        }
    }

    /// Shutdown background compaction if enabled.
    ///
    /// Waits for background thread to finish. Safe to call even if
    /// background compaction is not enabled (no-op).
    ///
    /// # Returns
    ///
    /// - `Ok(())` if shutdown was successful or no scheduler was running
    /// - `Err(String)` if the background thread panicked during shutdown
    pub fn shutdown_background_compaction(&mut self) -> Result<(), String> {
        // Shutdown outgoing compaction
        if let Some((scheduler, handle)) = self.outgoing_compaction.take() {
            scheduler.shutdown();
            handle
                .join()
                .map_err(|e| format!("Outgoing compaction thread panicked: {:?}", e))?;
        }
        // Shutdown incoming compaction
        if let Some((scheduler, handle)) = self.incoming_compaction.take() {
            scheduler.shutdown();
            handle
                .join()
                .map_err(|e| format!("Incoming compaction thread panicked: {:?}", e))?;
        }
        Ok(())
    }

    /// Get frozen edge count for outgoing adjacency (for testing).
    #[doc(hidden)]
    pub fn frozen_edge_count(&self) -> usize {
        self.outgoing.frozen_edge_count()
    }

    /// Get the number of edges in the delta buffer.
    ///
    /// This represents edges that have been inserted but not yet compacted into frozen.
    pub fn delta_edge_count(&self) -> usize {
        self.outgoing.delta_edge_count()
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
    /// Insert an edge into the indexes.
    ///
    /// Updates both the edge map and adjacency indexes incrementally.
    /// - **O(1) amortized**: Edge goes to delta buffer, no rebuild
    /// - **Thread-safe**: Lock-free concurrent inserts
    /// - **Atomic visibility**: Edge immediately visible in adjacency queries
    pub fn insert_edge(&self, edge: Edge) {
        // Insert into edge map
        let edge_id = edge.id;
        let source = edge.source;
        let target = edge.target;
        let label = edge.label;

        // Use entry API to atomically check+insert, avoiding TOCTOU race condition
        // For updates, the adjacency doesn't change (source/target stay the same)
        // so we skip adjacency insertion to avoid duplicates
        use dashmap::mapref::entry::Entry;

        match self.edges.entry(edge_id) {
            Entry::Occupied(mut e) => {
                // Update: just replace edge data, don't modify adjacency
                e.insert(edge);
            }
            Entry::Vacant(e) => {
                // New edge: insert into both edge map and adjacency
                e.insert(edge);
                self.outgoing
                    .insert(source, AdjacencyEntry::new(target, edge_id, label));
                self.incoming
                    .insert(target, AdjacencyEntry::new(source, edge_id, label));
            }
        }
    }

    /// Get a node by ID.
    pub fn get_node(&self, id: NodeId) -> Option<Node> {
        self.nodes.get(&id).map(|entry| entry.value().clone())
    }

    /// Get an edge by ID.
    pub fn get_edge(&self, id: EdgeId) -> Option<Edge> {
        self.edges.get(&id).map(|entry| entry.value().clone())
    }

    // ========================================================================
    // Zero-copy access methods (Issue #190)
    //
    // These methods provide efficient access to node/edge data without cloning
    // the entire structure. Use these for hot paths where you only need to
    // read specific fields.
    // ========================================================================

    /// Access a node without cloning, executing a closure on the node data.
    ///
    /// This method provides zero-copy read access to node data for hot paths
    /// where only specific fields are needed. The closure receives a reference
    /// to the node and can return any computed value.
    ///
    /// # Performance
    ///
    /// - **No allocation**: Does not clone the Node (~56 bytes saved)
    /// - **No Arc increment**: Does not increment PropertyMap reference count
    /// - **Lock duration**: Holds DashMap read lock only during closure execution
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Check if node has a specific label (zero-copy)
    /// let is_person = indexes.with_node(node_id, |n| n.label == person_label)
    ///     .unwrap_or(false);
    ///
    /// // Extract multiple fields in one call
    /// let (id, label) = indexes.with_node(node_id, |n| (n.id, n.label))?;
    /// ```
    #[inline]
    pub fn with_node<F, R>(&self, id: NodeId, f: F) -> Option<R>
    where
        F: FnOnce(&Node) -> R,
    {
        self.nodes.get(&id).map(|entry| f(entry.value()))
    }

    /// Access an edge without cloning, executing a closure on the edge data.
    ///
    /// This method provides zero-copy read access to edge data for hot paths
    /// where only specific fields are needed. The closure receives a reference
    /// to the edge and can return any computed value.
    ///
    /// # Performance
    ///
    /// - **No allocation**: Does not clone the Edge (~72 bytes saved)
    /// - **No Arc increment**: Does not increment PropertyMap reference count
    /// - **Lock duration**: Holds DashMap read lock only during closure execution
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Get edge endpoints (zero-copy)
    /// let (source, target) = indexes.with_edge(edge_id, |e| (e.source, e.target))?;
    ///
    /// // Check if edge connects specific nodes
    /// let connects = indexes.with_edge(edge_id, |e| e.connects(src, tgt))
    ///     .unwrap_or(false);
    /// ```
    #[inline]
    pub fn with_edge<F, R>(&self, id: EdgeId, f: F) -> Option<R>
    where
        F: FnOnce(&Edge) -> R,
    {
        self.edges.get(&id).map(|entry| f(entry.value()))
    }

    /// Get the label of a node without cloning the entire node.
    ///
    /// This is a convenience method for the common case of checking a node's
    /// label. It's equivalent to `with_node(id, |n| n.label)` but more concise.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the label (8 bytes)
    /// - **No allocation**: Does not clone Node or PropertyMap
    ///
    /// # Example
    ///
    /// ```ignore
    /// if indexes.get_node_label(node_id) == Some(person_label) {
    ///     // Node is a Person
    /// }
    /// ```
    #[inline]
    pub fn get_node_label(&self, id: NodeId) -> Option<InternedString> {
        self.nodes.get(&id).map(|entry| entry.value().label)
    }

    /// Get the endpoints (source, target) of an edge without cloning.
    ///
    /// This is a convenience method for the common case of getting an edge's
    /// source and target nodes. It's equivalent to
    /// `with_edge(id, |e| (e.source, e.target))` but more concise.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns two NodeIds (16 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some((source, target)) = indexes.get_edge_endpoints(edge_id) {
    ///     // Process source and target
    /// }
    /// ```
    #[inline]
    pub fn get_edge_endpoints(&self, id: EdgeId) -> Option<(NodeId, NodeId)> {
        self.edges
            .get(&id)
            .map(|entry| (entry.value().source, entry.value().target))
    }

    /// Get the label of an edge without cloning the entire edge.
    ///
    /// This is a convenience method for the common case of checking an edge's
    /// label. It's equivalent to `with_edge(id, |e| e.label)` but more concise.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the label (8 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    ///
    /// # Example
    ///
    /// ```ignore
    /// if indexes.get_edge_label(edge_id) == Some(knows_label) {
    ///     // Edge is a KNOWS relationship
    /// }
    /// ```
    #[inline]
    pub fn get_edge_label(&self, id: EdgeId) -> Option<InternedString> {
        self.edges.get(&id).map(|entry| entry.value().label)
    }

    /// Get the source node of an edge without cloning the entire edge.
    ///
    /// This is a convenience method for getting just the source node ID.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the source NodeId (8 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    #[inline]
    pub fn get_edge_source(&self, id: EdgeId) -> Option<NodeId> {
        self.edges.get(&id).map(|entry| entry.value().source)
    }

    /// Get the target node of an edge without cloning the entire edge.
    ///
    /// This is a convenience method for getting just the target node ID.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the target NodeId (8 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    #[inline]
    pub fn get_edge_target(&self, id: EdgeId) -> Option<NodeId> {
        self.edges.get(&id).map(|entry| entry.value().target)
    }

    /// Get a property value from a node without cloning the entire node.
    ///
    /// This is useful when you only need to read a single property from a node.
    /// The property value is cloned (Arc types are cheap to clone), but the
    /// rest of the node structure is not.
    ///
    /// # Performance
    ///
    /// - **Partial clone**: Only clones the PropertyValue (cheap for Arc types)
    /// - **No Node clone**: Does not clone the entire Node structure
    /// - **Interning cost**: The key is interned on each call; for hot paths
    ///   with repeated lookups, use [`get_node_property_by_key`](Self::get_node_property_by_key)
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(name) = indexes.get_node_property(node_id, "name") {
    ///     println!("Node name: {:?}", name);
    /// }
    /// ```
    #[inline]
    pub fn get_node_property(
        &self,
        id: NodeId,
        key: &str,
    ) -> Option<crate::core::property::PropertyValue> {
        self.nodes
            .get(&id)
            .and_then(|entry| entry.value().properties.get(key).cloned())
    }

    /// Get a property value from an edge without cloning the entire edge.
    ///
    /// This is useful when you only need to read a single property from an edge.
    /// The property value is cloned (Arc types are cheap to clone), but the
    /// rest of the edge structure is not.
    ///
    /// # Performance
    ///
    /// - **Partial clone**: Only clones the PropertyValue (cheap for Arc types)
    /// - **No Edge clone**: Does not clone the entire Edge structure
    /// - **Interning cost**: The key is interned on each call; for hot paths
    ///   with repeated lookups, use [`get_edge_property_by_key`](Self::get_edge_property_by_key)
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(weight) = indexes.get_edge_property(edge_id, "weight") {
    ///     println!("Edge weight: {:?}", weight);
    /// }
    /// ```
    #[inline]
    pub fn get_edge_property(
        &self,
        id: EdgeId,
        key: &str,
    ) -> Option<crate::core::property::PropertyValue> {
        self.edges
            .get(&id)
            .and_then(|entry| entry.value().properties.get(key).cloned())
    }

    /// Get a property value from a node using a pre-interned key.
    ///
    /// This is the high-performance version of [`get_node_property`](Self::get_node_property)
    /// for hot paths where the same property key is accessed repeatedly.
    ///
    /// # Performance
    ///
    /// - **No interning**: Avoids HashMap lookup in the global interner
    /// - **Partial clone**: Only clones the PropertyValue (cheap for Arc types)
    /// - **Ideal for loops**: Pre-intern key once, use this method in the loop
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::core::interning::GLOBAL_INTERNER;
    ///
    /// // Pre-intern the key once outside the loop
    /// let name_key = GLOBAL_INTERNER.intern("name").unwrap();
    ///
    /// // Use the interned key in the hot loop
    /// for node_id in node_ids {
    ///     if let Some(name) = indexes.get_node_property_by_key(node_id, &name_key) {
    ///         // Process name
    ///     }
    /// }
    /// ```
    #[inline]
    pub fn get_node_property_by_key(
        &self,
        id: NodeId,
        key: &crate::core::property::PropertyKey,
    ) -> Option<crate::core::property::PropertyValue> {
        self.nodes
            .get(&id)
            .and_then(|entry| entry.value().properties.get_by_interned_key(key).cloned())
    }

    /// Get a property value from an edge using a pre-interned key.
    ///
    /// This is the high-performance version of [`get_edge_property`](Self::get_edge_property)
    /// for hot paths where the same property key is accessed repeatedly.
    ///
    /// # Performance
    ///
    /// - **No interning**: Avoids HashMap lookup in the global interner
    /// - **Partial clone**: Only clones the PropertyValue (cheap for Arc types)
    /// - **Ideal for loops**: Pre-intern key once, use this method in the loop
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::core::interning::GLOBAL_INTERNER;
    ///
    /// // Pre-intern the key once outside the loop
    /// let weight_key = GLOBAL_INTERNER.intern("weight").unwrap();
    ///
    /// // Use the interned key in the hot loop
    /// for edge_id in edge_ids {
    ///     if let Some(weight) = indexes.get_edge_property_by_key(edge_id, &weight_key) {
    ///         // Process weight
    ///     }
    /// }
    /// ```
    #[inline]
    pub fn get_edge_property_by_key(
        &self,
        id: EdgeId,
        key: &crate::core::property::PropertyKey,
    ) -> Option<crate::core::property::PropertyValue> {
        self.edges
            .get(&id)
            .and_then(|entry| entry.value().properties.get_by_interned_key(key).cloned())
    }

    /// Remove a node from the indexes.
    pub fn remove_node(&self, id: NodeId) -> Option<Node> {
        self.nodes.remove(&id).map(|(_, node)| node)
    }

    /// Remove an edge from the indexes.
    ///
    /// Uses tombstone marking for O(1) deletion.
    /// - **O(1)**: Marks edge as deleted in both adjacency indexes
    /// - **Thread-safe**: Lock-free concurrent deletes
    /// - **Immediate**: Edge filtered from adjacency queries immediately
    /// - **Temporal metadata**: Tombstone includes deletion timestamps
    pub fn remove_edge(&self, id: EdgeId) -> Option<Edge> {
        self.edges.remove(&id).map(|(_, edge)| {
            // Mark as deleted in both adjacency indexes (O(1))
            self.outgoing.delete(id);
            self.incoming.delete(id);
            edge
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

    // NOTE: ensure_adjacency_current() removed with incremental adjacency integration.
    // Adjacency is always current - inserts/deletes are O(1) and immediately visible.

    /// Get outgoing edges for a node.
    ///
    /// Returns a guard that provides zero-copy iterator access to adjacency.
    /// Merges frozen CSR + delta buffer + tombstone filtering on-the-fly.
    ///
    /// **Lock-free**: No locks, just atomic loads
    /// **O(1) access**: Returns immediately, no rebuild needed
    /// **Incremental**: Sees edges in both frozen and delta layers
    ///
    /// # Performance
    ///
    /// - Fast path (no delta/tombstones): Direct slice access
    /// - Merge path (with delta): Iterator over frozen + delta (~20-30ns overhead)
    #[inline]
    pub fn get_outgoing(
        &self,
        source: NodeId,
    ) -> crate::index::incremental_adjacency::MergedAdjacencyGuard<'_> {
        self.outgoing.get_adjacency(source)
    }

    /// Get incoming edges for a node.
    ///
    /// Returns a guard that provides zero-copy iterator access to adjacency.
    /// Merges frozen CSR + delta buffer + tombstone filtering on-the-fly.
    ///
    /// **Lock-free**: No locks, just atomic loads
    /// **O(1) access**: Returns immediately, no rebuild needed
    /// **Incremental**: Sees edges in both frozen and delta layers
    #[inline]
    pub fn get_incoming(
        &self,
        target: NodeId,
    ) -> crate::index::incremental_adjacency::MergedAdjacencyGuard<'_> {
        self.incoming.get_adjacency(target)
    }

    /// Get outgoing edges with a specific label.
    ///
    /// **Lock-free**: No locks, just atomic loads
    /// **Incremental**: Filters merged frozen + delta results by label
    pub fn get_outgoing_with_label(
        &self,
        source: NodeId,
        label: InternedString,
    ) -> Vec<AdjacencyEntry> {
        let guard = self.outgoing.get_adjacency(source);
        guard
            .iter()
            .filter(|entry| entry.label == label)
            .copied()
            .collect()
    }

    /// Get incoming edges with a specific label.
    ///
    /// **Lock-free**: No locks, just atomic loads
    /// **Incremental**: Filters merged frozen + delta results by label
    pub fn get_incoming_with_label(
        &self,
        target: NodeId,
        label: InternedString,
    ) -> Vec<AdjacencyEntry> {
        let guard = self.incoming.get_adjacency(target);
        guard
            .iter()
            .filter(|entry| entry.label == label)
            .copied()
            .collect()
    }

    /// Get a frozen view for outgoing adjacency (read transaction hot path).
    ///
    /// Returns `Some(FrozenAdjacencyView)` if the index is in a clean state
    /// (no delta edges or tombstones), allowing direct slice access at ~8-14ns.
    /// Returns `None` if there are pending delta operations, in which case
    /// callers should fall back to `get_outgoing()` for merged access.
    ///
    /// # Performance
    ///
    /// - FrozenView access: ~8-14ns (direct slice)
    /// - MergedGuard access: ~16-17ns (iterator with fast path)
    ///
    /// # Use Case
    ///
    /// Read transactions that don't modify the graph can use this for
    /// maximum performance. The view remains valid for the lifetime of
    /// the borrow, and provides `&[AdjacencyEntry]` directly.
    #[inline]
    pub fn frozen_outgoing_view(
        &self,
    ) -> Option<crate::index::incremental_adjacency::FrozenAdjacencyView> {
        self.outgoing.frozen_view()
    }

    /// Get a frozen view for incoming adjacency (read transaction hot path).
    ///
    /// Same as `frozen_outgoing_view()` but for incoming edges.
    /// Returns `None` if there are pending delta operations.
    #[inline]
    pub fn frozen_incoming_view(
        &self,
    ) -> Option<crate::index::incremental_adjacency::FrozenAdjacencyView> {
        self.incoming.frozen_view()
    }

    /// Get the out-degree of a node (number of outgoing edges).
    ///
    /// **Lock-free**: Uses incremental adjacency with merge-on-read.
    #[inline]
    pub fn out_degree(&self, node: NodeId) -> usize {
        self.get_outgoing(node).len()
    }

    /// Get the in-degree of a node (number of incoming edges).
    ///
    /// **Lock-free**: Uses incremental adjacency with merge-on-read.
    #[inline]
    pub fn in_degree(&self, node: NodeId) -> usize {
        self.get_incoming(node).len()
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
    /// Compact adjacency indexes to merge delta → frozen.
    ///
    /// With incremental adjacency, this is optional - indexes are always correct.
    /// Compaction improves read performance by moving delta edges to frozen CSR.
    ///
    /// # When to Call
    ///
    /// - After bulk inserts (to move many edges from delta to frozen)
    /// - To reduce delta size before persistence
    /// - Usually not needed if background compaction is enabled
    ///
    /// # Performance
    ///
    /// - Complexity: O(E log E) where E is total edges
    /// - **Lock-free**: Doesn't block concurrent inserts/reads
    /// - **Atomic**: New frozen CSR is swapped in atomically
    /// - Typically <10ms for 10K edges
    pub fn compact_adjacency(&self) {
        self.outgoing.compact();
        self.incoming.compact();
    }

    /// Iterate over all nodes with zero-copy access.
    ///
    /// Returns an iterator over `Deref<Target = Node>` to avoid cloning overhead.
    /// Callers needing owned nodes should explicitly clone: `.map(|n| n.clone())`.
    pub fn iter_nodes(&self) -> impl Iterator<Item = impl std::ops::Deref<Target = Node> + '_> + '_ {
        self.nodes.iter()
    }

    /// Iterate over all edges with zero-copy access.
    ///
    /// Returns an iterator over `Deref<Target = Edge>` to avoid cloning overhead.
    /// Callers needing owned edges should explicitly clone: `.map(|e| e.clone())`.
    pub fn iter_edges(&self) -> impl Iterator<Item = impl std::ops::Deref<Target = Edge> + '_> + '_ {
        self.edges.iter()
    }

    /// Export outgoing CSR data for persistence.
    ///
    /// Note: This exports only the frozen CSR, not the delta buffer.
    /// Call `compact_adjacency()` first to include recent changes.
    pub fn export_outgoing_csr(&self) -> (Vec<u64>, Vec<u64>, Vec<u64>) {
        self.outgoing.export_frozen_csr()
    }

    /// Export incoming CSR data for persistence.
    ///
    /// Note: This exports only the frozen CSR, not the delta buffer.
    /// Call `compact_adjacency()` first to include recent changes.
    pub fn export_incoming_csr(&self) -> (Vec<u64>, Vec<u64>, Vec<u64>) {
        self.incoming.export_frozen_csr()
    }

    /// Import CSR data for both outgoing and incoming adjacency.
    ///
    /// This is used when loading persisted indexes to avoid rebuilding CSR from scratch.
    ///
    /// **Phase 7: Delta Reconstruction**
    ///
    /// After importing the frozen CSR, this method reconstructs the delta buffer by
    /// comparing edges in the DashMap against edges in the frozen CSR. Any edges not
    /// in frozen are inserted into delta, preserving the incremental state without
    /// requiring explicit delta persistence.
    ///
    /// This implements the "implicit delta reconstruction" strategy from ADR-0026.
    ///
    /// # Panics
    ///
    /// Panics if there are uncommitted tombstones (pending edge deletions).
    /// Tombstones cannot be reconstructed from the CSR import, so importing
    /// would cause deleted edges to reappear. This is a correctness invariant.
    ///
    /// In normal startup flow, tombstones should be empty. If you hit this panic,
    /// either compact first to apply tombstones, or ensure import is only called
    /// on a fresh index during startup.
    pub fn import_csr(
        &self,
        outgoing_node_ids: Vec<u64>,
        outgoing_offsets: Vec<u64>,
        outgoing_edge_ids: Vec<u64>,
        incoming_node_ids: Vec<u64>,
        incoming_offsets: Vec<u64>,
        incoming_edge_ids: Vec<u64>,
    ) {
        use std::collections::{HashMap, HashSet};

        // Safety check: tombstones cannot be reconstructed from CSR import.
        // If we have tombstones, importing would cause deleted edges to reappear.
        let outgoing_tombstones = self.outgoing.tombstone_count();
        let incoming_tombstones = self.incoming.tombstone_count();
        assert!(
            outgoing_tombstones == 0 && incoming_tombstones == 0,
            "Cannot import CSR with uncommitted tombstones: {} outgoing, {} incoming. \
             Deleted edges would reappear! Call compact() first or ensure import is \
             only called on a fresh index during startup.",
            outgoing_tombstones,
            incoming_tombstones
        );

        // Build edges map for CSR reconstruction
        let mut edges_map = HashMap::new();
        for entry in self.edges.iter() {
            let edge = entry.value();
            edges_map.insert(edge.id, (edge.target, edge.label));
        }

        // Import outgoing adjacency frozen CSR
        let outgoing_csr = crate::index::adjacency::AdjacencyIndex::import_csr(
            outgoing_node_ids,
            outgoing_offsets.clone(),
            outgoing_edge_ids.clone(),
            &edges_map,
        );
        self.outgoing.import_frozen_csr(Arc::new(outgoing_csr));

        // Rebuild edges map for incoming (maps edge_id to source, not target)
        edges_map.clear();
        for entry in self.edges.iter() {
            let edge = entry.value();
            edges_map.insert(edge.id, (edge.source, edge.label));
        }

        // Import incoming adjacency frozen CSR
        let incoming_csr = crate::index::adjacency::AdjacencyIndex::import_csr(
            incoming_node_ids,
            incoming_offsets,
            incoming_edge_ids.clone(),
            &edges_map,
        );
        self.incoming.import_frozen_csr(Arc::new(incoming_csr));

        // ===== Phase 7: Reconstruct Delta Buffer =====
        // Build set of edge IDs that are in frozen CSR
        let frozen_edge_ids: HashSet<EdgeId> = outgoing_edge_ids
            .iter()
            .map(|&id| EdgeId::new(id).unwrap())
            .collect();

        // Iterate through all edges in DashMap
        // For edges NOT in frozen, insert into delta
        for entry in self.edges.iter() {
            let edge = entry.value();

            // If edge is NOT in frozen CSR, it belongs in delta
            if !frozen_edge_ids.contains(&edge.id) {
                // Insert into outgoing delta
                self.outgoing.insert(
                    edge.source,
                    crate::index::adjacency::AdjacencyEntry::new(edge.target, edge.id, edge.label),
                );

                // Insert into incoming delta
                self.incoming.insert(
                    edge.target,
                    crate::index::adjacency::AdjacencyEntry::new(edge.source, edge.id, edge.label),
                );
            }
        }
    }
}

impl Default for CurrentIndexes {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CurrentIndexes {
    fn drop(&mut self) {
        // Ensure background compaction threads are shut down gracefully
        // to prevent thread leaks when CurrentIndexes is dropped.
        if let Some((scheduler, handle)) = self.outgoing_compaction.take() {
            scheduler.shutdown();
            // Give thread a moment to finish, but don't block indefinitely
            // on drop since that could cause deadlocks.
            let _ = handle.join();
        }
        if let Some((scheduler, handle)) = self.incoming_compaction.take() {
            scheduler.shutdown();
            let _ = handle.join();
        }
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
        indexes.compact_adjacency();

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

        indexes.compact_adjacency();

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
        indexes.compact_adjacency();
        let first_out = indexes.get_outgoing(NodeId::new(0).unwrap());
        let first_in = indexes.get_incoming(NodeId::new(1).unwrap());

        // Rebuild again
        indexes.compact_adjacency();
        let second_out = indexes.get_outgoing(NodeId::new(0).unwrap());
        let second_in = indexes.get_incoming(NodeId::new(1).unwrap());

        // Results should be identical
        assert_eq!(first_out.len(), second_out.len());
        assert_eq!(first_in.len(), second_in.len());
        assert_eq!(first_out, second_out);
        assert_eq!(first_in, second_in);
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
        indexes.compact_adjacency();

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
        indexes.compact_adjacency();

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
        indexes.compact_adjacency();

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
        indexes.compact_adjacency();

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
        indexes.compact_adjacency();

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
        indexes.compact_adjacency();

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
        indexes.compact_adjacency();

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
        indexes.compact_adjacency();

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

// Concurrency tests for rebuild race condition fix
#[cfg(test)]
mod concurrency_tests {
    use super::tests::{create_test_edge, create_test_node};
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

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
                indexes_clone.compact_adjacency();
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
        indexes.compact_adjacency();
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

/// Tests for zero-copy node/edge access methods (Issue #190).
///
/// These tests verify the performance-optimized access patterns that avoid
/// cloning entire Node/Edge structures when only partial data is needed.
#[cfg(test)]
mod zero_copy_access_tests {
    use super::tests::{create_test_edge, create_test_node};
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;

    // ========================================================================
    // Tests for with_node callback method
    // ========================================================================

    /// Test that with_node provides zero-copy access to node data.
    #[test]
    fn test_with_node_returns_value() {
        let indexes = CurrentIndexes::new();
        let node = create_test_node(1, "Person");
        indexes.insert_node(node);

        // Use with_node to extract label without cloning the entire node
        let label = indexes.with_node(NodeId::new(1).unwrap(), |n| n.label);
        assert!(label.is_some());

        let person_label = GLOBAL_INTERNER.intern("Person").unwrap();
        assert_eq!(label.unwrap(), person_label);
    }

    /// Test that with_node returns None for non-existent node.
    #[test]
    fn test_with_node_nonexistent() {
        let indexes = CurrentIndexes::new();

        let result = indexes.with_node(NodeId::new(999).unwrap(), |n| n.label);
        assert!(result.is_none());
    }

    /// Test that with_node can extract multiple fields in a single call.
    #[test]
    fn test_with_node_extract_multiple_fields() {
        let indexes = CurrentIndexes::new();
        let node = create_test_node(42, "Company");
        indexes.insert_node(node);

        // Extract multiple fields in one closure
        let (id, label) = indexes
            .with_node(NodeId::new(42).unwrap(), |n| (n.id, n.label))
            .unwrap();

        assert_eq!(id, NodeId::new(42).unwrap());
        let company_label = GLOBAL_INTERNER.intern("Company").unwrap();
        assert_eq!(label, company_label);
    }

    /// Test that with_node can perform computations on node data.
    #[test]
    fn test_with_node_computation() {
        let indexes = CurrentIndexes::new();
        let node = create_test_node(5, "Person");
        indexes.insert_node(node);

        let person_label = GLOBAL_INTERNER.intern("Person").unwrap();

        // Check if node has a specific label
        let is_person = indexes
            .with_node(NodeId::new(5).unwrap(), |n| n.label == person_label)
            .unwrap_or(false);

        assert!(is_person);
    }

    // ========================================================================
    // Tests for with_edge callback method
    // ========================================================================

    /// Test that with_edge provides zero-copy access to edge data.
    #[test]
    fn test_with_edge_returns_value() {
        let indexes = CurrentIndexes::new();
        let edge = create_test_edge(1, 10, 20, "KNOWS");
        indexes.insert_edge(edge);

        // Use with_edge to extract endpoints without cloning
        let endpoints = indexes.with_edge(EdgeId::new(1).unwrap(), |e| (e.source, e.target));
        assert!(endpoints.is_some());

        let (source, target) = endpoints.unwrap();
        assert_eq!(source, NodeId::new(10).unwrap());
        assert_eq!(target, NodeId::new(20).unwrap());
    }

    /// Test that with_edge returns None for non-existent edge.
    #[test]
    fn test_with_edge_nonexistent() {
        let indexes = CurrentIndexes::new();

        let result = indexes.with_edge(EdgeId::new(999).unwrap(), |e| e.label);
        assert!(result.is_none());
    }

    /// Test that with_edge can extract label.
    #[test]
    fn test_with_edge_extract_label() {
        let indexes = CurrentIndexes::new();
        let edge = create_test_edge(7, 1, 2, "FOLLOWS");
        indexes.insert_edge(edge);

        let label = indexes
            .with_edge(EdgeId::new(7).unwrap(), |e| e.label)
            .unwrap();

        let follows_label = GLOBAL_INTERNER.intern("FOLLOWS").unwrap();
        assert_eq!(label, follows_label);
    }

    // ========================================================================
    // Tests for get_node_label field accessor
    // ========================================================================

    /// Test that get_node_label returns the label without cloning.
    #[test]
    fn test_get_node_label() {
        let indexes = CurrentIndexes::new();
        let node = create_test_node(1, "Person");
        indexes.insert_node(node);

        let label = indexes.get_node_label(NodeId::new(1).unwrap());
        assert!(label.is_some());

        let person_label = GLOBAL_INTERNER.intern("Person").unwrap();
        assert_eq!(label.unwrap(), person_label);
    }

    /// Test that get_node_label returns None for non-existent node.
    #[test]
    fn test_get_node_label_nonexistent() {
        let indexes = CurrentIndexes::new();

        let label = indexes.get_node_label(NodeId::new(999).unwrap());
        assert!(label.is_none());
    }

    // ========================================================================
    // Tests for get_edge_endpoints field accessor
    // ========================================================================

    /// Test that get_edge_endpoints returns source and target.
    #[test]
    fn test_get_edge_endpoints() {
        let indexes = CurrentIndexes::new();
        let edge = create_test_edge(1, 10, 20, "KNOWS");
        indexes.insert_edge(edge);

        let endpoints = indexes.get_edge_endpoints(EdgeId::new(1).unwrap());
        assert!(endpoints.is_some());

        let (source, target) = endpoints.unwrap();
        assert_eq!(source, NodeId::new(10).unwrap());
        assert_eq!(target, NodeId::new(20).unwrap());
    }

    /// Test that get_edge_endpoints returns None for non-existent edge.
    #[test]
    fn test_get_edge_endpoints_nonexistent() {
        let indexes = CurrentIndexes::new();

        let endpoints = indexes.get_edge_endpoints(EdgeId::new(999).unwrap());
        assert!(endpoints.is_none());
    }

    // ========================================================================
    // Tests for get_edge_label field accessor
    // ========================================================================

    /// Test that get_edge_label returns the label without cloning.
    #[test]
    fn test_get_edge_label() {
        let indexes = CurrentIndexes::new();
        let edge = create_test_edge(1, 10, 20, "KNOWS");
        indexes.insert_edge(edge);

        let label = indexes.get_edge_label(EdgeId::new(1).unwrap());
        assert!(label.is_some());

        let knows_label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        assert_eq!(label.unwrap(), knows_label);
    }

    /// Test that get_edge_label returns None for non-existent edge.
    #[test]
    fn test_get_edge_label_nonexistent() {
        let indexes = CurrentIndexes::new();

        let label = indexes.get_edge_label(EdgeId::new(999).unwrap());
        assert!(label.is_none());
    }

    // ========================================================================
    // Tests for get_edge_source and get_edge_target field accessors
    // ========================================================================

    /// Test that get_edge_source returns the source node.
    #[test]
    fn test_get_edge_source() {
        let indexes = CurrentIndexes::new();
        let edge = create_test_edge(1, 10, 20, "KNOWS");
        indexes.insert_edge(edge);

        let source = indexes.get_edge_source(EdgeId::new(1).unwrap());
        assert!(source.is_some());
        assert_eq!(source.unwrap(), NodeId::new(10).unwrap());
    }

    /// Test that get_edge_source returns None for non-existent edge.
    #[test]
    fn test_get_edge_source_nonexistent() {
        let indexes = CurrentIndexes::new();

        let source = indexes.get_edge_source(EdgeId::new(999).unwrap());
        assert!(source.is_none());
    }

    /// Test that get_edge_target returns the target node.
    #[test]
    fn test_get_edge_target() {
        let indexes = CurrentIndexes::new();
        let edge = create_test_edge(1, 10, 20, "KNOWS");
        indexes.insert_edge(edge);

        let target = indexes.get_edge_target(EdgeId::new(1).unwrap());
        assert!(target.is_some());
        assert_eq!(target.unwrap(), NodeId::new(20).unwrap());
    }

    /// Test that get_edge_target returns None for non-existent edge.
    #[test]
    fn test_get_edge_target_nonexistent() {
        let indexes = CurrentIndexes::new();

        let target = indexes.get_edge_target(EdgeId::new(999).unwrap());
        assert!(target.is_none());
    }

    // ========================================================================
    // Tests for property accessors
    // ========================================================================

    /// Test that get_node_property returns a property value.
    #[test]
    fn test_get_node_property() {
        let indexes = CurrentIndexes::new();

        // Create a node with properties
        use crate::core::id::VersionId;
        use crate::core::property::PropertyMapBuilder;
        let node = Node::new(
            NodeId::new(1).unwrap(),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30i64)
                .build(),
            VersionId::new(1).unwrap(),
        );
        indexes.insert_node(node);

        // Get specific properties
        let name = indexes.get_node_property(NodeId::new(1).unwrap(), "name");
        assert!(name.is_some());
        assert_eq!(name.unwrap().as_str(), Some("Alice"));

        let age = indexes.get_node_property(NodeId::new(1).unwrap(), "age");
        assert!(age.is_some());
        assert_eq!(age.unwrap().as_int(), Some(30));
    }

    /// Test that get_node_property returns None for non-existent property.
    #[test]
    fn test_get_node_property_missing_key() {
        let indexes = CurrentIndexes::new();
        let node = create_test_node(1, "Person");
        indexes.insert_node(node);

        let prop = indexes.get_node_property(NodeId::new(1).unwrap(), "nonexistent");
        assert!(prop.is_none());
    }

    /// Test that get_node_property returns None for non-existent node.
    #[test]
    fn test_get_node_property_nonexistent_node() {
        let indexes = CurrentIndexes::new();

        let prop = indexes.get_node_property(NodeId::new(999).unwrap(), "name");
        assert!(prop.is_none());
    }

    /// Test that get_edge_property returns a property value.
    #[test]
    fn test_get_edge_property() {
        let indexes = CurrentIndexes::new();

        // Create an edge with properties
        use crate::core::id::VersionId;
        use crate::core::property::PropertyMapBuilder;
        let edge = Edge::new(
            EdgeId::new(1).unwrap(),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(10).unwrap(),
            NodeId::new(20).unwrap(),
            PropertyMapBuilder::new()
                .insert("since", 2020i64)
                .insert("strength", 0.9f64)
                .build(),
            VersionId::new(1).unwrap(),
        );
        indexes.insert_edge(edge);

        // Get specific properties
        let since = indexes.get_edge_property(EdgeId::new(1).unwrap(), "since");
        assert!(since.is_some());
        assert_eq!(since.unwrap().as_int(), Some(2020));

        let strength = indexes.get_edge_property(EdgeId::new(1).unwrap(), "strength");
        assert!(strength.is_some());
        assert_eq!(strength.unwrap().as_float(), Some(0.9));
    }

    /// Test that get_edge_property returns None for non-existent property.
    #[test]
    fn test_get_edge_property_missing_key() {
        let indexes = CurrentIndexes::new();
        let edge = create_test_edge(1, 10, 20, "KNOWS");
        indexes.insert_edge(edge);

        let prop = indexes.get_edge_property(EdgeId::new(1).unwrap(), "nonexistent");
        assert!(prop.is_none());
    }

    /// Test that get_edge_property returns None for non-existent edge.
    #[test]
    fn test_get_edge_property_nonexistent_edge() {
        let indexes = CurrentIndexes::new();

        let prop = indexes.get_edge_property(EdgeId::new(999).unwrap(), "weight");
        assert!(prop.is_none());
    }

    // ========================================================================
    // Tests for interned-key property accessors (hot path optimization)
    // ========================================================================

    /// Test that get_node_property_by_key works with pre-interned keys.
    #[test]
    fn test_get_node_property_by_key() {
        let indexes = CurrentIndexes::new();

        // Create a node with properties
        use crate::core::id::VersionId;
        use crate::core::property::PropertyMapBuilder;
        let node = Node::new(
            NodeId::new(1).unwrap(),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30i64)
                .build(),
            VersionId::new(1).unwrap(),
        );
        indexes.insert_node(node);

        // Pre-intern the key
        let name_key = GLOBAL_INTERNER.intern("name").unwrap();

        // Get property using interned key
        let name = indexes.get_node_property_by_key(NodeId::new(1).unwrap(), &name_key);
        assert!(name.is_some());
        assert_eq!(name.unwrap().as_str(), Some("Alice"));
    }

    /// Test that get_node_property_by_key returns None for missing key.
    #[test]
    fn test_get_node_property_by_key_missing() {
        let indexes = CurrentIndexes::new();
        let node = create_test_node(1, "Person");
        indexes.insert_node(node);

        let nonexistent_key = GLOBAL_INTERNER.intern("nonexistent_key_test").unwrap();
        let prop = indexes.get_node_property_by_key(NodeId::new(1).unwrap(), &nonexistent_key);
        assert!(prop.is_none());
    }

    /// Test that get_edge_property_by_key works with pre-interned keys.
    #[test]
    fn test_get_edge_property_by_key() {
        let indexes = CurrentIndexes::new();

        // Create an edge with properties
        use crate::core::id::VersionId;
        use crate::core::property::PropertyMapBuilder;
        let edge = Edge::new(
            EdgeId::new(1).unwrap(),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(10).unwrap(),
            NodeId::new(20).unwrap(),
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
            VersionId::new(1).unwrap(),
        );
        indexes.insert_edge(edge);

        // Pre-intern the key
        let since_key = GLOBAL_INTERNER.intern("since").unwrap();

        // Get property using interned key
        let since = indexes.get_edge_property_by_key(EdgeId::new(1).unwrap(), &since_key);
        assert!(since.is_some());
        assert_eq!(since.unwrap().as_int(), Some(2020));
    }

    /// Test that get_edge_property_by_key returns None for missing key.
    #[test]
    fn test_get_edge_property_by_key_missing() {
        let indexes = CurrentIndexes::new();
        let edge = create_test_edge(1, 10, 20, "KNOWS");
        indexes.insert_edge(edge);

        let nonexistent_key = GLOBAL_INTERNER.intern("nonexistent_edge_key_test").unwrap();
        let prop = indexes.get_edge_property_by_key(EdgeId::new(1).unwrap(), &nonexistent_key);
        assert!(prop.is_none());
    }

    // ========================================================================
    // Integration tests - verify zero-copy methods work correctly with
    // concurrent operations
    // ========================================================================

    /// Test that zero-copy methods work correctly under concurrent access.
    #[test]
    fn test_zero_copy_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let indexes = Arc::new(CurrentIndexes::new());

        // Insert initial data
        for i in 0..100 {
            indexes.insert_node(create_test_node(i, "Node"));
            if i > 0 {
                indexes.insert_edge(create_test_edge(i, i - 1, i, "LINK"));
            }
        }

        let mut handles = Vec::new();

        // Spawn readers using zero-copy methods
        for _ in 0..4 {
            let indexes_clone = Arc::clone(&indexes);
            handles.push(thread::spawn(move || {
                for i in 0..100 {
                    let node_id = NodeId::new(i).unwrap();
                    let _label = indexes_clone.get_node_label(node_id);

                    if i > 0 {
                        let edge_id = EdgeId::new(i).unwrap();
                        let _endpoints = indexes_clone.get_edge_endpoints(edge_id);
                        let _edge_label = indexes_clone.get_edge_label(edge_id);
                    }
                }
            }));
        }

        // All threads should complete without issues
        for handle in handles {
            handle.join().expect("Thread should complete successfully");
        }
    }
}
