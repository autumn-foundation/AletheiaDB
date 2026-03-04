//! Result Iterators
//!
//! Pull-based iterators for query execution. Each physical operator
//! has a corresponding iterator that lazily produces results.

use parking_lot::RwLock;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet, VecDeque};
use std::sync::Arc;

#[cfg(feature = "observability")]
use tracing;

use crate::core::error::Result;
use crate::core::graph::Node;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyValue;
use crate::core::vector::cosine_similarity;
use crate::core::{NodeId, Timestamp};
use crate::query::ir::{Direction, Predicate, PredicateValue};
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;

use super::results::{EntityId, EntityResult, QueryRow};

/// Trait for result iteration (pull-based).
pub trait ResultIterator: Send {
    /// Get the next result row
    fn next(&mut self) -> Option<Result<QueryRow>>;

    /// Estimate the remaining results
    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, None)
    }
}

/// Empty iterator that produces no results.
pub struct EmptyIterator;

impl ResultIterator for EmptyIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(0))
    }
}

/// Iterator for direct node lookups.
pub struct NodeLookupIterator {
    node_ids: std::vec::IntoIter<NodeId>,
    current: Arc<CurrentStorage>,
}

impl NodeLookupIterator {
    pub fn new(node_ids: Vec<NodeId>, current: Arc<CurrentStorage>) -> Self {
        NodeLookupIterator {
            node_ids: node_ids.into_iter(),
            current,
        }
    }
}

impl ResultIterator for NodeLookupIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.node_ids.next().map(|id| {
            self.current
                .get_node(id)
                .map(|node| QueryRow::from_entity(EntityResult::Node(node)))
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.node_ids.size_hint()
    }
}

/// Iterator for node scans with optional label filter.
///
/// # Memory Considerations
///
/// **WARNING**: This iterator collects all node IDs into a `Vec` upfront during
/// initialization. For very large graphs (millions of nodes), this can cause:
///
/// - **High memory consumption**: O(n) where n = number of nodes
/// - **Initial latency**: Delay before the first result is produced
///
/// This design is a trade-off due to the `Send` bound on `ResultIterator` and
/// the fact that DashMap's iterators hold internal locks that cannot be sent
/// across threads. The current implementation prioritizes correctness and
/// simplicity over optimal memory usage for full scans.
///
/// ## Mitigation Strategies
///
/// For production workloads with large graphs:
/// 1. **Use label filters** - `scan(Some("Person"))` limits the scan scope
/// 2. **Use LIMIT** - Add `.limit(n)` to queries to enable early termination
/// 3. **Prefer targeted queries** - Use `start(node_id)` instead of full scans
///
/// ## Future Improvements (Issue #307)
///
/// Possible optimizations include:
/// - Streaming iteration using channels (`std::sync::mpsc`)
/// - Chunked iteration to limit memory per batch
/// - Index-based iteration that doesn't require holding locks
pub struct NodeScanIterator {
    label: Option<String>,
    current: Arc<CurrentStorage>,
    initialized: bool,
    node_ids: Option<std::vec::IntoIter<NodeId>>,
}

impl NodeScanIterator {
    /// Create a new NodeScanIterator.
    pub fn new(label: Option<String>, current: Arc<CurrentStorage>) -> Self {
        NodeScanIterator {
            label,
            current,
            initialized: false,
            node_ids: None,
        }
    }

    fn initialize(&mut self) {
        if self.initialized {
            return;
        }
        self.initialized = true;

        // Collect all node IDs upfront.
        //
        // NOTE: This is a known memory concern for large graphs. See the struct
        // documentation above for details and mitigation strategies.
        //
        // The current implementation trades memory efficiency for correctness:
        // DashMap iterators cannot be sent across threads (not Send), and the
        // ResultIterator trait requires Send for parallel query execution.
        let ids: Vec<NodeId> = self.current.get_all_node_ids();
        self.node_ids = Some(ids.into_iter());
    }
}

impl ResultIterator for NodeScanIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.initialize();

        loop {
            match self.node_ids.as_mut()?.next() {
                Some(id) => {
                    match self.current.get_node(id) {
                        Ok(node) => {
                            // Check label filter by comparing InternedString IDs
                            if let Some(ref label_str) = self.label {
                                // Get the InternedString ID for the filter label
                                let label_id = GLOBAL_INTERNER.get_id(label_str);
                                if label_id != Some(node.label) {
                                    continue; // Skip this node
                                }
                            }
                            return Some(Ok(QueryRow::from_entity(EntityResult::Node(node))));
                        }
                        Err(e) => return Some(Err(e)),
                    }
                }
                None => return None,
            }
        }
    }
}

/// Iterator for vector search results.
pub struct VectorResultIterator {
    results: std::vec::IntoIter<(NodeId, f32)>,
    current: Arc<CurrentStorage>,
}

impl VectorResultIterator {
    pub fn new(results: Vec<(NodeId, f32)>, current: Arc<CurrentStorage>) -> Self {
        VectorResultIterator {
            results: results.into_iter(),
            current,
        }
    }
}

impl ResultIterator for VectorResultIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.results.next().map(|(node_id, score)| {
            self.current
                .get_node(node_id)
                .map(|node| QueryRow::with_score(EntityResult::Node(node), score))
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.results.size_hint()
    }
}

