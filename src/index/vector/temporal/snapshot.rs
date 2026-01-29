use super::config::{MAX_DELTA_CHAIN_DEPTH, MIN_CAPACITY_ESTIMATE};
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::index::vector::VectorIndex;
use crate::index::vector::hnsw::HnswIndex;
use crate::utils::{Result, VectorError};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// Type alias for vector snapshot: map of NodeId to vector data
/// Represents vector data in a snapshot, supporting both full and delta formats.
///
/// Full snapshots store all vectors, while delta snapshots store only changes
/// relative to a base snapshot, significantly reducing memory usage for incremental updates.
#[derive(Clone)]
pub(crate) enum VectorSnapshot {
    /// Full snapshot containing all vectors
    Full(Arc<HashMap<NodeId, Arc<[f32]>>>),

    /// Delta snapshot containing only changes relative to a base
    Delta {
        /// Timestamp of the base full snapshot
        base_time: Timestamp,
        /// Vectors added or updated since base
        added: Arc<HashMap<NodeId, Arc<[f32]>>>,
        /// Vectors removed since base
        removed: Arc<HashSet<NodeId>>,
    },
}

impl VectorSnapshot {
    /// Get a vector from this snapshot, given access to the full snapshot data.
    ///
    /// Uses iterative traversal through delta chain to avoid stack overflow.
    /// Enforces MAX_DELTA_CHAIN_DEPTH to prevent unbounded traversal.
    ///
    /// Returns:
    /// - Ok(Some(vector)) - Vector found
    /// - Ok(None) - Vector not found or was removed
    /// - Err - Delta chain depth exceeded or corrupted snapshot state
    pub(crate) fn get_vector(
        &self,
        node_id: &NodeId,
        all_snapshots: &BTreeMap<Timestamp, VectorSnapshot>,
    ) -> Result<Option<Arc<[f32]>>> {
        let mut current = self;
        let mut depth = 0;

        // Iteratively traverse the delta chain with depth limit
        loop {
            // SAFETY: Check depth limit to prevent unbounded traversal
            // If chain exceeds MAX_DELTA_CHAIN_DEPTH, return error instead of silently failing
            if depth >= MAX_DELTA_CHAIN_DEPTH {
                return Err(VectorError::IndexError(format!(
                    "Delta chain depth exceeded {} for node {:?}. \
                         This indicates corrupted snapshot state or misconfiguration. \
                         Reduce full_snapshot_interval or check snapshot integrity.",
                    MAX_DELTA_CHAIN_DEPTH, node_id
                ))
                .into());
            }

            match current {
                VectorSnapshot::Full(vectors) => {
                    return Ok(vectors.get(node_id).cloned());
                }
                VectorSnapshot::Delta {
                    base_time,
                    added,
                    removed,
                } => {
                    // First check if removed
                    if removed.contains(node_id) {
                        return Ok(None);
                    }

                    // Then check if in added/updated
                    if let Some(vec) = added.get(node_id) {
                        return Ok(Some(Arc::clone(vec)));
                    }

                    // Continue to base snapshot
                    if let Some(base) = all_snapshots.get(base_time) {
                        current = base;
                        depth += 1;
                    } else {
                        // Base snapshot was pruned - this is corrupted state
                        return Err(VectorError::IndexError(format!(
                            "Base snapshot at timestamp {} not found for node {:?}. \
                                 Snapshot state is corrupted or base was incorrectly pruned.",
                            base_time, node_id
                        ))
                        .into());
                    }
                }
            }
        }
    }

    /// Reconstruct all vectors in this snapshot as a HashMap.
    ///
    /// For delta snapshots, this combines the base with added/removed changes.
    /// Uses iterative traversal with depth limiting to prevent stack overflow.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Delta chain depth exceeds MAX_DELTA_CHAIN_DEPTH (corrupted state)
    /// - Base snapshot is missing (corrupted state or incorrect pruning)
    pub(crate) fn to_hashmap(
        &self,
        all_snapshots: &BTreeMap<Timestamp, VectorSnapshot>,
    ) -> Result<HashMap<NodeId, Arc<[f32]>>> {
        // Use iterative approach to prevent stack overflow (similar to get_vector)
        let mut current = self;
        let mut depth = 0;

        // Collect the chain of deltas from newest to oldest
        struct DeltaLayer<'a> {
            added: &'a Arc<HashMap<NodeId, Arc<[f32]>>>,
            removed: &'a Arc<HashSet<NodeId>>,
        }
        let mut delta_layers: Vec<DeltaLayer<'_>> = Vec::new();

