//! Current-state indexes using concurrent data structures.
//!
//! This module provides indexes for the current state of the graph using
//! DashMap for lock-free concurrent access. These are the "hot path" indexes
//! that must be extremely fast.

use crate::core::graph::{Edge, Node, NodeHeader};
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
    /// Node ID → Node (O(1) lookup, cold path with full PropertyMap)
    nodes: DashMap<NodeId, Node>,
    /// Node ID → NodeHeader (hot path for label filtering, 16 bytes per entry)
    ///
    /// Kept in sync with `nodes`: every `insert_node` writes a header and every
    /// `remove_node` removes it. Scanning this map for label matches touches far
    /// less memory than scanning the full `nodes` map.
    node_headers: DashMap<NodeId, NodeHeader>,
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
            node_headers: DashMap::new(),
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
            node_headers: DashMap::new(),
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
    ///
    /// Also inserts a [`NodeHeader`] into the hot-path header map so label
    /// filtering via [`filter_nodes_by_label`](Self::filter_nodes_by_label)
    /// can scan 16-byte headers instead of full nodes.
    pub fn insert_node(&self, node: Node) {
        let header = NodeHeader::from(&node);
        self.nodes.insert(node.id, node);
        self.node_headers.insert(header.id, header);
    }

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

    /// Mutably access a node without replacing it, executing a closure on mutable node data.
    ///
    /// After the closure runs, the [`NodeHeader`] is refreshed so that
    /// `get_node_header` and `filter_nodes_by_label` reflect any label change
    /// made inside the closure.
    pub fn with_node_mut<F>(&self, id: NodeId, f: F)
    where
        F: FnOnce(&mut Node),
    {
        if let Some(mut entry) = self.nodes.get_mut(&id) {
            f(entry.value_mut());
            // Refresh the header while still holding the write lock so that
            // label changes are immediately visible to filter_nodes_by_label.
            let header = NodeHeader::from(entry.value());
            self.node_headers.insert(id, header);
        }
    }

    /// Mutably access an edge without replacing it.
    pub fn with_edge_mut<F>(&self, id: EdgeId, f: F)
    where
        F: FnOnce(&mut Edge),
    {
        if let Some(mut entry) = self.edges.get_mut(&id) {
            f(entry.value_mut());
        }
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

    /// Check whether a node's label equals the given interned string, without cloning.
    ///
    /// This is the preferred hot-path API for label-filtered vector search (Issue #339).
    /// It avoids the `Option`-wrapping overhead of `get_node_label` when only a boolean
    /// answer is needed, saving a branch and keeping the closure passed to the HNSW
    /// filter as minimal as possible.
    ///
    /// Returns `false` if the node does not exist.
    #[inline]
    pub fn check_node_label(&self, id: NodeId, expected: InternedString) -> bool {
        self.nodes
            .get(&id)
            .is_some_and(|entry| entry.value().label == expected)
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
    /// use aletheiadb::core::interning::GLOBAL_INTERNER;
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
    /// use aletheiadb::core::interning::GLOBAL_INTERNER;
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
    ///
    /// Removes from the primary node map first, then removes the header only
    /// if the node was actually present. This avoids a brief window where the
    /// header is absent but the node still exists, which could cause a
    /// label-filter miss under concurrent reads.
    pub fn remove_node(&self, id: NodeId) -> Option<Node> {
        self.nodes.remove(&id).map(|(_, node)| {
            self.node_headers.remove(&id);
            node
        })
    }

    /// Get the hot-path header for a node by ID.
    ///
    /// Returns the 16-byte [`NodeHeader`] (id + label) without touching the
    /// full node struct or its `PropertyMap`. Useful when the caller only
    /// needs the label, e.g. for quick existence or type checks.
    ///
    /// Returns `None` if the node does not exist.
    #[inline]
    pub fn get_node_header(&self, id: NodeId) -> Option<NodeHeader> {
        self.node_headers.get(&id).map(|entry| *entry.value())
    }

    /// Return an iterator over the IDs of all nodes whose label matches `label`.
    ///
    /// Scans the compact header map (16 bytes per entry) instead of the full
    /// node map, reducing memory bandwidth by ~10-30x for large property maps.
    /// No allocation occurs until the caller collects the iterator.
    ///
    /// To limit results, chain `.take(n)` on the returned iterator.
    ///
    /// # Performance
    ///
    /// - Reads 16 bytes per candidate (header) vs 100-500+ bytes (full node)
    /// - Zero allocation during scan; caller controls when/whether to collect
    pub fn filter_nodes_by_label(
        &self,
        label: InternedString,
    ) -> impl Iterator<Item = NodeId> + '_ {
        self.node_headers
            .iter()
            .filter(move |entry| entry.value().label == label)
            .map(|entry| *entry.key())
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
        let mut result = Vec::with_capacity(guard.capacity_hint());
        result.extend(guard.iter().filter(|entry| entry.label == label).copied());
        result
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
        let mut result = Vec::with_capacity(guard.capacity_hint());
        result.extend(guard.iter().filter(|entry| entry.label == label).copied());
        result
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

    /// Iterate over all nodes.
    ///
    /// Returns an iterator over DashMap guard references to avoid cloning.
    /// If you need owned `Node` values, call `.map(|n| n.clone())` on the iterator.
    ///
    /// # Performance
    ///
    /// - **Zero allocation**: Returns guards/references to nodes without cloning (~56 bytes + Arc overhead)
    /// - **Efficient**: Avoids implicit cloning in hot loops
    pub fn iter_nodes(
        &self,
    ) -> impl Iterator<Item = impl std::ops::Deref<Target = Node> + '_> + '_ {
        self.nodes.iter()
    }

    /// Iterate over all node IDs.
    ///
    /// This is more efficient than `iter_nodes().map(|n| n.id)` when only
    /// IDs are needed, as it avoids cloning the full Node objects.
    pub fn iter_node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.iter().map(|entry| *entry.key())
    }

    /// Iterate over all edges.
    ///
    /// Returns an iterator over DashMap guard references to avoid cloning.
    /// If you need owned `Edge` values, call `.map(|e| e.clone())` on the iterator.
    ///
    /// # Performance
    ///
    /// - **Zero allocation**: Returns guards/references to edges without cloning (~72 bytes + Arc overhead)
    /// - **Efficient**: Avoids implicit cloning in hot loops (benchmark: ~370µs -> ~200µs for 10k edges)
    pub fn iter_edges(
        &self,
    ) -> impl Iterator<Item = impl std::ops::Deref<Target = Edge> + '_> + '_ {
        self.edges.iter()
    }

    /// Iterate over all edge IDs.
    ///
    /// This is more efficient than `iter_edges().map(|e| e.id)` when only
    /// IDs are needed, as it avoids cloning the full Edge objects.
    pub fn iter_edge_ids(&self) -> impl Iterator<Item = EdgeId> + '_ {
        self.edges.iter().map(|entry| *entry.key())
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
        use crate::core::hasher::IdentityHasher;
        use std::collections::{HashMap, HashSet};
        use std::hash::BuildHasherDefault;

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
        let mut edges_map = HashMap::with_hasher(BuildHasherDefault::<IdentityHasher>::default());
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

    #[test]
    fn test_iter_nodes_returns_references() {
        let indexes = CurrentIndexes::new();
        let node = create_test_node(1, "Person");
        indexes.insert_node(node.clone());

        // Should be able to deref without clone
        let collected: Vec<_> = indexes.iter_nodes().map(|n| n.id).collect();
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0], node.id);

        // Should be able to clone when needed
        let cloned: Vec<Node> = indexes.iter_nodes().map(|n| n.clone()).collect();
        assert_eq!(cloned.len(), 1);
        assert_eq!(cloned[0].id, node.id);
    }

    #[test]
    fn test_iter_edges_returns_references() {
        let indexes = CurrentIndexes::new();
        let edge = create_test_edge(1, 0, 1, "KNOWS");
        indexes.insert_edge(edge.clone());

        // Should be able to deref without clone
        let collected: Vec<_> = indexes.iter_edges().map(|e| e.id).collect();
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0], edge.id);

        // Should be able to clone when needed
        let cloned: Vec<Edge> = indexes.iter_edges().map(|e| e.clone()).collect();
        assert_eq!(cloned.len(), 1);
        assert_eq!(cloned[0].id, edge.id);
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

    #[test]
    fn test_iter_node_ids_optimization() {
        let indexes = CurrentIndexes::new();

        // Create nodes with properties to verify we're not cloning them
        let n1 = create_test_node(1, "Person");
        let n2 = create_test_node(2, "Person");
        indexes.insert_node(n1);
        indexes.insert_node(n2);

        // Collect IDs using new optimized method
        let ids: Vec<NodeId> = indexes.iter_node_ids().collect();

        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&NodeId::new(1).unwrap()));
        assert!(ids.contains(&NodeId::new(2).unwrap()));
    }

    #[test]
    fn test_iter_edge_ids_optimization() {
        let indexes = CurrentIndexes::new();

        let e1 = create_test_edge(1, 0, 1, "KNOWS");
        let e2 = create_test_edge(2, 1, 2, "FOLLOWS");
        indexes.insert_edge(e1);
        indexes.insert_edge(e2);

        let ids: Vec<EdgeId> = indexes.iter_edge_ids().collect();

        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&EdgeId::new(1).unwrap()));
        assert!(ids.contains(&EdgeId::new(2).unwrap()));
    }

    // ========================================================================
    // Tests for check_node_label (Issue #339)
    // ========================================================================

    /// check_node_label returns true when the node exists and its label matches.
    #[test]
    fn test_check_node_label_matches() {
        let indexes = CurrentIndexes::new();
        let node = create_test_node(1, "Person");
        indexes.insert_node(node);

        let person_label = GLOBAL_INTERNER.intern("Person").unwrap();
        assert!(indexes.check_node_label(NodeId::new(1).unwrap(), person_label));
    }

    /// check_node_label returns false when the node exists but its label differs.
    #[test]
    fn test_check_node_label_mismatch() {
        let indexes = CurrentIndexes::new();
        let node = create_test_node(1, "Person");
        indexes.insert_node(node);

        let company_label = GLOBAL_INTERNER.intern("Company").unwrap();
        assert!(!indexes.check_node_label(NodeId::new(1).unwrap(), company_label));
    }

    /// check_node_label returns false for a node that does not exist.
    #[test]
    fn test_check_node_label_nonexistent() {
        let indexes = CurrentIndexes::new();

        let person_label = GLOBAL_INTERNER.intern("Person").unwrap();
        assert!(!indexes.check_node_label(NodeId::new(999).unwrap(), person_label));
    }

    /// check_node_label is consistent with get_node_label for the same node.
    #[test]
    fn test_check_node_label_consistent_with_get_node_label() {
        let indexes = CurrentIndexes::new();
        let node = create_test_node(5, "Document");
        indexes.insert_node(node);

        let doc_label = GLOBAL_INTERNER.intern("Document").unwrap();
        let other_label = GLOBAL_INTERNER.intern("Article").unwrap();

        // check_node_label should agree with manual get_node_label comparison
        let node_id = NodeId::new(5).unwrap();
        let via_get = indexes
            .get_node_label(node_id)
            .map(|l| l == doc_label)
            .unwrap_or(false);
        assert_eq!(indexes.check_node_label(node_id, doc_label), via_get);

        let via_get_other = indexes
            .get_node_label(node_id)
            .map(|l| l == other_label)
            .unwrap_or(false);
        assert_eq!(
            indexes.check_node_label(node_id, other_label),
            via_get_other
        );
    }
}