/// Iterator for temporal node lookups.
///
/// This iterator reconstructs nodes at a specific point in bi-temporal time
/// by querying the historical storage for the appropriate version and
/// reconstructing properties using the anchor+delta compression strategy.
///
/// The reconstruction process:
/// 1. Find the version valid at the requested (valid_time, transaction_time)
/// 2. Reconstruct properties from the version using anchor+delta
/// 3. Return a Node with the historical label and properties
pub struct TemporalNodeIterator {
    node_ids: std::vec::IntoIter<NodeId>,
    valid_time: Timestamp,
    transaction_time: Timestamp,
    historical: Arc<RwLock<HistoricalStorage>>,
}

impl TemporalNodeIterator {
    pub fn new(
        node_ids: Vec<NodeId>,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        historical: Arc<RwLock<HistoricalStorage>>,
    ) -> Self {
        TemporalNodeIterator {
            node_ids: node_ids.into_iter(),
            valid_time,
            transaction_time,
            historical,
        }
    }
}

impl ResultIterator for TemporalNodeIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.node_ids.next().map(|id| {
            // Acquire read lock on historical storage (per-node)
            // For bulk queries, use BatchTemporalNodeIterator instead
            let historical = self.historical.read();

            // Find the version valid at the requested time
            let version_id =
                historical
                    .find_node_version_at_time(id, self.valid_time, self.transaction_time)
                    .ok_or(crate::core::error::TemporalError::NodeNotFoundAtTime {
                        node_id: id,
                        valid_time: self.valid_time,
                        transaction_time: self.transaction_time,
                    })?;

            // INVARIANT: If find_node_version_at_time returns a version_id,
            // that version MUST exist in storage. If this fails, it indicates
            // a critical data inconsistency (broken version chain or dangling version_id).
            debug_assert!(
                historical.get_node_version(version_id).is_some(),
                "INVARIANT VIOLATION: find_node_version_at_time returned non-existent version_id {}",
                version_id
            );

            // Get the version metadata
            let version = historical
                .get_node_version(version_id)
                .ok_or(crate::core::error::TemporalError::VersionNotFound(version_id))?;

            // Reconstruct the properties from the version
            let properties = historical.reconstruct_node_properties(version_id)?;

            // Construct a node with the historical data
            let node = Node::new(id, version.label, properties, version_id);

            Ok(QueryRow::from_entity(EntityResult::Node(node)).at_time(self.valid_time))
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.node_ids.size_hint()
    }
}

/// Batch temporal node iterator for bulk queries.
///
/// This iterator is optimized for querying many nodes at once by acquiring
/// a single read lock, reconstructing all nodes at once, then releasing the lock.
///
/// **Performance**: Use this for bulk queries (>100 nodes) where lock acquisition
/// overhead is significant. For small queries, use `TemporalNodeIterator` instead.
///
/// **Trade-off**: Collects all results eagerly during construction, which requires
/// more memory upfront but avoids per-node lock overhead and allows the lock to be
/// released immediately after construction.
pub struct BatchTemporalNodeIterator {
    results: std::vec::IntoIter<Result<QueryRow>>,
}

impl BatchTemporalNodeIterator {
    /// Create a new batch temporal node iterator.
    ///
    /// Acquires the historical storage lock once, reconstructs all nodes,
    /// then releases the lock and returns the iterator over results.
    ///
    /// # Errors
    /// Returns an error if the historical storage lock is poisoned.
    pub fn new(
        node_ids: Vec<NodeId>,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        historical: Arc<RwLock<HistoricalStorage>>,
    ) -> Result<Self> {
        // Acquire lock once for all nodes
        let guard = historical.read();

        // Reconstruct all nodes while holding the lock
        let results: Vec<Result<QueryRow>> = node_ids
            .into_iter()
            .map(|id| {
                // Find the version valid at the requested time
                let version_id = guard
                    .find_node_version_at_time(id, valid_time, transaction_time)
                    .ok_or(crate::core::error::TemporalError::NodeNotFoundAtTime {
                        node_id: id,
                        valid_time,
                        transaction_time,
                    })?;

                // INVARIANT: If find_node_version_at_time returns a version_id,
                // that version MUST exist in storage. If this fails, it indicates
                // a critical data inconsistency (broken version chain or dangling version_id).
                debug_assert!(
                    guard.get_node_version(version_id).is_some(),
                    "INVARIANT VIOLATION: find_node_version_at_time returned non-existent version_id {}",
                    version_id
                );

                // Get the version metadata
                let version = guard
                    .get_node_version(version_id)
                    .ok_or(crate::core::error::TemporalError::VersionNotFound(version_id))?;

                // Reconstruct the properties from the version
                let properties = guard.reconstruct_node_properties(version_id)?;

                // Construct a node with the historical data
                let node = Node::new(id, version.label, properties, version_id);

                Ok(QueryRow::from_entity(EntityResult::Node(node)).at_time(valid_time))
            })
            .collect();

        // Lock is automatically released here when `guard` goes out of scope

        Ok(BatchTemporalNodeIterator {
            results: results.into_iter(),
        })
    }
}

impl ResultIterator for BatchTemporalNodeIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.results.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.results.size_hint()
    }
}