        // Walk backwards through delta chain to find the Full snapshot base
        let base_vectors: HashMap<NodeId, Arc<[f32]>> = loop {
            if depth >= MAX_DELTA_CHAIN_DEPTH {
                // Return error instead of partial results to prevent silent data loss
                return Err(VectorError::IndexError(format!(
                    "Delta chain depth exceeded {} in to_hashmap(). \
                         This indicates corrupted snapshot state or misconfiguration. \
                         Reduce full_snapshot_interval or check snapshot integrity.",
                    MAX_DELTA_CHAIN_DEPTH
                ))
                .into());
            }

            match current {
                VectorSnapshot::Full(vectors) => {
                    // Found the base Full snapshot
                    break (**vectors).clone();
                }
                VectorSnapshot::Delta {
                    base_time,
                    added,
                    removed,
                } => {
                    // Record this delta layer for later application
                    delta_layers.push(DeltaLayer { added, removed });

                    // Move to base snapshot
                    if let Some(base_snapshot) = all_snapshots.get(base_time) {
                        current = base_snapshot;
                        depth += 1;
                    } else {
                        // Base was pruned - return error to prevent silent data loss
                        return Err(VectorError::IndexError(format!(
                            "Base snapshot at timestamp {} not found in to_hashmap(). \
                                 Snapshot state is corrupted or base was incorrectly pruned.",
                            base_time
                        ))
                        .into());
                    }
                }
            }
        };

        // Apply delta layers in reverse order (oldest to newest)
        let mut result = base_vectors;
        for layer in delta_layers.iter().rev() {
            // Apply removals
            for node_id in layer.removed.iter() {
                result.remove(node_id);
            }
            // Apply additions/updates
            for (node_id, vector) in layer.added.iter() {
                result.insert(*node_id, Arc::clone(vector));
            }
        }

        Ok(result)
    }

    /// Returns an approximate count of vectors in this snapshot.
    ///
    /// **IMPORTANT**: For delta snapshots, this returns ONLY the count of added vectors,
    /// ignoring the base snapshot size and removed vectors. This is intentionally an
    /// underestimate used for capacity estimation during index construction.
    ///
    /// **For exact counts**, use `to_hashmap().len()` which reconstructs the full snapshot
    /// by applying all deltas to the base. Note that reconstruction has O(depth) cost and
    /// may return partial results if the delta chain exceeds `MAX_DELTA_CHAIN_DEPTH`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Delta snapshot: base has 100 vectors, added 10, removed 5
    /// snapshot.len()           // Returns 10 (approximation, added only)
    /// snapshot.to_hashmap().len()  // Returns 105 (exact: 100 + 10 - 5)
    /// ```
    pub(crate) fn len(&self) -> usize {
        match self {
            VectorSnapshot::Full(vectors) => vectors.len(),
            VectorSnapshot::Delta { added, .. } => {
                // Approximation: just return added size
                // This is used only for capacity estimation during index construction
                // and intentionally underestimates to avoid excessive memory allocation
                added.len().max(MIN_CAPACITY_ESTIMATE)
            }
        }
    }

    /// Collect all vectors in this snapshot into a Vec.
    ///
    /// For delta snapshots, this reconstructs the full set.
    /// Returns a vector of (NodeId, Arc<[f32]>) pairs.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot state is corrupted (see `to_hashmap` for details).
    pub(crate) fn collect_all(
        &self,
        all_snapshots: &BTreeMap<Timestamp, VectorSnapshot>,
    ) -> Result<Vec<(NodeId, Arc<[f32]>)>> {
        let hashmap = self.to_hashmap(all_snapshots)?;
        Ok(hashmap.into_iter().collect())
    }
}

/// Storage structure for snapshot data.
/// Can be either a full HNSW index or a delta index.
#[derive(Clone)]
pub(crate) enum SnapshotIndex {
    Full(Arc<HnswIndex>),
    Delta(Arc<DeltaIndex>),
}

impl std::fmt::Debug for SnapshotIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotIndex::Full(index) => f
                .debug_struct("SnapshotIndex::Full")
                .field("len", &index.len())
                .field("dimensions", &index.dimensions())
                .finish(),
            SnapshotIndex::Delta(delta) => f
                .debug_struct("SnapshotIndex::Delta")
                .field("base_len", &delta.base.len())
                .field("added_len", &delta.added.len())
                .field("removed_len", &delta.removed.len())
                .field(
                    "total_len",
                    &(delta.base.len() + delta.added.len() - delta.removed.len()),
                )
                .finish(),
        }
    }
}

