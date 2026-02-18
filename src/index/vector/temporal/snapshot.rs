//! Internal snapshot data structures.
//!
//! This module implements the core data structures for storing and querying
//! temporal vector snapshots, including full and delta formats.

use super::config::{MAX_DELTA_CHAIN_DEPTH, MIN_CAPACITY_ESTIMATE};
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::index::vector::VectorIndex;
use crate::index::vector::hnsw::HnswIndex;
use crate::core::error::{Result, VectorError};
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

    /// Delta snapshot storing only differences
    Delta {
        /// Timestamp of the base snapshot this delta is relative to
        base_time: Timestamp,
        /// Vectors added or updated since the base snapshot
        added: Arc<HashMap<NodeId, Arc<[f32]>>>,
        /// NodeIds removed since the base snapshot
        removed: Arc<HashSet<NodeId>>,
    },
}

impl VectorSnapshot {
    /// Retrieves a vector for a given node ID, traversing the delta chain if necessary.
    ///
    /// # Complexity
    /// - Full: O(1)
    /// - Delta: O(depth) where depth is the number of delta layers
    ///
    /// # Safety
    /// Enforces `MAX_DELTA_CHAIN_DEPTH` to prevent stack overflow or excessive latency.
    pub(crate) fn get_vector(
        &self,
        node_id: &NodeId,
        all_snapshots: &BTreeMap<Timestamp, VectorSnapshot>,
    ) -> Result<Option<Arc<[f32]>>> {
        let mut current = self;
        let mut depth = 0;

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

                    // If not found, traverse to base
                    if let Some(base) = all_snapshots.get(base_time) {
                        current = base;
                        depth += 1;
                    } else {
                        // Base snapshot missing - this is a serious integrity error
                        return Err(VectorError::IndexError(format!(
                            "Base snapshot at {} missing for delta snapshot",
                            base_time
                        ))
                        .into());
                    }
                }
            }
        }
    }

    /// Reconstructs the full state of vectors at this snapshot.
    ///
    /// Useful for operations that need the complete dataset (e.g., drift analysis).
    ///
    /// # Performance
    /// This operation can be expensive for deep delta chains. Consider caching results
    /// if called frequently.
    pub(crate) fn to_hashmap(
        &self,
        all_snapshots: &BTreeMap<Timestamp, VectorSnapshot>,
    ) -> Result<HashMap<NodeId, Arc<[f32]>>> {
        // Collect layers
        let mut current = self;
        let mut delta_layers = Vec::new();
        let mut depth = 0;

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
                    // Clone the inner HashMap (O(N)) - unavoidable for reconstruction
                    break vectors.as_ref().clone();
                }
                VectorSnapshot::Delta {
                    base_time,
                    added,
                    removed,
                } => {
                    // Store delta layer to apply later (in reverse order)
                    struct DeltaLayer<'a> {
                        added: &'a HashMap<NodeId, Arc<[f32]>>,
                        removed: &'a HashSet<NodeId>,
                    }
                    delta_layers.push(DeltaLayer {
                        added: added.as_ref(),
                        removed: removed.as_ref(),
                    });

                    // Move to previous snapshot
                    if let Some(base) = all_snapshots.get(base_time) {
                        current = base;
                        depth += 1;
                    } else {
                        return Err(VectorError::IndexError(format!(
                            "Base snapshot at {} missing during reconstruction",
                            base_time
                        ))
                        .into());
                    }
                }
            }
        };

        // Apply deltas forward (from oldest to newest)
        // Base is already cloned, so we can modify it directly
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

    /// Optimized version of to_hashmap that collects ALL vectors from a set of snapshots.
    ///
    /// This is more efficient than calling to_hashmap on each snapshot individually
    /// when iterating over a time range.
    pub(crate) fn collect_all(
        &self,
        all_snapshots: &BTreeMap<Timestamp, VectorSnapshot>,
    ) -> Result<Vec<(NodeId, Arc<[f32]>)>> {
        let map = self.to_hashmap(all_snapshots)?;
        Ok(map.into_iter().collect())
    }

    /// Returns the number of vectors in this snapshot.
    ///
    /// # Accuracy Note
    /// For Full snapshots, this is exact.
    /// For Delta snapshots, this is an **estimate** based on the added vectors count.
    /// It does not account for removals or base vectors, so it returns a safe
    /// underestimate used for capacity estimation during index construction.
    ///
    /// **For exact counts**, use `to_hashmap().len()` which reconstructs the full snapshot
    /// by applying all deltas to the base. Note that reconstruction has O(depth) cost and
    /// should be avoided in hot paths unless necessary.
    ///
    /// # Example
    /// ```rust,ignore
    /// // Fast, O(1), but approximate for Deltas
    /// let approx_len = snapshot.len();
    ///
    /// // Slow, O(depth), but exact
    /// let exact_len = snapshot.to_hashmap(&all_snapshots)?.len();
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
    #[allow(dead_code)]
    pub(crate) fn to_vec(
        &self,
        all_snapshots: &BTreeMap<Timestamp, VectorSnapshot>,
    ) -> Result<Vec<(NodeId, Arc<[f32]>)>> {
        let map = self.to_hashmap(all_snapshots)?;
        Ok(map.into_iter().collect())
    }
}