/// Iterator for temporal node lookups with optional label filtering.
///
/// This iterator addresses the deep nesting issue (#356) by extracting the
/// filtering logic into well-defined helper methods:
///
/// - `get_temporal_version()` - Retrieves a node at a specific point in bi-temporal time
/// - `apply_label_filter()` - Checks if a node matches the optional label filter
/// - `filter_node()` - Orchestrates the filtering logic with maximum 2-3 levels of nesting
///
/// ## Design Rationale
///
/// Instead of deeply nested conditionals (8+ levels), this design:
/// 1. Separates concerns into small, focused methods
/// 2. Keeps each method at 2-3 levels of nesting maximum
/// 3. Makes each component independently testable
/// 4. Improves readability and maintainability
///
/// ## Lock Duration Trade-off
///
/// The `next()` method holds the historical read lock for the entire iteration
/// loop until a matching node is found. This is intentional:
/// - **Advantage**: Avoids lock thrashing (acquiring/releasing on every node)
/// - **Trade-off**: For large result sets with many filtered-out nodes, the lock
///   may be held longer, potentially increasing writer latency
///
/// For bulk queries where this is a concern, consider using `BatchTemporalNodeIterator`
/// which processes all nodes upfront and releases the lock immediately.
///
/// ## Example
///
/// ```ignore
/// let iter = TemporalNodeScanIterator::new(
///     node_ids,
///     valid_time,
///     transaction_time,
///     historical,
///     Some("Person".to_string()), // Optional label filter
/// );
///
/// for result in iter {
///     // Only Person nodes at the specified time point
/// }
/// ```
pub struct TemporalNodeScanIterator {
    node_ids: std::vec::IntoIter<NodeId>,
    valid_time: Timestamp,
    transaction_time: Timestamp,
    historical: Arc<RwLock<HistoricalStorage>>,
    /// Optional label filter - if Some, only nodes with matching label are returned
    label_filter: Option<String>,
    /// Pre-computed interned ID of the label filter for efficient comparison.
    /// Avoids repeated hashmap lookups in apply_label_filter().
    interned_label_filter: Option<crate::core::interning::InternedString>,
}

impl TemporalNodeScanIterator {
    /// Create a new temporal node scan iterator.
    ///
    /// # Arguments
    ///
    /// * `node_ids` - The node IDs to iterate over
    /// * `valid_time` - The valid time for temporal reconstruction
    /// * `transaction_time` - The transaction time for temporal reconstruction
    /// * `historical` - Reference to historical storage
    /// * `label_filter` - Optional label to filter nodes by
    pub fn new(
        node_ids: Vec<NodeId>,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        historical: Arc<RwLock<HistoricalStorage>>,
        label_filter: Option<String>,
    ) -> Self {
        // Pre-compute the interned label ID once during construction
        // to avoid repeated hashmap lookups during iteration
        let interned_label_filter = label_filter
            .as_ref()
            .and_then(|label| GLOBAL_INTERNER.get_id(label));

        TemporalNodeScanIterator {
            node_ids: node_ids.into_iter(),
            valid_time,
            transaction_time,
            historical,
            label_filter,
            interned_label_filter,
        }
    }

    /// Retrieve the temporal version of a node at the configured time point.
    ///
    /// This helper method encapsulates the temporal reconstruction logic:
    /// 1. Find the version valid at (valid_time, transaction_time)
    /// 2. Retrieve the version metadata
    /// 3. Reconstruct properties from anchor+delta compression
    /// 4. Return a fully reconstructed Node
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No version exists at the specified time point
    /// - Version metadata is missing (data inconsistency)
    /// - Property reconstruction fails
    pub(crate) fn get_temporal_version(
        &self,
        node_id: NodeId,
        guard: &parking_lot::RwLockReadGuard<'_, HistoricalStorage>,
    ) -> Result<Node> {
        // Step 1: Find the version valid at the requested time
        let version_id = guard
            .find_node_version_at_time(node_id, self.valid_time, self.transaction_time)
            .ok_or(crate::core::error::TemporalError::NodeNotFoundAtTime {
                node_id,
                valid_time: self.valid_time,
                transaction_time: self.transaction_time,
            })?;

        // INVARIANT: If find_node_version_at_time returns a version_id,
        // that version MUST exist in storage. If this fails, it indicates
        // a critical data inconsistency (broken version chain or dangling version_id).
        debug_assert!(
            guard.get_node_version(version_id).is_some(),
            "INVARIANT VIOLATION: find_node_version_at_time returned non-existent version_id {}",
            version_id
        );

        // Step 2: Get the version metadata
        let version = guard.get_node_version(version_id).ok_or(
            crate::core::error::TemporalError::VersionNotFound(version_id),
        )?;

        // Step 3: Reconstruct properties
        let properties = guard.reconstruct_node_properties(version_id)?;

        // Step 4: Build and return the node
        Ok(Node::new(node_id, version.label, properties, version_id))
    }

    /// Check if a node passes the label filter.
    ///
    /// Returns `true` if:
    /// - No label filter is configured (all nodes pass)
    /// - The node's label matches the filter
    ///
    /// Returns `false` if:
    /// - The node's label doesn't match the filter
    /// - The filter label doesn't exist in the interner (no nodes can match)
    ///
    /// Uses the pre-computed interned label ID for O(1) comparison.
    #[inline]
    pub(crate) fn apply_label_filter(&self, node: &Node) -> bool {
        match (&self.label_filter, self.interned_label_filter) {
            (None, _) => true,        // No filter, all nodes pass
            (Some(_), None) => false, // Filter label doesn't exist, no nodes match
            (Some(_), Some(filter_id)) => filter_id == node.label,
        }
    }