impl SnapshotIndex {
    pub(crate) fn search(&self, query: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>> {
        match self {
            SnapshotIndex::Full(index) => index.search(query, k),
            SnapshotIndex::Delta(delta) => delta.search(query, k),
        }
    }

    pub(crate) fn search_with_filter(
        &self,
        query: &[f32],
        k: usize,
        predicate: &(dyn Fn(&NodeId) -> bool + Send + Sync),
    ) -> Result<Vec<(NodeId, f32)>> {
        match self {
            SnapshotIndex::Full(index) => index.search_with_filter(query, k, predicate),
            SnapshotIndex::Delta(delta) => delta.search_with_filter(query, k, predicate),
        }
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            SnapshotIndex::Full(index) => index.len(),
            SnapshotIndex::Delta(delta) => {
                // Correct count: base + added - removed
                // removed.len() is O(1), so no performance penalty
                delta.base.len() + delta.added.len() - delta.removed.len()
            }
        }
    }

    pub(crate) fn dimensions(&self) -> usize {
        match self {
            SnapshotIndex::Full(index) => index.dimensions(),
            SnapshotIndex::Delta(delta) => delta.added.dimensions(),
        }
    }
}

/// A delta snapshot that stores only changes relative to a base snapshot.
#[derive(Clone)]
pub(crate) struct DeltaIndex {
    /// The base snapshot this delta is built upon (usually a Full snapshot)
    pub(crate) base: Arc<SnapshotIndex>,
    /// Vectors added or updated since the base snapshot
    pub(crate) added: Arc<HnswIndex>,
    /// IDs of vectors that were removed or updated (invalidating the base version)
    pub(crate) removed: Arc<HashSet<NodeId>>,
}

impl std::fmt::Debug for DeltaIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeltaIndex")
            .field("base", &self.base)
            .field("added_len", &self.added.len())
            .field("added_dimensions", &self.added.dimensions())
            .field("removed_count", &self.removed.len())
            .finish()
    }
}

impl DeltaIndex {
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>> {
        // Search strategy: Search for k*2 candidates from each index to ensure we don't
        // miss true top-k when merging. This is necessary because the global top-k might
        // be distributed across both indexes.
        //
        // Example: If added has [0.99, 0.95, 0.90] and base has [0.97, 0.93, 0.88],
        // searching for k=2 in each would give [0.99, 0.95] + [0.97, 0.93].
        // Merging gives true top-2: [0.99, 0.97] ✓
        let search_k = k.saturating_mul(2).max(k + 10);

        // 1. Search added vectors (new and updated)
        let mut results = self.added.search(query, search_k)?;

        // 2. Search base vectors with filter
        // Filter out any ID that is in the 'removed' set, which includes:
        // - Nodes that were updated (old version in base, new version in added)
        // - Nodes that were removed (present in base, deleted from current state)
        let removed = &self.removed;
        let base_results = self
            .base
            .search_with_filter(query, search_k, &|id| !removed.contains(id))?;

        // 3. Merge results from both indexes
        results.extend(base_results);

        // 4. Deduplicate: Although the removed filter should prevent duplicates,
        // we deduplicate as a safety measure to ensure correctness.
        // We keep the first occurrence (which has the better score after sorting).
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        results.retain(|(id, _)| seen.insert(*id));

        // 5. Sort by similarity (descending) and truncate to k
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);

        Ok(results)
    }

    fn search_with_filter(
        &self,
        query: &[f32],
        k: usize,
        predicate: &(dyn Fn(&NodeId) -> bool + Send + Sync),
    ) -> Result<Vec<(NodeId, f32)>> {
        // Search for k*2 candidates to ensure global top-k (same strategy as search())
        let search_k = k.saturating_mul(2).max(k + 10);

        // Combine user predicate with our removed set
        let removed = &self.removed;
        let combined_predicate = |id: &NodeId| predicate(id) && !removed.contains(id);

        // Search added (using user predicate only)
        let mut results = self.added.search_with_filter(query, search_k, predicate)?;

        // Search base (using combined predicate to filter out removed/updated nodes)
        let base_results = self
            .base
            .search_with_filter(query, search_k, &combined_predicate)?;

        // Merge results
        results.extend(base_results);

        // Deduplicate: Although the combined predicate should prevent duplicates,
        // we deduplicate as a safety measure to ensure correctness.
        // We keep the first occurrence (which has the better score after sorting).
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        results.retain(|(id, _)| seen.insert(*id));

        // Sort and truncate to k
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);

        Ok(results)
    }
}

/// Snapshot data protected by a single lock.
///
/// Groups snapshots and vector history together to ensure atomic updates
/// and prevent deadlocks from acquiring multiple locks sequentially.
pub(crate) struct SnapshotData {
    /// Historical HNSW snapshots at anchor timestamps
    /// Key: Timestamp when snapshot was created
    /// Value: (Stable snapshot ID, SnapshotIndex)
    pub(crate) snapshots: BTreeMap<Timestamp, (usize, SnapshotIndex)>,

    /// Historical vector values at each snapshot
    /// Key: Timestamp when snapshot was created
    /// Value: Immutable map of NodeId -> Vector for that snapshot
    pub(crate) vector_history: BTreeMap<Timestamp, VectorSnapshot>,
}