/// Helper struct for `SnapshotIndex` enum.
/// Wraps either a full HNSW index or a delta index.
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
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>> {
        match self {
            SnapshotIndex::Full(index) => index.search(query, k),
            SnapshotIndex::Delta(delta) => delta.search(query, k),
        }
    }

    pub fn search_with_filter(
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
                // Estimate length for delta
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

/// Internal delta index implementation.
///
/// Stores changes relative to a base index.
pub(crate) struct DeltaIndex {
    pub(crate) base: Arc<SnapshotIndex>,
    pub(crate) added: Arc<HnswIndex>,
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
        // Search for k*2 candidates to ensure global top-k
        // We need more candidates because some might be in the 'removed' set
        // or shadowed by newer versions in 'added'
        let search_k = k.saturating_mul(2).max(k + 10);

        // Search added (small index, fast)
        let mut results = self.added.search(query, search_k)?;

        // Filter out results that are in 'removed' set (unlikely for added, but possible if
        // a node was added then removed in same snapshot window - though usually that's handled at construction)
        // More importantly, this handles the case where we merge results.

        // Search base
        // We must filter out nodes that are in 'removed' set OR in 'added' set (updates shadow old versions)
        let removed = &self.removed;
        let added_ids: HashSet<NodeId> = results.iter().map(|(id, _)| *id).collect();

        let predicate = |id: &NodeId| !removed.contains(id) && !added_ids.contains(id);

        let base_results = self.base.search_with_filter(query, search_k, &predicate)?;

        // Merge results
        results.extend(base_results);

        // Sort and truncate to k
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
pub(crate) struct SnapshotData {
    /// Maps timestamp -> (snapshot_id, snapshot_index)
    pub(crate) snapshots: BTreeMap<Timestamp, (usize, SnapshotIndex)>,

    /// Maps timestamp -> vector_data (for reconstruction)
    pub(crate) vector_history: BTreeMap<Timestamp, VectorSnapshot>,
}

impl SnapshotData {
    pub(crate) fn new() -> Self {
        Self {
            snapshots: BTreeMap::new(),
            vector_history: BTreeMap::new(),
        }
    }

    pub(crate) fn insert(
        &mut self,
        timestamp: Timestamp,
        id: usize,
        index: SnapshotIndex,
        vectors: VectorSnapshot,
    ) {
        self.snapshots.insert(timestamp, (id, index));
        self.vector_history.insert(timestamp, vectors);
    }

    pub(crate) fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub(crate) fn remove_oldest(&mut self) {
        if let Some(key) = self.snapshots.keys().next().copied() {
            self.snapshots.remove(&key);
            self.vector_history.remove(&key);
        }
    }
}

/// Snapshot metadata tracking state.
pub(crate) struct SnapshotMetadata {
    /// Total number of snapshots created (ever)
    pub(crate) total_snapshots: usize,

    /// Number of transactions since last snapshot
    pub(crate) transactions_since_snapshot: usize,

    /// Last snapshot creation time
    pub(crate) last_snapshot_time: Timestamp,

    /// Set of NodeIds changed since last snapshot (for ChangeThreshold)
    pub(crate) vectors_changed_since_snapshot: HashSet<NodeId>,

    /// Last FULL snapshot time (for delta calculation)
    pub(crate) last_full_snapshot_time: Timestamp,

    /// Number of snapshots since last FULL snapshot
    pub(crate) snapshots_since_full: usize,

    /// Accumulator for ALL changes since last FULL snapshot
    /// Used to build the delta index.
    pub(crate) changes_accumulated: HashSet<NodeId>,
}

impl SnapshotMetadata {
    pub(crate) fn new(initial_time: Timestamp) -> Self {
        Self {
            total_snapshots: 0,
            transactions_since_snapshot: 0,
            last_snapshot_time: initial_time,
            vectors_changed_since_snapshot: HashSet::new(),
            last_full_snapshot_time: initial_time,
            snapshots_since_full: 0,
            changes_accumulated: HashSet::new(),
        }
    }

    pub(crate) fn record_change(&mut self, id: NodeId) {
        self.vectors_changed_since_snapshot.insert(id);
        self.changes_accumulated.insert(id);
    }

    pub(crate) fn record_transaction(&mut self) {
        self.transactions_since_snapshot += 1;
    }

    pub(crate) fn reset(&mut self, current_time: Timestamp, is_full: bool) {
        self.transactions_since_snapshot = 0;
        self.last_snapshot_time = current_time;
        self.vectors_changed_since_snapshot.clear();
        self.total_snapshots += 1;

        if is_full {
            self.last_full_snapshot_time = current_time;
            self.snapshots_since_full = 0;
            self.changes_accumulated.clear();
        } else {
            self.snapshots_since_full += 1;
        }
    }
}

/// **Issue #233 Optimization**: This struct combines vector storage and metadata
/// into a single structure protected by one RwLock, reducing lock acquisitions
/// from 3 to 1 during hot-path operations like `add()`.
pub(crate) struct VectorState {
    /// In-memory storage of current vectors
    pub(crate) vectors: HashMap<NodeId, Arc<[f32]>>,

    /// Metadata for snapshot tracking
    pub(crate) metadata: SnapshotMetadata,
}

impl VectorState {
    pub(crate) fn new(initial_time: Timestamp) -> Self {
        Self {
            vectors: HashMap::new(),
            metadata: SnapshotMetadata::new(initial_time),
        }
    }
}