    /// Orchestrate the filtering logic for a single node.
    ///
    /// This method combines temporal reconstruction with label filtering
    /// while maintaining flat control flow (2-3 levels of nesting max).
    ///
    /// # Returns
    ///
    /// - `Some(Ok(QueryRow))` - Node exists at time point and passes label filter
    /// - `Some(Err(...))` - Node lookup failed (error should be propagated)
    /// - `None` - Node exists but doesn't pass label filter (skip to next)
    pub(crate) fn filter_node(
        &self,
        node_id: NodeId,
        guard: &parking_lot::RwLockReadGuard<'_, HistoricalStorage>,
    ) -> Option<Result<QueryRow>> {
        // Step 1: Get the temporal version
        let node = match self.get_temporal_version(node_id, guard) {
            Ok(n) => n,
            Err(e) => return Some(Err(e)),
        };

        // Step 2: Apply label filter
        if !self.apply_label_filter(&node) {
            return None; // Skip this node
        }

        // Step 3: Build and return the query row
        Some(Ok(
            QueryRow::from_entity(EntityResult::Node(node)).at_time(self.valid_time)
        ))
    }
}

impl ResultIterator for TemporalNodeScanIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        // Acquire read lock once for the duration of finding the next valid node
        let guard = self.historical.read();

        loop {
            let node_id = self.node_ids.next()?;

            match self.filter_node(node_id, &guard) {
                Some(result) => return Some(result), // Found valid node or error
                None => continue,                    // Label filter didn't match, try next
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (_lower, upper) = self.node_ids.size_hint();
        // When a label filter is active, this iterator may skip node IDs,
        // so we cannot safely use the underlying lower bound.
        // Upper bound remains valid as we can't return more than remaining IDs.
        if self.label_filter.is_some() {
            (0, upper)
        } else {
            // No label filtering: all remaining node_ids will be yielded
            // (assuming they exist in storage at the requested time point).
            self.node_ids.size_hint()
        }
    }
}

/// Iterator for graph traversal using BFS.
///
/// # Deduplication Semantics
///
/// The `visited` set is cleared for each new input node. This means:
/// - Each input node gets independent traversal results
/// - If multiple input nodes can reach the same target, it appears multiple times
/// - This is intentional for path-based semantics (e.g., "all friends of each person")
///
/// For global deduplication across all inputs, wrap the output in a `DistinctIterator`.
///
/// # Example
///
/// ```text
/// Input: [A, B]
/// Graph: A → C, B → C
///
/// Output: [C (from A), C (from B)]  // C appears twice
/// ```
pub struct TraversalIterator {
    input: Box<dyn ResultIterator>,
    direction: Direction,
    label: Option<String>,
    depth: usize,
    current: Arc<CurrentStorage>,
    historical: Arc<RwLock<HistoricalStorage>>,
    /// Optional temporal context (valid_time, transaction_time) for edge filtering.
    /// When present, only edges that existed at the specified point in time are traversed.
    temporal_context: Option<(Timestamp, Timestamp)>,
    // BFS state - reset for each input node (see doc comment above)
    frontier: VecDeque<(NodeId, Vec<EntityId>, usize)>,
    visited: HashSet<NodeId>,
    input_exhausted: bool,
}

impl TraversalIterator {
    pub fn new(
        input: Box<dyn ResultIterator>,
        direction: Direction,
        label: Option<String>,
        depth: usize,
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_context: Option<(Timestamp, Timestamp)>,
    ) -> Self {
        TraversalIterator {
            input,
            direction,
            label,
            depth,
            current,
            historical,
            temporal_context,
            frontier: VecDeque::new(),
            visited: HashSet::new(),
            input_exhausted: false,
        }
    }

    /// Check if an edge existed at the specified temporal context using a pre-acquired lock guard.
    /// Returns true if no temporal context is set (current state query).
    #[inline]
    fn edge_visible_at_time(
        &self,
        edge_id: crate::core::EdgeId,
        historical_guard: &Option<parking_lot::RwLockReadGuard<'_, HistoricalStorage>>,
    ) -> bool {
        match self.temporal_context {
            Some((valid_time, tx_time)) => {
                // Use the pre-acquired guard to avoid per-edge lock acquisition
                historical_guard
                    .as_ref()
                    .expect("historical_guard must be Some when temporal_context is Some")
                    .find_edge_version_at_time(edge_id, valid_time, tx_time)
                    .is_some()
            }
            None => true, // No temporal context, use current state
        }
    }