impl SnapshotData {
    pub(crate) fn new() -> Self {
        SnapshotData {
            snapshots: BTreeMap::new(),
            vector_history: BTreeMap::new(),
        }
    }

    pub(crate) fn insert(
        &mut self,
        timestamp: Timestamp,
        stable_id: usize,
        snapshot: SnapshotIndex,
        vectors: VectorSnapshot,
    ) {
        self.snapshots.insert(timestamp, (stable_id, snapshot));
        self.vector_history.insert(timestamp, vectors);
    }

    pub(crate) fn remove_oldest(&mut self) {
        if let Some(oldest_key) = self.snapshots.keys().next().copied() {
            self.snapshots.remove(&oldest_key);
            self.vector_history.remove(&oldest_key);
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.snapshots.len()
    }
}

/// Metadata for snapshot management.
///
/// Tracks state needed to determine when to create the next snapshot.
#[derive(Debug, Clone)]
pub(crate) struct SnapshotMetadata {
    /// Last snapshot timestamp (microseconds since epoch)
    pub(crate) last_snapshot_time: Timestamp,

    /// Transaction count since last snapshot
    pub(crate) transactions_since_snapshot: usize,

    /// Vectors changed since last snapshot (resets every snapshot)
    pub(crate) vectors_changed_since_snapshot: HashSet<NodeId>,

    /// Total snapshots created (for ID generation)
    pub(crate) total_snapshots: usize,

    /// Time of the last FULL snapshot
    pub(crate) last_full_snapshot_time: Timestamp,

    /// Accumulated changes since the last FULL snapshot.
    /// This is used to build Delta snapshots.
    /// Resets only when a FULL snapshot is created.
    ///
    /// **MEMORY GROWTH WARNING**: This set grows unboundedly between full snapshots.
    /// In write-heavy workloads with large `full_snapshot_interval` values, this can
    /// consume significant memory. Mitigation strategies:
    /// - Reduce `full_snapshot_interval` to trigger full snapshots more frequently
    /// - Use manual snapshots (`create_manual_snapshot()`) during idle periods
    /// - Monitor memory usage if updating >100k unique vectors between full snapshots
    pub(crate) changes_accumulated: HashSet<NodeId>,

    /// Number of snapshots created since the last FULL snapshot.
    /// Used to trigger periodic FULL snapshots.
    pub(crate) snapshots_since_full: usize,
}

impl SnapshotMetadata {
    pub(crate) fn new(initial_time: Timestamp) -> Self {
        SnapshotMetadata {
            last_snapshot_time: initial_time,
            transactions_since_snapshot: 0,
            vectors_changed_since_snapshot: HashSet::new(),
            total_snapshots: 0,
            last_full_snapshot_time: initial_time,
            changes_accumulated: HashSet::new(),
            snapshots_since_full: 0,
        }
    }

    /// Record a vector change for snapshot tracking.
    pub(crate) fn record_change(&mut self, node_id: NodeId) {
        self.vectors_changed_since_snapshot.insert(node_id);
        self.changes_accumulated.insert(node_id);
    }

    /// Record a transaction (increment counter).
    pub(crate) fn record_transaction(&mut self) {
        self.transactions_since_snapshot += 1;
    }

    /// Reset tracking after creating a snapshot.
    pub(crate) fn reset(&mut self, snapshot_time: Timestamp, is_full: bool) {
        self.last_snapshot_time = snapshot_time;
        self.transactions_since_snapshot = 0;
        self.vectors_changed_since_snapshot.clear();
        self.total_snapshots += 1;

        if is_full {
            self.last_full_snapshot_time = snapshot_time;
            self.changes_accumulated.clear();
            self.snapshots_since_full = 0;
        } else {
            self.snapshots_since_full += 1;
        }
    }
}

/// Combined state for current vectors and metadata.
///
/// **Issue #233 Optimization**: This struct combines vector storage and metadata
/// into a single structure protected by one RwLock, reducing lock acquisitions
/// from 3 to 1 per add() operation.
///
/// This eliminates the need for:
/// - DashMap internal locking for vectors
/// - Separate RwLock for metadata
///
/// Instead, we acquire a single write lock to update both vectors and metadata atomically.
#[derive(Debug)]
pub(crate) struct VectorState {
    /// Current vector storage - maintains actual vector data for snapshot copying
    /// Maps NodeId to the vector embedding
    pub(crate) vectors: HashMap<NodeId, Arc<[f32]>>,

    /// Metadata for snapshot management
    pub(crate) metadata: SnapshotMetadata,
}

impl VectorState {
    pub(crate) fn new(initial_time: Timestamp) -> Self {
        VectorState {
            vectors: HashMap::new(),
            metadata: SnapshotMetadata::new(initial_time),
        }
    }
}