/// Tests for hot/cold path split via NodeHeader (Issue #340).
///
/// NodeHeader stores only id+label (16 bytes) so label-filtering scans
/// touch far less memory than iterating full Node structs.
#[cfg(test)]
mod node_header_index_tests {
    use super::tests::create_test_node;
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;

    #[test]
    fn test_insert_node_creates_header() {
        let indexes = CurrentIndexes::new();
        indexes.insert_node(create_test_node(1, "Person"));

        let header = indexes.get_node_header(NodeId::new(1).unwrap());
        assert!(
            header.is_some(),
            "inserting a node should create its header"
        );

        let h = header.unwrap();
        assert_eq!(h.id, NodeId::new(1).unwrap());
        let person_label = GLOBAL_INTERNER.intern("Person").unwrap();
        assert_eq!(h.label, person_label);
    }

    #[test]
    fn test_remove_node_removes_header() {
        let indexes = CurrentIndexes::new();
        indexes.insert_node(create_test_node(2, "Person"));
        assert!(indexes.get_node_header(NodeId::new(2).unwrap()).is_some());

        indexes.remove_node(NodeId::new(2).unwrap());
        assert!(
            indexes.get_node_header(NodeId::new(2).unwrap()).is_none(),
            "removing a node must also remove its header"
        );
    }