    fn get_neighbors(&self, node_id: NodeId) -> Vec<(NodeId, crate::core::EdgeId)> {
        // Acquire historical lock ONCE for all edge checks in this call.
        // This avoids the performance regression of acquiring per-edge locks.
        let historical_guard = self.temporal_context.map(|_| self.historical.read());

        match self.direction {
            Direction::Outgoing => {
                // Use iterator methods to avoid intermediate Vec allocation (Issue #187)
                if let Some(ref label) = self.label {
                    self.current
                        .get_outgoing_edges_with_label_iter(node_id, label)
                        .filter_map(|edge_id| {
                            if !self.edge_visible_at_time(edge_id, &historical_guard) {
                                return None;
                            }
                            // Zero-copy: only get target NodeId, not full Edge (Issue #190)
                            self.current
                                .get_edge_target(edge_id)
                                .ok()
                                .map(|target| (target, edge_id))
                        })
                        .collect()
                } else {
                    self.current
                        .get_outgoing_edges_iter(node_id)
                        .filter_map(|edge_id| {
                            if !self.edge_visible_at_time(edge_id, &historical_guard) {
                                return None;
                            }
                            // Zero-copy: only get target NodeId, not full Edge (Issue #190)
                            self.current
                                .get_edge_target(edge_id)
                                .ok()
                                .map(|target| (target, edge_id))
                        })
                        .collect()
                }
            }
            Direction::Incoming => {
                // Use iterator methods to avoid intermediate Vec allocation (Issue #187)
                if let Some(ref label) = self.label {
                    self.current
                        .get_incoming_edges_with_label_iter(node_id, label)
                        .filter_map(|edge_id| {
                            if !self.edge_visible_at_time(edge_id, &historical_guard) {
                                return None;
                            }
                            // Zero-copy: only get source NodeId, not full Edge (Issue #190)
                            self.current
                                .get_edge_source(edge_id)
                                .ok()
                                .map(|source| (source, edge_id))
                        })
                        .collect()
                } else {
                    self.current
                        .get_incoming_edges_iter(node_id)
                        .filter_map(|edge_id| {
                            if !self.edge_visible_at_time(edge_id, &historical_guard) {
                                return None;
                            }
                            // Zero-copy: only get source NodeId, not full Edge (Issue #190)
                            self.current
                                .get_edge_source(edge_id)
                                .ok()
                                .map(|source| (source, edge_id))
                        })
                        .collect()
                }
            }
            Direction::Both => {
                let mut neighbors = Vec::new();

                // Use iterator methods to avoid intermediate Vec allocation (Issue #187)
                // Helper closure to process edges and add to neighbors
                // Zero-copy: only get target NodeId, not full Edge (Issue #190)
                let mut process_outgoing = |edge_id| {
                    if !self.edge_visible_at_time(edge_id, &historical_guard) {
                        return;
                    }
                    if let Ok(target) = self.current.get_edge_target(edge_id) {
                        neighbors.push((target, edge_id));
                    }
                };

                if let Some(ref label) = self.label {
                    for edge_id in self
                        .current
                        .get_outgoing_edges_with_label_iter(node_id, label)
                    {
                        process_outgoing(edge_id);
                    }
                } else {
                    for edge_id in self.current.get_outgoing_edges_iter(node_id) {
                        process_outgoing(edge_id);
                    }
                }

                // Zero-copy: only get source NodeId, not full Edge (Issue #190)
                let mut process_incoming = |edge_id| {
                    if !self.edge_visible_at_time(edge_id, &historical_guard) {
                        return;
                    }
                    if let Ok(source) = self.current.get_edge_source(edge_id) {
                        neighbors.push((source, edge_id));
                    }
                };

                if let Some(ref label) = self.label {
                    for edge_id in self
                        .current
                        .get_incoming_edges_with_label_iter(node_id, label)
                    {
                        process_incoming(edge_id);
                    }
                } else {
                    for edge_id in self.current.get_incoming_edges_iter(node_id) {
                        process_incoming(edge_id);
                    }
                }

                neighbors
            }
        }
    }
}

impl ResultIterator for TraversalIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        loop {
            // Process current frontier
            if let Some((node_id, path, current_depth)) = self.frontier.pop_front() {
                if current_depth >= self.depth {
                    // Reached target depth, yield result
                    match self.current.get_node(node_id) {
                        Ok(node) => {
                            return Some(Ok(QueryRow::with_path(EntityResult::Node(node), path)));
                        }
                        Err(e) => return Some(Err(e)),
                    }
                }

                // Expand neighbors
                let neighbors = self.get_neighbors(node_id);
                for (target, edge_id) in neighbors {
                    if self.visited.insert(target) {
                        let mut new_path = path.clone();
                        new_path.push(EntityId::Edge(edge_id));
                        new_path.push(EntityId::Node(target));
                        self.frontier
                            .push_back((target, new_path, current_depth + 1));
                    }
                }
                continue;
            }

            // Frontier exhausted, get next from input
            if self.input_exhausted {
                return None;
            }

            match self.input.next() {
                Some(Ok(row)) => {
                    if let Some(node_id) = row.entity.node_id() {
                        self.visited.clear();
                        self.visited.insert(node_id);
                        self.frontier
                            .push_back((node_id, vec![EntityId::Node(node_id)], 0));
                    }
                }
                Some(Err(e)) => return Some(Err(e)),
                None => {
                    self.input_exhausted = true;
                    // Process any remaining frontier
                    if self.frontier.is_empty() {
                        return None;
                    }
                }
            }
        }
    }
}

/// Iterator for filtering results.
///
/// # Example
///
/// ```rust
/// use aletheiadb::query::executor::{FilterIterator, NodeScanIterator};
/// use aletheiadb::query::ir::Predicate;
/// use std::sync::Arc;
///
/// let current = Arc::new(aletheiadb::storage::CurrentStorage::new());
/// let input = Box::new(NodeScanIterator::new(Some("Person".to_string()), current));
/// let predicate = Predicate::eq("name", "Alice");
/// let filter_iter = FilterIterator::new(input, predicate);
///
/// // Iterate results
/// // for row in filter_iter { ... }
/// ```
pub struct FilterIterator {
    input: Box<dyn ResultIterator>,
    predicate: Predicate,
}

impl FilterIterator {
    /// Create a new FilterIterator that filters results based on the predicate.
    pub fn new(input: Box<dyn ResultIterator>, predicate: Predicate) -> Self {
        FilterIterator { input, predicate }
    }

    fn evaluate(&self, node: &Node) -> bool {
        self.evaluate_predicate(&self.predicate, node)
    }

    fn evaluate_predicate(&self, predicate: &Predicate, node: &Node) -> bool {
        match predicate {
            Predicate::True => true,
            Predicate::False => false,
            Predicate::Eq { key, value } => self.evaluate_eq(node, key, value),
            Predicate::Ne { key, value } => self.evaluate_ne(node, key, value),
            Predicate::Gt { key, value } => self.evaluate_gt(node, key, value),
            Predicate::Lt { key, value } => self.evaluate_lt(node, key, value),
            Predicate::Gte { key, value } => self.evaluate_gte(node, key, value),
            Predicate::Lte { key, value } => self.evaluate_lte(node, key, value),
            Predicate::Exists(key) => node.properties.get(key).is_some(),
            Predicate::NotExists(key) => node.properties.get(key).is_none(),
            Predicate::Contains { key, substring } => self.evaluate_contains(node, key, substring),
            Predicate::StartsWith { key, prefix } => self.evaluate_starts_with(node, key, prefix),
            Predicate::EndsWith { key, suffix } => self.evaluate_ends_with(node, key, suffix),
            Predicate::In { key, values } => self.evaluate_in(node, key, values),
            Predicate::And(preds) => preds.iter().all(|p| self.evaluate_predicate(p, node)),
            Predicate::Or(preds) => preds.iter().any(|p| self.evaluate_predicate(p, node)),
            Predicate::Not(pred) => !self.evaluate_predicate(pred, node),
        }
    }

    fn evaluate_eq(&self, node: &Node, key: &str, value: &PredicateValue) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return false;
        };
        self.compare_eq(prop, value)
    }

    fn evaluate_ne(&self, node: &Node, key: &str, value: &PredicateValue) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return true; // Non-existent != anything
        };
        !self.compare_eq(prop, value)
    }

    fn evaluate_gt(&self, node: &Node, key: &str, value: &PredicateValue) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return false;
        };
        self.compare_gt(prop, value)
    }

    fn evaluate_lt(&self, node: &Node, key: &str, value: &PredicateValue) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return false;
        };
        self.compare_lt(prop, value)
    }

    fn evaluate_gte(&self, node: &Node, key: &str, value: &PredicateValue) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return false;
        };
        self.compare_gte(prop, value)
    }

    fn evaluate_lte(&self, node: &Node, key: &str, value: &PredicateValue) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return false;
        };
        self.compare_lte(prop, value)
    }

    fn evaluate_contains(&self, node: &Node, key: &str, substring: &str) -> bool {
        let Some(PropertyValue::String(s)) = node.properties.get(key) else {
            return false;
        };
        s.contains(substring)
    }

    fn evaluate_starts_with(&self, node: &Node, key: &str, prefix: &str) -> bool {
        let Some(PropertyValue::String(s)) = node.properties.get(key) else {
            return false;
        };
        s.starts_with(prefix)
    }

    fn evaluate_ends_with(&self, node: &Node, key: &str, suffix: &str) -> bool {
        let Some(PropertyValue::String(s)) = node.properties.get(key) else {
            return false;
        };
        s.ends_with(suffix)
    }

    fn evaluate_in(&self, node: &Node, key: &str, values: &[PredicateValue]) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return false;
        };
        values.iter().any(|v| self.compare_eq(prop, v))
    }

    fn compare_eq(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Bool(a), PredicateValue::Bool(b)) => a == b,
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a == b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => (a - b).abs() < f64::EPSILON,
            (PropertyValue::String(a), PredicateValue::String(b)) => a.as_ref() == b.as_str(),
            (PropertyValue::Null, PredicateValue::Null) => true,
            _ => false,
        }
    }

    fn compare_gt(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a > b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => a > b,
            _ => false,
        }
    }

    fn compare_lt(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a < b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => a < b,
            _ => false,
        }
    }

    fn compare_gte(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a >= b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => a >= b,
            _ => false,
        }
    }

    fn compare_lte(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a <= b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => a <= b,
            _ => false,
        }
    }
}

impl ResultIterator for FilterIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        loop {
            match self.input.next() {
                Some(Ok(row)) => {
                    if let Some(node) = row.entity.as_node() {
                        if self.evaluate(node) {
                            return Some(Ok(row));
                        }
                        // Filter didn't pass, continue to next
                    } else {
                        // Non-node entities pass through
                        return Some(Ok(row));
                    }
                }
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }
    }
}

/// Helper struct for maintaining query rows with similarity scores in a heap.
/// Ordered by score (higher is better) via Ord implementation.
#[derive(Clone)]
struct ScoredRow {
    row: QueryRow,
    score: f32,
}