    #[test]
    fn test_get_node_header_nonexistent() {
        let indexes = CurrentIndexes::new();
        assert!(indexes.get_node_header(NodeId::new(999).unwrap()).is_none());
    }

    #[test]
    fn test_filter_nodes_by_label_matches() {
        let indexes = CurrentIndexes::new();
        indexes.insert_node(create_test_node(1, "Person"));
        indexes.insert_node(create_test_node(2, "Person"));
        indexes.insert_node(create_test_node(3, "Company"));

        let person_label = GLOBAL_INTERNER.intern("Person").unwrap();
        let mut ids: Vec<_> = indexes.filter_nodes_by_label(person_label).collect();
        ids.sort();

        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], NodeId::new(1).unwrap());
        assert_eq!(ids[1], NodeId::new(2).unwrap());
    }

    #[test]
    fn test_filter_nodes_by_label_no_match() {
        let indexes = CurrentIndexes::new();
        indexes.insert_node(create_test_node(1, "Person"));

        let company_label = GLOBAL_INTERNER.intern("Company").unwrap();
        assert_eq!(indexes.filter_nodes_by_label(company_label).count(), 0);
    }

    #[test]
    fn test_filter_nodes_by_label_all_match() {
        let indexes = CurrentIndexes::new();
        for i in 1u64..=5 {
            indexes.insert_node(create_test_node(i, "Event"));
        }

        let event_label = GLOBAL_INTERNER.intern("Event").unwrap();
        assert_eq!(indexes.filter_nodes_by_label(event_label).count(), 5);
    }

    #[test]
    fn test_filter_nodes_by_label_empty_index() {
        let indexes = CurrentIndexes::new();
        let label = GLOBAL_INTERNER.intern("Anything").unwrap();
        assert_eq!(indexes.filter_nodes_by_label(label).count(), 0);
    }

    #[test]
    fn test_header_consistent_with_node() {
        let indexes = CurrentIndexes::new();
        indexes.insert_node(create_test_node(10, "User"));

        let node = indexes.get_node(NodeId::new(10).unwrap()).unwrap();
        let header = indexes.get_node_header(NodeId::new(10).unwrap()).unwrap();

        assert_eq!(header.id, node.id);
        assert_eq!(header.label, node.label);
    }

    #[test]
    fn test_header_updated_after_label_mutation_via_with_node_mut() {
        let indexes = CurrentIndexes::new();
        indexes.insert_node(create_test_node(20, "Person"));

        let user_label = GLOBAL_INTERNER.intern("User").unwrap();
        indexes.with_node_mut(NodeId::new(20).unwrap(), |n| {
            n.label = user_label;
        });

        // Header must reflect the new label immediately
        let header = indexes.get_node_header(NodeId::new(20).unwrap()).unwrap();
        assert_eq!(
            header.label, user_label,
            "header must reflect mutated label"
        );

        // filter_nodes_by_label must use updated header
        let person_label = GLOBAL_INTERNER.intern("Person").unwrap();
        assert_eq!(
            indexes.filter_nodes_by_label(person_label).count(),
            0,
            "old label should no longer match"
        );
        assert_eq!(
            indexes.filter_nodes_by_label(user_label).count(),
            1,
            "new label should match"
        );
    }
}

#[cfg(test)]
pub(crate) mod tests;