impl PartialEq for ScoredRow {
    fn eq(&self, other: &Self) -> bool {
        self.score.to_bits() == other.score.to_bits()
    }
}
impl Eq for ScoredRow {}

impl PartialOrd for ScoredRow {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScoredRow {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Invariant: compute_similarity() filters out non-finite values,
        // so all scores in the heap are finite.
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

/// Iterator for vector reranking.
pub struct VectorRerankIterator {
    sorted: Option<std::vec::IntoIter<(QueryRow, f32)>>,
    input: Option<Box<dyn ResultIterator>>,
    embedding: Arc<[f32]>,
    k: usize,
    _current: Arc<CurrentStorage>,
    /// Vector property name, or None if no vector index is configured
    vector_property: Option<String>,
}

impl VectorRerankIterator {
    /// Create a new VectorRerankIterator.
    ///
    /// # Arguments
    /// * `input` - The input iterator to rerank
    /// * `embedding` - The target embedding for similarity comparison
    /// * `k` - Maximum number of results to keep
    /// * `current` - Reference to current storage
    /// * `property_key` - Optional property to use for reranking. If None, uses default.
    pub fn new(
        input: Box<dyn ResultIterator>,
        embedding: Arc<[f32]>,
        k: usize,
        current: Arc<CurrentStorage>,
        property_key: Option<String>,
    ) -> Self {
        // Use explicit property if provided, otherwise get default from storage
        let vector_property = property_key.or_else(|| current.get_vector_property_name());

        VectorRerankIterator {
            sorted: None,
            input: Some(input),
            embedding,
            k,
            _current: current,
            vector_property,
        }
    }

    /// Compute similarity score for a query row if it has a vector property.
    /// Returns None if the node has no vector, or if the similarity is invalid (NaN/Inf).
    fn compute_similarity(&self, row: &QueryRow, vector_property: &str) -> Option<f32> {
        let node = row.entity.as_node()?;
        let PropertyValue::Vector(vec) = node.properties.get(vector_property)? else {
            return None;
        };
        let similarity = cosine_similarity(&self.embedding, vec).ok()?;
        // Reject NaN/Inf values - these indicate invalid input (e.g., zero-length vectors)
        if similarity.is_finite() {
            Some(similarity)
        } else {
            #[cfg(feature = "observability")]
            tracing::debug!(
                "Skipping node {:?} with non-finite similarity score: {}",
                node.id,
                similarity
            );
            None
        }
    }
}

impl ResultIterator for VectorRerankIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        // Lazy initialization: collect and sort on first call
        if self.sorted.is_none() && self.input.is_some() {
            // Check if vector index is configured
            let vector_property = match &self.vector_property {
                Some(prop) => prop.clone(),
                None => {
                    return Some(Err(crate::core::error::Error::Vector(
                        crate::core::error::VectorError::IndexError(
                            "VectorRerank requires a vector index to be enabled. \
                             Call db.vector_index(\"...\").hnsw(...).enable() first."
                                .to_string(),
                        ),
                    )));
                }
            };

            let mut input = self.input.take().unwrap();
            // Use a min-heap to keep the top-k results
            let mut heap = BinaryHeap::with_capacity(self.k);

            while let Some(result) = input.next() {
                match result {
                    Ok(row) => {
                        // Get vector from node and compute similarity
                        if let Some(similarity) = self.compute_similarity(&row, &vector_property) {
                            debug_assert!(similarity.is_finite(), "Non-finite similarity score");
                            if heap.len() < self.k {
                                heap.push(Reverse(ScoredRow {
                                    row,
                                    score: similarity,
                                }));
                            } else {
                                #[allow(clippy::collapsible_if)]
                                if let Some(Reverse(min_row)) = heap.peek() {
                                    if similarity > min_row.score {
                                        heap.pop();
                                        heap.push(Reverse(ScoredRow {
                                            row,
                                            score: similarity,
                                        }));
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => return Some(Err(e)),
                }
            }

            // Convert heap to sorted vector (descending score)
            // BinaryHeap::into_sorted_vec() returns elements in ascending order of T.
            // Since T is Reverse<ScoredRow>, the order is:
            // [Smallest Reverse<ScoredRow>, ..., Largest Reverse<ScoredRow>]
            // Smallest Reverse<ScoredRow> corresponds to Largest ScoredRow (highest score).
            // So the result is [Highest Score, ..., Lowest Score], which is exactly what we want.
            let sorted_rows: Vec<(QueryRow, f32)> = heap
                .into_sorted_vec()
                .into_iter()
                .map(|Reverse(item)| (item.row, item.score))
                .collect();

            self.sorted = Some(sorted_rows.into_iter());
        }

        self.sorted.as_mut()?.next().map(|(mut row, score)| {
            row.score = Some(score);
            Ok(row)
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if let Some(ref sorted) = self.sorted {
            sorted.size_hint()
        } else {
            (0, Some(self.k))
        }
    }
}

/// Iterator for limiting results.
///
/// # Example
///
/// ```rust
/// use aletheiadb::query::executor::{LimitIterator, NodeScanIterator};
/// use std::sync::Arc;
///
/// let current = Arc::new(aletheiadb::storage::CurrentStorage::new());
/// let input = Box::new(NodeScanIterator::new(Some("Person".to_string()), current));
///
/// // Skip 5, take 10
/// let limit_iter = LimitIterator::new(input, 5, 10);
/// ```
pub struct LimitIterator {
    input: Box<dyn ResultIterator>,
    offset: usize,
    count: usize,
    skipped: usize,
    returned: usize,
}

impl LimitIterator {
    /// Create a new LimitIterator that applies offset and limit to the input.
    pub fn new(input: Box<dyn ResultIterator>, offset: usize, count: usize) -> Self {
        LimitIterator {
            input,
            offset,
            count,
            skipped: 0,
            returned: 0,
        }
    }
}

impl ResultIterator for LimitIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        // Skip offset
        while self.skipped < self.offset {
            match self.input.next() {
                Some(Ok(_)) => self.skipped += 1,
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }

        // Check count limit
        if self.returned >= self.count {
            return None;
        }

        match self.input.next() {
            Some(result) => {
                self.returned += 1;
                Some(result)
            }
            None => None,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.count.saturating_sub(self.returned);
        let (lower, upper) = self.input.size_hint();
        (lower.min(remaining), upper.map(|u| u.min(remaining)))
    }
}

/// Wrapper iterator that strips provenance metadata when include_provenance is false.
///
/// This iterator conditionally removes timestamp and path information from QueryRow
/// results based on the query hint. When include_provenance is false, these fields
/// are set to None for better performance and reduced memory usage.
pub struct ProvenanceFilterIterator {
    inner: Box<dyn ResultIterator>,
    include_provenance: bool,
}

impl ProvenanceFilterIterator {
    /// Create a new ProvenanceFilterIterator that conditionally strips metadata.
    pub fn new(inner: Box<dyn ResultIterator>, include_provenance: bool) -> Self {
        ProvenanceFilterIterator {
            inner,
            include_provenance,
        }
    }
}

impl ResultIterator for ProvenanceFilterIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.inner.next().map(|result| {
            result.map(|mut row| {
                if !self.include_provenance {
                    // Strip provenance metadata
                    row.path = None;
                    row.timestamp = None;
                }
                row
            })
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

/// Iterator for projecting specific properties from query results.
pub struct ProjectIterator {
    input: Box<dyn ResultIterator>,
    properties: Vec<String>,
}

impl ProjectIterator {
    /// Create a new ProjectIterator that projects specific properties from the results.
    pub fn new(input: Box<dyn ResultIterator>, mut properties: Vec<String>) -> Self {
        // Deduplicate properties to prevent errors when projecting same property multiple times
        properties.sort();
        properties.dedup();
        ProjectIterator { input, properties }
    }
}

impl ResultIterator for ProjectIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        match self.input.next() {
            Some(Ok(mut row)) => {
                if let Some(node) = row.entity.as_node() {
                    let mut new_props = crate::core::PropertyMapBuilder::new();
                    for prop in &self.properties {
                        if let Some(val) = node.properties.get(prop) {
                            new_props = new_props.try_insert(prop, val.clone()).unwrap();
                        }
                    }
                    let new_node = crate::core::graph::Node::new(
                        node.id,
                        node.label,
                        new_props.build(),
                        node.current_version,
                    );
                    row.entity = EntityResult::Node(new_node);
                }
                Some(Ok(row))
            }
            other => other,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.input.size_hint()
    }
}

/// Convert a `PredicateValue` to a `PropertyValue` for storage-level lookups.
fn predicate_to_property_value(pv: &PredicateValue) -> PropertyValue {
    match pv {
        PredicateValue::Null => PropertyValue::Null,
        PredicateValue::Bool(b) => PropertyValue::Bool(*b),
        PredicateValue::Int(i) => PropertyValue::Int(*i),
        PredicateValue::Float(f) => PropertyValue::Float(*f),
        PredicateValue::String(s) => PropertyValue::String(Arc::from(s.as_str())),
    }
}

/// Iterator for property-based node scans.
///
/// Calls `CurrentStorage::find_nodes_by_property` to get matching node IDs,
/// then resolves each to a full `Node` for the query result.
pub struct PropertyScanIterator {
    current: Arc<CurrentStorage>,
    initialized: bool,
    node_ids: Option<std::vec::IntoIter<NodeId>>,
    label: String,
    property_value: PropertyValue,
    property_key: String,
}

impl PropertyScanIterator {
    pub fn new(
        label: String,
        key: String,
        value: &PredicateValue,
        current: Arc<CurrentStorage>,
    ) -> Self {
        PropertyScanIterator {
            current,
            initialized: false,
            node_ids: None,
            label,
            property_value: predicate_to_property_value(value),
            property_key: key,
        }
    }

    fn initialize(&mut self) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        let ids = self.current.find_nodes_by_property(
            &self.label,
            &self.property_key,
            &self.property_value,
        );
        self.node_ids = Some(ids.into_iter());
    }
}

impl ResultIterator for PropertyScanIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.initialize();

        match self.node_ids.as_mut()?.next() {
            Some(id) => match self.current.get_node(id) {
                Ok(node) => Some(Ok(QueryRow::from_entity(EntityResult::Node(node)))),
                Err(e) => Some(Err(e)),
            },
            None => None,
        }
    }
}

#[cfg(test)]
mod tests;
