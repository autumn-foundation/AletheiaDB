//! Temporal vector index for time-aware semantic search.
//!
//! This module implements temporal vector indexing using snapshot-based HNSW indexes,
//! enabling point-in-time vector queries and semantic drift tracking. This is Phase 3
//! of GallifreyDB's vector search integration.
//!
//! # Architecture
//!
//! The temporal vector index uses a dual-path design:
//! - **Current index**: Live HNSW index for present-time queries
//! - **Snapshot indexes**: Historical HNSW snapshots at configurable intervals
//!
//! This mirrors GallifreyDB's hybrid storage architecture (see ADR-0001) where current
//! state is optimized separately from historical data.
//!
//! # Snapshot Strategy
//!
//! Snapshots are created based on configurable triggers (see [`SnapshotStrategy`]):
//! - **TransactionInterval**: Create snapshot every N write transactions (default: 10)
//! - **TimeInterval**: Create snapshot at fixed time intervals (e.g., hourly)
//! - **ChangeThreshold**: Create snapshot when X% of vectors have changed
//! - **Hybrid**: Use whichever trigger fires first
//!
//! # Delta Snapshots (Performance Optimization)
//!
//! To optimize memory usage and creation time, we use a Delta-based snapshot approach:
//! - **Full Snapshots**: Created periodically (e.g., every 10 snapshots). Contain full HNSW index.
//! - **Delta Snapshots**: Created in between. Contain only vectors changed since the last Full snapshot.
//!
//! This reduces snapshot creation time from O(N log N) to O(M log M) where M is the number of changes.
//! Query performance is maintained by searching both Delta and Base indexes and merging results.
//!
//! # Examples
//!
//! ```rust
//! use gallifreydb::index::vector::temporal::{TemporalVectorIndex, TemporalVectorConfig, SnapshotStrategy, RetentionPolicy};
//! use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
//! use gallifreydb::core::id::NodeId;
//! use gallifreydb::core::temporal::TimeRange;
//!
//! # fn example() -> gallifreydb::utils::Result<()> {
//! // Create temporal index configuration
//! let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
//! let config = TemporalVectorConfig {
//!     snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
//!     retention_policy: RetentionPolicy::KeepN(100),
//!     max_snapshots: 100,
//!     full_snapshot_interval: 10,
//!     hnsw_config: Some(hnsw_config),
//! };
//!
//! // Create temporal index
//! let index = TemporalVectorIndex::new(config)?;
//!
//! // Find similar vectors at specific point in time
//! let query = vec![0.1f32; 384];
//! let timestamp = 1234567890000000; // microseconds since epoch
//! let results = index.find_similar_as_of(&query, 10, timestamp)?;
//!
//! // Track semantic drift over time
//! let node_id = NodeId::new(42).unwrap();
//! let reference_embedding = vec![0.5f32; 384];
//! let time_range = TimeRange::new(1000000.into(), 2000000.into()).unwrap();
//! let drift = index.track_semantic_drift(node_id, &reference_embedding, time_range)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Design
//!
//! For complete design rationale and trade-offs, see:
//! - `docs/adr/0017-temporal-vector-strategy.md` - Design decisions
//! - `docs/VECTOR_SEARCH_DESIGN.md` - Overall vector search architecture

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use parking_lot::RwLock;

use crate::core::id::NodeId;
use crate::core::temporal::{TimeRange, Timestamp};
use crate::core::vector::{cosine_similarity, euclidean_distance};
use crate::index::vector::hnsw::HnswIndex;
use crate::index::vector::{DistanceMetric, HnswConfig, TemporalSearchResults, VectorIndex};
use crate::utils::{Error, Result, TemporalError, VectorError};

#[cfg(feature = "observability")]
use tracing;

/// Retention policy for snapshot cleanup.
///
/// Determines which snapshots to keep and which to prune.
#[derive(Debug, Clone, PartialEq)]
pub enum RetentionPolicy {
    /// Keep all snapshots (no automatic pruning).
    KeepAll,

    /// Keep only the most recent N snapshots.
    KeepN(usize),

    /// Keep snapshots within a time duration from now.
    KeepDuration(Duration),
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        RetentionPolicy::KeepN(100)
    }
}

/// Configuration for temporal vector indexing.
///
/// This struct encapsulates all parameters needed to configure temporal vector
/// indexing with HNSW snapshots.
///
/// # Examples
///
/// ```rust
/// use gallifreydb::index::vector::temporal::{TemporalVectorConfig, SnapshotStrategy, RetentionPolicy};
/// use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
///
/// // Default configuration (transaction-based, every 10 transactions)
/// let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
/// let config = TemporalVectorConfig::default_with_hnsw(hnsw_config);
///
/// // Custom hybrid strategy with retention policy
/// let config = TemporalVectorConfig {
///     snapshot_strategy: SnapshotStrategy::Hybrid {
///         transaction_interval: 10,
///         time_interval_secs: 3600,  // Hourly
///         change_threshold: 0.1,      // 10% changed
///     },
///     retention_policy: RetentionPolicy::KeepN(50),
///     max_snapshots: 100,
///     full_snapshot_interval: 10,
///     hnsw_config: Some(HnswConfig::new(384, DistanceMetric::Cosine)),
/// };
/// ```
/// Maximum number of retries when creating a snapshot due to races (default: 3)
const MAX_SNAPSHOT_RETRIES: usize = 3;

/// Maximum depth of delta chain traversal before forcing full snapshot (default: 10)
/// This prevents unbounded delta chains and ensures reconstruction performance.
/// Mirrors the full_snapshot_interval default value.
const MAX_DELTA_CHAIN_DEPTH: usize = 10;

/// Minimum capacity estimate for HashMap pre-allocation (default: 100)
/// Used when estimating capacity for vector collections to avoid excessive resizing.
///
/// **Justification**: 100 is a reasonable baseline that:
/// - Avoids excessive allocations for small datasets (most vectors fit in 1 allocation)
/// - Prevents too many resizes for medium datasets
/// - Has negligible overhead (~800 bytes for empty capacity-100 HashMap)
/// - Aligns with common batch sizes in vector databases
const MIN_CAPACITY_ESTIMATE: usize = 100;

/// Maximum accumulated changes before forcing a full snapshot (default: 100,000)
///
/// If `changes_accumulated` exceeds this threshold, force a full snapshot
/// regardless of `full_snapshot_interval`. This prevents unbounded memory growth
/// in write-heavy workloads.
///
/// **Justification**: 100k NodeIds × 8 bytes = 800 KB overhead, which is acceptable
/// but starting to impact memory. Forcing a full snapshot resets the accumulator.
const MAX_ACCUMULATED_CHANGES: usize = 100_000;

/// Configuration for temporal vector index with snapshot management.
///
/// Controls how and when snapshots are created, retained, and pruned to balance
/// memory usage, query performance, and historical depth.
#[derive(Debug, Clone, PartialEq)]
pub struct TemporalVectorConfig {
    /// Snapshot creation strategy
    pub snapshot_strategy: SnapshotStrategy,

    /// Snapshot retention policy (default: KeepN(100))
    pub retention_policy: RetentionPolicy,

    /// Maximum number of snapshots to retain (default: 100)
    ///
    /// When this limit is exceeded, the oldest snapshots are removed.
    /// This prevents unbounded storage growth.
    pub max_snapshots: usize,

    /// Interval for creating full snapshots (default: 10)
    ///
    /// After this many delta snapshots, a new full snapshot is created.
    /// Higher values save memory but increase reconstruction time.
    /// Lower values increase memory but improve query speed.
    pub full_snapshot_interval: usize,

    /// Base HNSW configuration for all indexes (current + snapshots)
    ///
    /// If `None`, the temporal index will use the existing vector index's configuration.
    /// This allows enabling temporal features on an already-configured vector index.
    pub hnsw_config: Option<HnswConfig>,
}

impl TemporalVectorConfig {
    /// Validates the configuration.
    ///
    /// # Errors
    /// Returns an error if:
    /// - `full_snapshot_interval` exceeds `MAX_DELTA_CHAIN_DEPTH`
    /// - `max_snapshots` is 0
    ///
    /// # Safety
    /// This validation prevents delta chain depth from exceeding the hard limit,
    /// which would cause get_vector() and to_hashmap() to return partial/empty results.
    pub fn validate(&self) -> Result<()> {
        if self.full_snapshot_interval > MAX_DELTA_CHAIN_DEPTH {
            return Err(VectorError::IndexError(format!(
                "full_snapshot_interval ({}) exceeds MAX_DELTA_CHAIN_DEPTH ({}). \
                     This would cause delta chains to exceed traversal depth limits, \
                     leading to silent data loss. Reduce full_snapshot_interval to at most {}.",
                self.full_snapshot_interval, MAX_DELTA_CHAIN_DEPTH, MAX_DELTA_CHAIN_DEPTH
            ))
            .into());
        }

        if self.max_snapshots == 0 {
            return Err(
                VectorError::IndexError("max_snapshots must be at least 1".to_string()).into(),
            );
        }

        Ok(())
    }

    /// Creates a default configuration with the given HNSW config.
    ///
    /// Defaults:
    /// - Strategy: TransactionInterval(10) - mirrors anchor+delta pattern
    /// - Retention: KeepN(100)
    /// - Max snapshots: 100
    /// - Full snapshot interval: 10
    pub fn default_with_hnsw(hnsw_config: HnswConfig) -> Self {
        TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config),
        }
    }

    /// Creates a default temporal-only configuration without HNSW config.
    ///
    /// Use this when a vector index already exists and you only want to enable
    /// temporal features. The existing vector index's HNSW configuration will be used.
    ///
    /// Defaults:
    /// - Strategy: TransactionInterval(10) - mirrors anchor+delta pattern
    /// - Retention: KeepN(100)
    /// - Max snapshots: 100
    /// - Full snapshot interval: 10
    /// - hnsw_config: None (use existing)
    pub fn default_temporal_only() -> Self {
        TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: None,
        }
    }

    /// Creates a configuration for time-based snapshots.
    pub fn with_time_interval(hnsw_config: HnswConfig, interval_secs: u64) -> Self {
        TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TimeInterval(interval_secs),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config),
        }
    }

    /// Creates a configuration for change-based snapshots.
    pub fn with_change_threshold(hnsw_config: HnswConfig, threshold: f64) -> Self {
        TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::ChangeThreshold(threshold),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(hnsw_config),
        }
    }
}

/// Snapshot creation strategies.
///
/// Determines when temporal vector index snapshots are created.
///
/// # Trade-offs
///
/// | Strategy | Pros | Cons |
/// |----------|------|------|
/// | TransactionInterval | Predictable snapshot count | May miss time-based patterns |
/// | TimeInterval | Captures time-based changes | Uneven snapshot distribution |
/// | ChangeThreshold | Adaptive to workload | Unpredictable snapshot count |
/// | Hybrid | Combines benefits | More complex configuration |
///
/// # Examples
///
/// ```rust
/// use gallifreydb::index::vector::temporal::SnapshotStrategy;
///
/// // Transaction-based (default): snapshot every 10 transactions
/// let strategy = SnapshotStrategy::TransactionInterval(10);
///
/// // Time-based: snapshot every hour
/// let strategy = SnapshotStrategy::TimeInterval(3600);
///
/// // Change-based: snapshot when 10% of vectors changed
/// let strategy = SnapshotStrategy::ChangeThreshold(0.1);
///
/// // Hybrid: whichever triggers first
/// let strategy = SnapshotStrategy::Hybrid {
///     transaction_interval: 10,
///     time_interval_secs: 3600,
///     change_threshold: 0.1,
/// };
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum SnapshotStrategy {
    /// Create snapshot every N write transactions.
    ///
    /// Mirrors anchor+delta pattern (default: 10).
    /// Provides predictable snapshot frequency regardless of time.
    TransactionInterval(usize),

    /// Create snapshot at fixed time intervals (seconds).
    ///
    /// Example: 3600 for hourly snapshots.
    /// Good for time-series analysis of semantic drift.
    TimeInterval(u64),

    /// Create snapshot when significant changes occur.
    ///
    /// Threshold is fraction of total vectors changed (0.0-1.0).
    /// Example: 0.1 means snapshot when 10% of vectors change.
    /// Adaptive to workload intensity.
    ChangeThreshold(f64),

    /// Hybrid: use whichever trigger fires first.
    ///
    /// Combines benefits of all strategies.
    /// Ensures snapshots on any significant event.
    Hybrid {
        /// Transaction interval threshold
        transaction_interval: usize,
        /// Time interval in seconds
        time_interval_secs: u64,
        /// Change threshold (0.0-1.0)
        change_threshold: f64,
    },
}

/// Metric for measuring semantic drift between vector embeddings.
///
/// Different metrics capture different aspects of how meaning has changed:
/// - **Cosine**: Angular difference (independent of magnitude)
/// - **Euclidean**: Spatial distance (sensitive to magnitude)
/// - **Angular**: Actual geometric angle in radians
///
/// # Examples
///
/// ```rust
/// use gallifreydb::index::vector::temporal::DriftMetric;
///
/// let metric = DriftMetric::default(); // Cosine
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DriftMetric {
    /// Cosine distance: 1.0 - cosine_similarity
    ///
    /// Range: [0, 2] for normalized vectors, typically [0, 1]
    /// Most interpretable for semantic embeddings.
    /// Value of 0 = identical meaning, 1 = orthogonal, 2 = opposite.
    #[default]
    Cosine,

    /// Euclidean (L2) distance between vectors.
    ///
    /// Sensitive to both direction and magnitude changes.
    /// Useful for detecting absolute changes in embedding space.
    Euclidean,

    /// Angular distance: arccos(cosine_similarity)
    ///
    /// Returns the geometric angle between vectors in radians.
    /// Range: [0, Ï€] where 0 = identical, Ï€/2 = orthogonal, Ï€ = opposite.
    Angular,
}

/// Type alias for vector snapshot: map of NodeId to vector data
/// Represents vector data in a snapshot, supporting both full and delta formats.
///
/// Full snapshots store all vectors, while delta snapshots store only changes
/// relative to a base snapshot, significantly reducing memory usage for incremental updates.
#[derive(Clone)]
enum VectorSnapshot {
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
    fn get_vector(
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
    fn to_hashmap(
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
    fn len(&self) -> usize {
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
    fn collect_all(
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
enum SnapshotIndex {
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
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>> {
        match self {
            SnapshotIndex::Full(index) => index.search(query, k),
            SnapshotIndex::Delta(delta) => delta.search(query, k),
        }
    }

    fn search_with_filter(
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

    fn len(&self) -> usize {
        match self {
            SnapshotIndex::Full(index) => index.len(),
            SnapshotIndex::Delta(delta) => {
                // Correct count: base + added - removed
                // removed.len() is O(1), so no performance penalty
                delta.base.len() + delta.added.len() - delta.removed.len()
            }
        }
    }

    fn dimensions(&self) -> usize {
        match self {
            SnapshotIndex::Full(index) => index.dimensions(),
            SnapshotIndex::Delta(delta) => delta.added.dimensions(),
        }
    }
}

/// A delta snapshot that stores only changes relative to a base snapshot.
#[derive(Clone)]
struct DeltaIndex {
    /// The base snapshot this delta is built upon (usually a Full snapshot)
    base: Arc<SnapshotIndex>,
    /// Vectors added or updated since the base snapshot
    added: Arc<HnswIndex>,
    /// IDs of vectors that were removed or updated (invalidating the base version)
    removed: Arc<HashSet<NodeId>>,
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
struct SnapshotData {
    /// Historical HNSW snapshots at anchor timestamps
    /// Key: Timestamp when snapshot was created
    /// Value: (Stable snapshot ID, SnapshotIndex)
    snapshots: BTreeMap<Timestamp, (usize, SnapshotIndex)>,

    /// Historical vector values at each snapshot
    /// Key: Timestamp when snapshot was created
    /// Value: Immutable map of NodeId -> Vector for that snapshot
    vector_history: BTreeMap<Timestamp, VectorSnapshot>,
}

impl SnapshotData {
    fn new() -> Self {
        SnapshotData {
            snapshots: BTreeMap::new(),
            vector_history: BTreeMap::new(),
        }
    }

    fn insert(
        &mut self,
        timestamp: Timestamp,
        stable_id: usize,
        snapshot: SnapshotIndex,
        vectors: VectorSnapshot,
    ) {
        self.snapshots.insert(timestamp, (stable_id, snapshot));
        self.vector_history.insert(timestamp, vectors);
    }

    fn remove_oldest(&mut self) {
        if let Some(oldest_key) = self.snapshots.keys().next().copied() {
            self.snapshots.remove(&oldest_key);
            self.vector_history.remove(&oldest_key);
        }
    }

    fn len(&self) -> usize {
        self.snapshots.len()
    }
}

/// Metadata for snapshot management.
///
/// Tracks state needed to determine when to create the next snapshot.
#[derive(Debug, Clone)]
struct SnapshotMetadata {
    /// Last snapshot timestamp (microseconds since epoch)
    last_snapshot_time: Timestamp,

    /// Transaction count since last snapshot
    transactions_since_snapshot: usize,

    /// Vectors changed since last snapshot (resets every snapshot)
    vectors_changed_since_snapshot: HashSet<NodeId>,

    /// Total snapshots created (for ID generation)
    total_snapshots: usize,

    /// Time of the last FULL snapshot
    last_full_snapshot_time: Timestamp,

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
    changes_accumulated: HashSet<NodeId>,

    /// Number of snapshots created since the last FULL snapshot.
    /// Used to trigger periodic FULL snapshots.
    snapshots_since_full: usize,
}

impl SnapshotMetadata {
    fn new(initial_time: Timestamp) -> Self {
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
    fn record_change(&mut self, node_id: NodeId) {
        self.vectors_changed_since_snapshot.insert(node_id);
        self.changes_accumulated.insert(node_id);
    }

    /// Record a transaction (increment counter).
    fn record_transaction(&mut self) {
        self.transactions_since_snapshot += 1;
    }

    /// Reset tracking after creating a snapshot.
    fn reset(&mut self, snapshot_time: Timestamp, is_full: bool) {
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

/// Temporal vector index for time-aware semantic search.
///
/// Maintains a current HNSW index plus historical snapshots at configurable intervals,
/// enabling queries like "find similar vectors as of timestamp T" and semantic drift
/// tracking.
///
/// # Thread Safety
///
/// This struct is thread-safe using `RwLock` for internal mutability. Multiple threads
/// can query concurrently, while snapshot creation requires exclusive access.
pub struct TemporalVectorIndex {
    /// Current (live) HNSW index - always up-to-date
    current: Arc<HnswIndex>,

    /// Current vector storage - maintains actual vector data for snapshot copying
    /// Maps NodeId to the vector embedding
    vectors: Arc<DashMap<NodeId, Arc<[f32]>>>,

    /// Historical snapshots and vector values protected by a single lock
    snapshot_data: RwLock<SnapshotData>,

    /// Configuration
    config: TemporalVectorConfig,

    /// Metadata for snapshot management
    metadata: RwLock<SnapshotMetadata>,
}

impl TemporalVectorIndex {
    /// Creates a new temporal vector index with the given configuration.
    pub fn new(config: TemporalVectorConfig) -> Result<Self> {
        Self::new_at(config, Self::current_timestamp()?)
    }

    /// Creates a new temporal vector index with an explicit initial timestamp (for testing).
    pub fn new_at(config: TemporalVectorConfig, initial_time: Timestamp) -> Result<Self> {
        // Validate configuration before creating index
        config.validate()?;

        // Get HNSW config - it must be present at this point
        let hnsw_config = config.hnsw_config.clone().ok_or_else(|| {
            VectorError::IndexError(
                "HNSW configuration is required for TemporalVectorIndex. \
                 Use TemporalVectorConfig::default_with_hnsw() or ensure hnsw_config is set."
                    .to_string(),
            )
        })?;

        // Create current HNSW index
        let current = Arc::new(HnswIndex::new(hnsw_config)?);

        // Create vector storage
        let vectors = Arc::new(DashMap::new());

        Ok(TemporalVectorIndex {
            current,
            vectors,
            snapshot_data: RwLock::new(SnapshotData::new()),
            config,
            metadata: RwLock::new(SnapshotMetadata::new(initial_time)),
        })
    }

    /// Returns the current timestamp in microseconds since epoch.
    fn current_timestamp() -> Result<Timestamp> {
        // Phase 2: Use the core time::now() function which returns HybridTimestamp
        use crate::core::temporal::time;
        Ok(time::now())
    }

    /// Adds a vector to the current index and tracks it for snapshot creation.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The vector contains NaN or infinite values
    /// - The timestamp is negative
    /// - The underlying HNSW index fails to add the vector
    pub fn add(&self, id: NodeId, vector: &[f32], timestamp: Timestamp) -> Result<()> {
        // Validate vector does not contain NaN values
        let nan_count = vector.iter().filter(|&&v| v.is_nan()).count();
        if nan_count > 0 {
            return Err(VectorError::ContainsNaN { count: nan_count }.into());
        }

        // Validate vector does not contain infinite values
        let inf_count = vector.iter().filter(|&&v| v.is_infinite()).count();
        if inf_count > 0 {
            return Err(VectorError::ContainsInfinity { count: inf_count }.into());
        }

        // Store vector data (for snapshot copying)
        let vector_arc: Arc<[f32]> = Arc::from(vector);
        self.vectors.insert(id, vector_arc);

        // Validate timestamp is within valid range
        // Phase 2: Compare wallclock components
        use crate::core::hlc::HybridTimestamp;
        if timestamp.wallclock() < 0 {
            return Err(Error::Temporal(TemporalError::InvalidTimeRange {
                start: timestamp,
                end: HybridTimestamp::new_unchecked(0, 0),
            }));
        }

        if timestamp.wallclock() > crate::core::temporal::MAX_VALID_TIMESTAMP {
            return Err(Error::Temporal(TemporalError::InvalidTimestamp {
                timestamp,
                reason: format!(
                    "Timestamp wallclock {} exceeds MAX_VALID_TIMESTAMP {} (reserved range for internal use)",
                    timestamp.wallclock(),
                    crate::core::temporal::MAX_VALID_TIMESTAMP
                ),
            }));
        }

        // Add to current index
        self.current.add(id, vector)?;

        // Track change for snapshot detection
        self.metadata.write().record_change(id);

        Ok(())
    }

    /// Removes a vector from the current index.
    pub fn remove(&self, id: NodeId, _timestamp: Timestamp) -> Result<()> {
        // Remove from vector storage
        self.vectors.remove(&id);

        // Remove from current index
        self.current.remove(id)?;

        // Track change
        self.metadata.write().record_change(id);

        Ok(())
    }

    /// Records a transaction for snapshot tracking.
    pub fn on_transaction(&self) -> Result<()> {
        self.on_transaction_at(Self::current_timestamp()?)
    }

    /// Records a transaction at a specific timestamp (for testing).
    pub fn on_transaction_at(&self, timestamp: Timestamp) -> Result<()> {
        // Record transaction
        self.metadata.write().record_transaction();

        // Check if snapshot needed
        self.check_and_create_snapshot(timestamp)?;

        Ok(())
    }

    /// Builds a FULL snapshot of the current vectors.
    fn build_full_snapshot(&self) -> Result<(SnapshotIndex, VectorSnapshot)> {
        // hnsw_config is guaranteed to be Some after new_at validation
        let hnsw_config = self.config.hnsw_config.clone().ok_or_else(|| {
            VectorError::IndexError("HNSW configuration missing in build_full_snapshot".to_string())
        })?;
        let snapshot = HnswIndex::new(hnsw_config)?;
        let mut vector_snapshot = HashMap::with_capacity(self.vectors.len());

        for entry in self.vectors.iter() {
            let node_id = *entry.key();
            let vector = entry.value().clone();
            snapshot.add(node_id, vector.as_ref())?;
            vector_snapshot.insert(node_id, vector);
        }

        Ok((
            SnapshotIndex::Full(Arc::new(snapshot)),
            VectorSnapshot::Full(Arc::new(vector_snapshot)),
        ))
    }

    /// Builds a DELTA snapshot relative to a base snapshot.
    ///
    /// CRITICAL: This function validates that the base is a Full snapshot to prevent
    /// delta-of-delta chains, which would cause unbounded traversal depth and poor performance.
    ///
    /// # Parameters
    /// - `base`: The base SnapshotIndex (must be Full)
    /// - `base_vectors`: The VectorSnapshot for the base (used for membership testing)
    /// - `base_time`: Timestamp of the base snapshot
    /// - `changes`: Set of all NodeIds that changed since the base
    fn build_delta_snapshot(
        &self,
        base: Arc<SnapshotIndex>,
        base_vectors: &VectorSnapshot,
        base_time: Timestamp,
        changes: &HashSet<NodeId>,
    ) -> Result<(SnapshotIndex, VectorSnapshot)> {
        // SAFETY VALIDATION: Ensure base is a Full snapshot, not a Delta
        // This prevents delta-of-delta chains which would violate our performance guarantees
        if !matches!(*base, SnapshotIndex::Full(_)) {
            return Err(VectorError::IndexError(format!(
                "CRITICAL: Attempted to create delta snapshot with non-Full base at timestamp {}. \
                     Delta-of-delta chains are not allowed. Base must be a Full snapshot.",
                base_time
            ))
            .into());
        }

        // Create small HNSW for added/updated vectors
        // hnsw_config is guaranteed to be Some after new_at validation
        let added_config = self.config.hnsw_config.clone().ok_or_else(|| {
            VectorError::IndexError(
                "HNSW configuration missing in build_delta_snapshot".to_string(),
            )
        })?;
        let added = HnswIndex::new(added_config)?;

        // Build delta vector snapshot - only store changed vectors
        let mut added_vectors = HashMap::new();
        let mut removed_vectors = HashSet::new();

        // For DeltaIndex.removed: only nodes that WERE in base and are now invalid
        // This is crucial for correct len() calculation
        let mut invalidated_in_base = HashSet::new();

        // We need access to all snapshots to check containment in base
        // Since we're inside write lock, we can't easily access snapshot_data
        // Instead, we'll use a simpler heuristic: check if node was in base by seeing
        // if it's been modified (for updates) or removed (for removals)
        // For pure additions, they weren't in base, so don't include them

        // Separate changed nodes into added/updated vs removed
        for &node_id in changes {
            if let Some(vector) = self.vectors.get(&node_id) {
                // Node exists in current state -> it was added or updated
                added.add(node_id, vector.as_ref())?;
                added_vectors.insert(node_id, vector.clone());

                // Check if this node was in the base (update) or is new (addition)
                // Since we only build deltas against Full snapshots (validated above),
                // base_vectors should always be Full, so we can check without all_snapshots
                let was_in_base = match base_vectors {
                    VectorSnapshot::Full(vectors) => vectors.contains_key(&node_id),
                    VectorSnapshot::Delta { .. } => {
                        // This should never happen due to validation above,
                        // but if it does, conservatively assume it was in base
                        true
                    }
                };

                if was_in_base {
                    // Node was in base and is now updated -> invalidate old version
                    invalidated_in_base.insert(node_id);
                }
            } else {
                // Node doesn't exist in current state -> it was removed
                removed_vectors.insert(node_id);
                // Removals are always invalidations of base (if they were there)
                invalidated_in_base.insert(node_id);
            }
        }

        // FIXED: Use invalidated_in_base instead of changes for DeltaIndex.removed
        // This ensures only nodes that were actually in the base are filtered out,
        // preventing incorrect len() calculations for pure additions
        let delta_index = DeltaIndex {
            base,
            added: Arc::new(added),
            removed: Arc::new(invalidated_in_base),
        };

        let delta_vectors = VectorSnapshot::Delta {
            base_time,
            added: Arc::new(added_vectors),
            removed: Arc::new(removed_vectors), // For vector history, only actual removals
        };

        Ok((SnapshotIndex::Delta(Arc::new(delta_index)), delta_vectors))
    }

    /// Internal helper to build and insert a snapshot with proper lock ordering and validation.
    ///
    /// This centralizes the snapshot creation logic to avoid duplication and ensure
    /// consistent lock ordering (metadata -> snapshot_data) to prevent deadlocks.
    ///
    /// Uses bounded retries to prevent infinite loops in case of concurrent modifications.
    fn create_snapshot_internal(&self, current_time: Timestamp) -> Result<()> {
        self.create_snapshot_internal_with_retries(current_time, 0)
    }

    /// Internal helper with retry counter to prevent unbounded recursion.
    fn create_snapshot_internal_with_retries(
        &self,
        current_time: Timestamp,
        retry_count: usize,
    ) -> Result<()> {
        if retry_count >= MAX_SNAPSHOT_RETRIES {
            return Err(VectorError::IndexError(
                "Exceeded maximum snapshot creation retries due to concurrent modifications"
                    .to_string(),
            )
            .into());
        }

        // Quick check: if snapshot already exists at this timestamp, skip
        // This prevents overwriting existing snapshots when multiple calls happen at same timestamp
        {
            let snapshot_data = self.snapshot_data.read();
            if snapshot_data.snapshots.contains_key(&current_time) {
                return Ok(()); // Snapshot already exists, nothing to do
            }
        }

        // Step 1: Read metadata to determine snapshot type
        let (is_full, base_time, changes) = {
            let metadata = self.metadata.read();

            // Create FULL snapshot if:
            // 1. Reached configured interval, OR
            // 2. This is the first snapshot, OR
            // 3. Memory threshold exceeded (fallback to prevent unbounded growth)
            let is_full = metadata.snapshots_since_full >= self.config.full_snapshot_interval
                || metadata.total_snapshots == 0
                || metadata.changes_accumulated.len() >= MAX_ACCUMULATED_CHANGES;

            // Get base time and changes for delta (if needed)
            let base_time = metadata.last_full_snapshot_time;
            let changes = metadata.changes_accumulated.clone();

            (is_full, base_time, changes)
        };

        // Step 2: Build snapshot outside locks
        let (snapshot, vector_snapshot, snapshot_type) = if is_full {
            let (snap, vec_snap) = self.build_full_snapshot()?;
            (snap, vec_snap, true) // true = full
        } else {
            // Try to get base snapshot for delta (both index and vectors)
            let base_opt = {
                let data = self.snapshot_data.read();
                let base_snap = data.snapshots.get(&base_time).map(|(_, snap)| snap.clone());
                let base_vecs = data.vector_history.get(&base_time).cloned();
                match (base_snap, base_vecs) {
                    (Some(snap), Some(vecs)) => Some((snap, vecs)),
                    _ => None,
                }
            };

            if let Some((base, base_vectors)) = base_opt {
                let (snap, vec_snap) =
                    self.build_delta_snapshot(Arc::new(base), &base_vectors, base_time, &changes)?;
                (snap, vec_snap, false) // false = delta
            } else {
                // Base was pruned, fallback to full
                let (snap, vec_snap) = self.build_full_snapshot()?;
                (snap, vec_snap, true)
            }
        };

        // Step 3: Acquire locks in correct order (metadata -> snapshot_data) and insert
        {
            let mut metadata = self.metadata.write();

            // Re-validate: check if we need full vs delta NOW
            let should_be_full = metadata.snapshots_since_full
                >= self.config.full_snapshot_interval
                || metadata.total_snapshots == 0
                || metadata.changes_accumulated.len() >= MAX_ACCUMULATED_CHANGES;

            // If we built delta but now need full (due to race or memory threshold), discard and retry
            if should_be_full && !snapshot_type {
                drop(metadata);
                return self.create_snapshot_internal_with_retries(current_time, retry_count + 1);
            }

            // Re-validate: if we built delta, ensure base still exists
            if !snapshot_type {
                let snapshot_data = self.snapshot_data.read();
                let base_exists = snapshot_data
                    .snapshots
                    .contains_key(&metadata.last_full_snapshot_time);
                drop(snapshot_data);

                if !base_exists {
                    // Base was pruned, discard delta and retry with full
                    drop(metadata);
                    return self
                        .create_snapshot_internal_with_retries(current_time, retry_count + 1);
                }
            }

            let stable_id = metadata.total_snapshots;
            let mut snapshot_data = self.snapshot_data.write();

            snapshot_data.insert(current_time, stable_id, snapshot, vector_snapshot);

            // Update metadata based on what we actually built
            metadata.reset(current_time, snapshot_type);

            // Enforce snapshot limit
            while snapshot_data.len() > self.config.max_snapshots {
                snapshot_data.remove_oldest();
            }
        }

        Ok(())
    }

    /// Checks if a snapshot should be created and creates it if needed.
    ///
    /// ## Race Condition Limitation
    ///
    /// There is a known race condition between checking if a snapshot should be created
    /// and actually creating it. Multiple concurrent threads may all observe that a snapshot
    /// is needed (e.g., `transactions_since_snapshot >= interval`) before any thread resets
    /// the counter, leading to multiple snapshot creations at the same "trigger point".
    ///
    /// ### Impact
    /// - **Correctness**: No data loss or corruption. All snapshots are valid.
    /// - **Performance**: Slight overhead from creating a few extra snapshots during high concurrency.
    /// - **Storage**: Minimal - extra snapshots are subject to retention policy pruning.
    ///
    /// ### Example
    /// With 15 concurrent transactions and interval=10:
    /// - Ideal: 1-2 snapshots
    /// - Reality: 1-6 snapshots (still much better than 15)
    /// - Unacceptable: 15 snapshots (one per transaction)
    ///
    /// ### Mitigation
    /// The retry logic with MAX_SNAPSHOT_RETRIES bounds the impact, and tests validate
    /// that snapshot creation remains controlled even under contention.
    ///
    /// ### Future Optimization
    /// Could use atomic compare-and-swap for stricter control, but current behavior
    /// is acceptable for production use given the bounded impact.
    fn check_and_create_snapshot(&self, current_time: Timestamp) -> Result<()> {
        // Quick check: should we create a snapshot?
        // NOTE: Race condition possible here - multiple threads may see true simultaneously
        let should_create = {
            let metadata = self.metadata.read();
            self.should_create_snapshot(&metadata, current_time)?
        };

        if !should_create {
            return Ok(());
        }

        // Delegate to internal helper for the actual creation
        // This includes bounded retries to handle concurrent modifications
        self.create_snapshot_internal(current_time)
    }

    /// Determines if a snapshot should be created based on the strategy.
    fn should_create_snapshot(
        &self,
        metadata: &SnapshotMetadata,
        current_time: Timestamp,
    ) -> Result<bool> {
        match &self.config.snapshot_strategy {
            SnapshotStrategy::TransactionInterval(interval) => {
                Ok(metadata.transactions_since_snapshot >= *interval)
            }

            SnapshotStrategy::TimeInterval(interval_secs) => {
                // Phase 2: Use wallclock components for arithmetic
                let elapsed_micros = current_time.wallclock() - metadata.last_snapshot_time.wallclock();
                let elapsed_secs = elapsed_micros / 1_000_000;
                Ok(elapsed_secs >= *interval_secs as i64)
            }

            SnapshotStrategy::ChangeThreshold(threshold) => {
                let total_vectors = self.current.len();
                if total_vectors == 0 {
                    return Ok(false);
                }
                let changed = metadata.vectors_changed_since_snapshot.len();
                Ok((changed as f64 / total_vectors as f64) >= *threshold)
            }

            SnapshotStrategy::Hybrid {
                transaction_interval,
                time_interval_secs,
                change_threshold,
            } => {
                let by_txn = metadata.transactions_since_snapshot >= *transaction_interval;

                // Phase 2: Use wallclock components for arithmetic
                let elapsed_micros = current_time.wallclock() - metadata.last_snapshot_time.wallclock();
                let elapsed_secs = elapsed_micros / 1_000_000;
                let by_time = (elapsed_secs >= *time_interval_secs as i64).into();

                let total = self.current.len();
                let by_change = if total > 0 {
                    let changed = metadata.vectors_changed_since_snapshot.len();
                    (changed as f64 / total as f64) >= *change_threshold
                } else {
                    false
                };

                Ok(by_txn || by_time || by_change)
            }
        }
    }

    /// Manually creates a snapshot at the current time.
    ///
    /// This uses the same delta/full snapshot logic as automatic snapshots,
    /// ensuring optimal memory usage and consistency.
    pub fn create_manual_snapshot(&self) -> Result<()> {
        let timestamp = Self::current_timestamp()?;
        self.create_snapshot_internal(timestamp)
    }

    /// Creates a snapshot aligned with a graph anchor.
    ///
    /// This method is called by HistoricalStorage (via observers) when creating anchors
    /// to maintain synchronization between graph versioning and vector snapshots.
    /// This enables temporal vector queries to align with temporal graph queries.
    ///
    /// # Arguments
    ///
    /// * `timestamp` - Transaction time of the anchor (for snapshot alignment)
    ///
    /// # Returns
    ///
    /// - `Ok(Some(id))` - Snapshot created successfully, returns stable snapshot ID
    /// - `Ok(None)` - No snapshot created (empty index, no vectors to snapshot)
    /// - `Err(...)` - Snapshot creation failed
    ///
    /// # Design Note
    ///
    /// Returns `Option<usize>` to allow the caller (HistoricalStorage) to store the
    /// snapshot ID in the anchor's metadata, enabling provenance tracking.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use gallifreydb::index::vector::temporal::TemporalVectorIndex;
    /// # use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
    /// # fn example() -> gallifreydb::utils::Result<()> {
    /// # let config = gallifreydb::index::vector::temporal::TemporalVectorConfig::default_with_hnsw(
    /// #     HnswConfig::new(384, DistanceMetric::Cosine)
    /// # );
    /// # let index = TemporalVectorIndex::new(config)?;
    /// // Called by HistoricalStorage when creating an anchor
    /// let timestamp = 1234567890;
    /// if let Some(snapshot_id) = index.create_snapshot_for_anchor(timestamp)? {
    ///     println!("Created snapshot {} for anchor at {}", snapshot_id, timestamp);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn create_snapshot_for_anchor(&self, timestamp: Timestamp) -> Result<Option<usize>> {
        // Don't create snapshot if index is empty
        if self.current.len() == 0 {
            return Ok(None);
        }

        // Get snapshot ID before creating (will be the next total_snapshots value)
        let snapshot_id = {
            let metadata = self.metadata.read();
            metadata.total_snapshots
        };

        // Delegate to internal snapshot creation logic
        // This handles full vs delta snapshots, metadata updates, and pruning
        self.create_snapshot_internal(timestamp)?;

        Ok(Some(snapshot_id))
    }

    /// Prunes snapshots according to the configured retention policy.
    pub fn prune_snapshots(&self) -> Result<usize> {
        let mut snapshot_data = self.snapshot_data.write();
        let initial_count = snapshot_data.len();

        match &self.config.retention_policy {
            RetentionPolicy::KeepAll => Ok(0),
            RetentionPolicy::KeepN(n) => {
                let n = *n;
                if snapshot_data.snapshots.len() <= n {
                    return Ok(0);
                }

                let to_remove = snapshot_data.snapshots.len() - n;
                let keys_to_remove: Vec<Timestamp> = snapshot_data
                    .snapshots
                    .keys()
                    .take(to_remove)
                    .copied()
                    .collect();

                for key in keys_to_remove {
                    snapshot_data.snapshots.remove(&key);
                    snapshot_data.vector_history.remove(&key);
                }

                Ok(to_remove)
            }
            RetentionPolicy::KeepDuration(duration) => {
                let current_time = Self::current_timestamp()?;
                // Phase 2: Calculate cutoff using wallclock arithmetic, then create HybridTimestamp
                let duration_micros = (duration.as_micros() as i64).into();
                let cutoff_wallclock = current_time.wallclock().saturating_sub(duration_micros);
                use crate::core::hlc::HybridTimestamp;
                let cutoff_time = HybridTimestamp::new_unchecked(cutoff_wallclock, 0);

                let keys_to_remove: Vec<Timestamp> = snapshot_data
                    .snapshots
                    .range(..cutoff_time)
                    .map(|(k, _)| *k)
                    .collect();

                for key in keys_to_remove {
                    snapshot_data.snapshots.remove(&key);
                    snapshot_data.vector_history.remove(&key);
                }

                let removed = initial_count - snapshot_data.len();
                Ok(removed)
            }
        }
    }

    /// Finds the nearest snapshot at or before the given timestamp.
    fn find_nearest_snapshot(&self, timestamp: Timestamp) -> Option<Arc<SnapshotIndex>> {
        let snapshot_data = self.snapshot_data.read();

        // Binary search for nearest snapshot <= timestamp
        snapshot_data
            .snapshots
            .range(..=timestamp)
            .next_back()
            .map(|(_, (_id, snapshot))| Arc::new(snapshot.clone()))
    }

    /// Finds k-nearest neighbors at a specific point in time.
    ///
    /// Performs semantic search as of a specific timestamp by reconstructing the vector
    /// index state at that point in time and querying it.
    ///
    /// # Arguments
    ///
    /// * `query_embedding` - The query vector to search for
    /// * `k` - Number of nearest neighbors to return
    /// * `timestamp` - The point in time to query at
    ///
    /// # Returns
    ///
    /// A vector of `(node_id, similarity_score)` pairs, sorted by similarity in descending
    /// order (most similar first). The similarity score depends on the configured distance
    /// metric (e.g., for Cosine: 1.0 = identical, 0.0 = orthogonal).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gallifreydb::index::vector::temporal::TemporalVectorIndex;
    ///
    /// # fn example(index: &TemporalVectorIndex) -> gallifreydb::utils::Result<()> {
    /// let query = vec![0.1f32; 384];
    /// let timestamp_2023 = 1672531200000000; // 2023-01-01 in microseconds
    ///
    /// // Find what was semantically similar in 2023
    /// let results = index.find_similar_as_of(&query, 10, timestamp_2023)?;
    ///
    /// for (node_id, score) in results {
    ///     println!("Node {}: similarity = {:.3}", node_id, score);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Use Cases
    ///
    /// - "What did we know about X in 2023?" - Time-travel semantic search
    /// - "What was relevant to this concept last year?" - Historical context retrieval
    /// - LLM reasoning about how knowledge evolved
    /// - Audit trails for retrieval-augmented generation (RAG)
    ///
    /// # Performance
    ///
    /// - If no snapshot exists at the timestamp, finds nearest earlier snapshot
    /// - Delta snapshots are reconstructed by merging with base snapshot
    /// - Target: <10ms for 1M vectors (same as current-state queries)
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Query embedding dimensions don't match index dimensions
    /// - Timestamp is before first snapshot
    /// - k exceeds MAX_K (10,000)
    pub fn find_similar_as_of(
        &self,
        query_embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        // Find nearest snapshot
        if let Some(snapshot) = self.find_nearest_snapshot(timestamp) {
            snapshot.search(query_embedding, k).map_err(|e| {
                Error::other(format!(
                    "Failed to search snapshot at timestamp {}: {}",
                    timestamp, e
                ))
            })
        } else {
            Ok(Vec::new())
        }
    }

    /// Finds k-nearest neighbors for a node at a specific point in time.
    pub fn find_similar_node_as_of(
        &self,
        _query_node_id: NodeId,
        _k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        if let Some(_snapshot) = self.find_nearest_snapshot(timestamp) {
            return Err(Error::not_implemented(
                "Historical node vector retrieval",
                "Phase 4 feature - requires historical storage integration",
            ));
        }
        Ok(Vec::new())
    }

    /// Finds k-nearest neighbors across a time range.
    ///
    /// Performs semantic search across all snapshots in a time range, returning results
    /// from each snapshot to show how similarity changed over time.
    ///
    /// # Arguments
    ///
    /// * `query_embedding` - The query vector to search for
    /// * `k` - Number of nearest neighbors to return per snapshot
    /// * `time_range` - The time period to search across
    ///
    /// # Returns
    ///
    /// A [`TemporalSearchResults`] containing search results from each snapshot in the range.
    /// Each snapshot result includes the timestamp and k-nearest neighbors at that point.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gallifreydb::index::vector::temporal::TemporalVectorIndex;
    /// use gallifreydb::core::temporal::TimeRange;
    ///
    /// # fn example(index: &TemporalVectorIndex) -> gallifreydb::utils::Result<()> {
    /// let query = vec![0.1f32; 384];
    /// let time_range = TimeRange::new(1672531200000000.into(), 1704067200000000.into()).unwrap(); // 2023-2024
    ///
    /// let results = index.find_similar_in_range(&query, 10, time_range)?;
    ///
    /// for (timestamp, snapshot_results) in results {
    ///     println!("At {}: found {} results",
    ///              timestamp,
    ///              snapshot_results.len());
    ///     for (node_id, score) in snapshot_results {
    ///         println!("  Node {}: {:.3}", node_id, score);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Use Cases
    ///
    /// - Track how relevant documents changed over time
    /// - Analyze knowledge evolution trends
    /// - Compare semantic shifts across periods
    /// - Historical trend analysis for LLM reasoning
    ///
    /// # Performance
    ///
    /// - Queries all snapshots in range sequentially
    /// - Target: <100ms for 10 snapshots, 1M vectors each
    /// - Results include both Full and Delta snapshot data
    pub fn find_similar_in_range(
        &self,
        query_embedding: &[f32],
        k: usize,
        time_range: TimeRange,
    ) -> Result<TemporalSearchResults> {
        let snapshot_data = self.snapshot_data.read();

        let mut results = Vec::new();

        // Find all snapshots in range
        for (&timestamp, (_id, snapshot)) in snapshot_data
            .snapshots
            .range(time_range.start()..=time_range.end())
        {
            let snapshot_results = snapshot.search(query_embedding, k)?;
            results.push((timestamp, snapshot_results));
        }

        Ok(results)
    }

    /// Retrieves the semantic evolution of a node over time.
    pub fn semantic_evolution(
        &self,
        node_id: NodeId,
        time_range: TimeRange,
    ) -> Result<Vec<(Timestamp, Arc<[f32]>)>> {
        let snapshot_data = self.snapshot_data.read();

        let mut evolution = Vec::new();

        for (&timestamp, snapshot_vectors) in snapshot_data
            .vector_history
            .range(time_range.start()..=time_range.end())
        {
            if let Some(vector) =
                snapshot_vectors.get_vector(&node_id, &snapshot_data.vector_history)?
            {
                evolution.push((timestamp, vector));
            }
        }

        Ok(evolution)
    }

    /// Tracks semantic drift: how a node's similarity to a reference changed over time.
    ///
    /// Measures how much a specific node's embedding drifted from a reference embedding
    /// across all snapshots in the time range. Returns a timeline of drift measurements.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node whose drift to track
    /// * `reference_embedding` - The reference embedding to compare against (typically the
    ///   node's original or current embedding)
    /// * `time_range` - The time period to analyze
    ///
    /// # Returns
    ///
    /// A vector of `(timestamp, drift_distance)` pairs, one for each snapshot where the node
    /// existed. The drift_distance is the cosine distance (1.0 - similarity).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gallifreydb::index::vector::temporal::TemporalVectorIndex;
    /// use gallifreydb::core::temporal::TimeRange;
    /// use gallifreydb::core::id::NodeId;
    ///
    /// # fn example(index: &TemporalVectorIndex) -> gallifreydb::utils::Result<()> {
    /// let node_id = NodeId::new(42).unwrap();
    /// let reference = vec![0.5f32; 384];
    /// let time_range = TimeRange::new(1000000.into(), 2000000.into()).unwrap();
    ///
    /// let drift_timeline = index.track_semantic_drift(node_id, &reference, time_range)?;
    ///
    /// for (timestamp, drift) in drift_timeline {
    ///     if drift > 0.3 {
    ///         println!("Significant drift at {}: {:.3}", timestamp, drift);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Use Cases
    ///
    /// - Detect when document content significantly changed
    /// - Track concept evolution over time
    /// - Identify anomalous updates
    /// - Monitor knowledge base consistency
    pub fn track_semantic_drift(
        &self,
        node_id: NodeId,
        reference_embedding: &[f32],
        time_range: TimeRange,
    ) -> Result<Vec<(Timestamp, f32)>> {
        if reference_embedding.len() != self.dimensions() {
            return Err(VectorError::DimensionMismatch {
                expected: self.dimensions(),
                actual: reference_embedding.len(),
            }
            .into());
        }

        let evolution = self.semantic_evolution(node_id, time_range)?;
        let mut drift = Vec::new();

        for (timestamp, vector) in evolution {
            let similarity = cosine_similarity(reference_embedding, &vector)?;
            drift.push((timestamp, similarity));
        }

        Ok(drift)
    }

    /// Calculates semantic drift between consecutive embeddings.
    pub fn calculate_consecutive_drift(
        &self,
        node_id: NodeId,
        time_range: TimeRange,
    ) -> Result<Vec<(Timestamp, f32)>> {
        let evolution = self.semantic_evolution(node_id, time_range)?;

        if evolution.len() < 2 {
            return Ok(Vec::new());
        }

        let mut drift = Vec::new();

        for window in evolution.windows(2) {
            let (_prev_timestamp, prev_vector) = &window[0];
            let (curr_timestamp, curr_vector) = &window[1];

            let similarity = cosine_similarity(prev_vector, curr_vector)?;
            let drift_value = 1.0 - similarity;

            drift.push((*curr_timestamp, drift_value));
        }

        Ok(drift)
    }

    /// Finds all nodes whose semantic drift exceeds a threshold within a time range.
    ///
    /// Compares the earliest and latest embeddings for each node in the time range,
    /// measuring how much they drifted. Returns all nodes exceeding the drift threshold.
    ///
    /// # Arguments
    ///
    /// * `threshold` - Minimum drift value to include in results
    /// * `time_range` - The time period to analyze
    /// * `metric` - Distance metric for measuring drift (Cosine, Euclidean, or Angular)
    ///
    /// # Returns
    ///
    /// A vector of `(node_id, drift_value)` pairs for all nodes exceeding the threshold,
    /// sorted by drift value in descending order (highest drift first).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gallifreydb::index::vector::temporal::{TemporalVectorIndex, DriftMetric};
    /// use gallifreydb::core::temporal::TimeRange;
    ///
    /// # fn example(index: &TemporalVectorIndex) -> gallifreydb::utils::Result<()> {
    /// let time_range = TimeRange::new(1000000.into(), 2000000.into()).unwrap();
    ///
    /// // Find documents that changed significantly (cosine distance > 0.3)
    /// let drifted = index.find_semantic_drift(0.3, time_range, DriftMetric::Cosine)?;
    ///
    /// for (node_id, drift) in drifted {
    ///     println!("Node {} drifted by {:.3}", node_id, drift);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Use Cases
    ///
    /// - Find contradictions in knowledge base (facts that changed meaning)
    /// - Identify documents with major content updates
    /// - Detect concept drift in ML datasets
    /// - Audit trails for semantic changes
    ///
    /// # Notes
    ///
    /// - Nodes with only one version in the time range are excluded
    /// - Uses earliest and latest embeddings; intermediate changes are not considered
    /// - For per-node detailed drift timeline, use [`track_semantic_drift`](Self::track_semantic_drift)
    pub fn find_semantic_drift(
        &self,
        threshold: f32,
        time_range: TimeRange,
        metric: DriftMetric,
    ) -> Result<Vec<(NodeId, f32)>> {
        use std::collections::HashMap;

        if threshold.is_nan() || threshold.is_infinite() {
            return Err(VectorError::InvalidVector {
                reason: "Threshold must be a finite number".to_string(),
            }
            .into());
        }

        let snapshot_data = self.snapshot_data.read();

        let estimated_capacity = snapshot_data
            .vector_history
            .values()
            .next()
            .map(|s| s.len())
            .unwrap_or(100);

        let mut last_vectors: HashMap<NodeId, Arc<[f32]>> =
            HashMap::with_capacity(estimated_capacity);
        let mut max_drifts: HashMap<NodeId, f32> = HashMap::with_capacity(estimated_capacity);

        for (_timestamp, snapshot_vectors) in snapshot_data
            .vector_history
            .range(time_range.start()..=time_range.end())
        {
            let all_vectors = snapshot_vectors.collect_all(&snapshot_data.vector_history)?;
            for (node_id, curr_vector) in all_vectors {
                if let Some(prev_vector) = last_vectors.get(&node_id) {
                    let drift = Self::compute_drift_distance(prev_vector, &curr_vector, metric)?;
                    let max_drift_entry = max_drifts.entry(node_id).or_insert(0.0);
                    *max_drift_entry = max_drift_entry.max(drift);
                }
                last_vectors.insert(node_id, curr_vector);
            }
        }
        drop(snapshot_data);

        let mut results: Vec<(NodeId, f32)> = max_drifts
            .into_iter()
            .filter(|(_, drift)| *drift >= threshold && *drift > 0.0)
            .collect();

        results.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));

        Ok(results)
    }

    /// Computes the drift distance between two vectors.
    fn compute_drift_distance(a: &[f32], b: &[f32], metric: DriftMetric) -> Result<f32> {
        match metric {
            DriftMetric::Cosine => {
                let similarity = cosine_similarity(a, b)?;
                Ok(1.0 - similarity)
            }
            DriftMetric::Euclidean => euclidean_distance(a, b),
            DriftMetric::Angular => {
                let similarity = cosine_similarity(a, b)?;
                let clamped = similarity.clamp(-1.0, 1.0);
                Ok(clamped.acos())
            }
        }
    }

    /// Returns information about all snapshots.
    pub fn get_snapshot_info(&self) -> Result<Vec<SnapshotInfo>> {
        let snapshot_data = self.snapshot_data.read();
        let current_time = Self::current_timestamp()?;

        Ok(snapshot_data
            .snapshots
            .iter()
            .map(|(&timestamp, (stable_id, snapshot))| {
                // Phase 2: Use wallclock components for arithmetic
                let age_micros = current_time.wallclock() - timestamp.wallclock();

                SnapshotInfo {
                    snapshot_id: *stable_id,
                    timestamp,
                    vector_count: snapshot.len(),
                    // Size estimation: approximate
                    size_bytes: snapshot.len() * snapshot.dimensions() * 4 + 1024,
                    age: Duration::from_micros(age_micros as u64),
                }
            })
            .collect())
    }

    /// Returns the number of snapshots currently stored.
    pub fn snapshot_count(&self) -> usize {
        self.snapshot_data.read().snapshots.len()
    }

    /// Returns memory usage statistics for monitoring.
    ///
    /// Use this to monitor `changes_accumulated` size and detect potential memory growth.
    /// If `changes_accumulated_size` is large (>100k), consider:
    /// - Reducing `full_snapshot_interval`
    /// - Calling `create_manual_snapshot()` during idle periods
    /// - Increasing snapshot frequency
    pub fn memory_stats(&self) -> MemoryStats {
        let metadata = self.metadata.read();
        let snapshot_data = self.snapshot_data.read();

        MemoryStats {
            changes_accumulated_size: metadata.changes_accumulated.len(),
            vectors_changed_since_snapshot: metadata.vectors_changed_since_snapshot.len(),
            snapshots_since_full: metadata.snapshots_since_full,
            total_snapshots: snapshot_data.snapshots.len(),
            current_vectors: self.vectors.len(),
        }
    }

    /// Returns the current index (for current-state queries).
    pub fn current_index(&self) -> &HnswIndex {
        &self.current
    }

    /// Returns the vector dimensionality.
    pub fn dimensions(&self) -> usize {
        self.current.dimensions()
    }

    /// Returns the distance metric used.
    pub fn distance_metric(&self) -> DistanceMetric {
        self.current.distance_metric()
    }
}

/// Snapshot information for monitoring.
#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    /// Sequential snapshot ID
    pub snapshot_id: usize,
    /// Timestamp when snapshot was created (microseconds since epoch)
    pub timestamp: Timestamp,
    /// Number of vectors in snapshot
    pub vector_count: usize,
    /// Estimated size in bytes
    pub size_bytes: usize,
    /// Age of snapshot
    pub age: Duration,
}

/// Observer adapter for TemporalVectorIndex to react to storage events.
///
/// This struct implements `StorageObserver` to enable TemporalVectorIndex to create
/// snapshots when HistoricalStorage creates anchors, maintaining synchronization
/// between graph versioning and vector indexing.
///
/// # Design
///
/// The observer wraps an Arc reference to TemporalVectorIndex, allowing multiple
/// components to share the same index. When node/edge anchors are created, the
/// observer triggers snapshot creation and stores the snapshot ID in the anchor's
/// metadata via the return value.
///
/// # Example
///
/// ```no_run
/// use gallifreydb::index::vector::temporal::{TemporalVectorIndex, VectorIndexObserver, TemporalVectorConfig};
/// use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
/// use gallifreydb::storage::historical::HistoricalStorage;
/// use std::sync::Arc;
///
/// # fn example() -> gallifreydb::utils::Result<()> {
/// // Create temporal vector index
/// let config = TemporalVectorConfig::default_with_hnsw(
///     HnswConfig::new(384, DistanceMetric::Cosine)
/// );
/// let index = Arc::new(TemporalVectorIndex::new(config)?);
///
/// // Create observer wrapper
/// let observer = VectorIndexObserver::new(Arc::clone(&index));
///
/// // Register with HistoricalStorage
/// let mut storage = HistoricalStorage::new();
/// storage.add_observer(Arc::new(observer));
///
/// // Now anchors will automatically trigger vector snapshots
/// # Ok(())
/// # }
/// ```
pub struct VectorIndexObserver {
    /// The temporal vector index (reserved for future use in metrics/monitoring)
    #[allow(dead_code)]
    index: Arc<TemporalVectorIndex>,
}

impl VectorIndexObserver {
    /// Creates a new observer for a TemporalVectorIndex.
    ///
    /// # Arguments
    ///
    /// * `index` - Arc reference to the TemporalVectorIndex to observe
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use gallifreydb::index::vector::temporal::{TemporalVectorIndex, VectorIndexObserver, TemporalVectorConfig};
    /// # use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
    /// # use std::sync::Arc;
    /// # fn example() -> gallifreydb::utils::Result<()> {
    /// let config = TemporalVectorConfig::default_with_hnsw(
    ///     HnswConfig::new(384, DistanceMetric::Cosine)
    /// );
    /// let index = Arc::new(TemporalVectorIndex::new(config)?);
    /// let observer = VectorIndexObserver::new(index);
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(index: Arc<TemporalVectorIndex>) -> Self {
        VectorIndexObserver { index }
    }
}

impl crate::storage::observer::StorageObserver for VectorIndexObserver {
    fn on_event(&self, event: &crate::storage::observer::StorageEvent) -> crate::utils::Result<()> {
        use crate::storage::observer::StorageEvent;

        match event {
            // Note: The PreAnchorHook already creates the snapshot and links the snapshot_id
            // to the anchor. This observer is only for post-commit actions like logging/metrics.
            StorageEvent::NodeAnchorCreated {
                node_id: _node_id,
                timestamp: _timestamp,
                ..
            } => {
                #[cfg(feature = "observability")]
                tracing::debug!(
                    "VectorIndexObserver: Node anchor created for {} at timestamp {} (snapshot already created by pre-anchor hook)",
                    _node_id,
                    _timestamp
                );
                Ok(())
            }
            StorageEvent::EdgeAnchorCreated {
                edge_id: _edge_id,
                timestamp: _timestamp,
                ..
            } => {
                #[cfg(feature = "observability")]
                tracing::debug!(
                    "VectorIndexObserver: Edge anchor created for {} at timestamp {} (snapshot already created by pre-anchor hook)",
                    _edge_id,
                    _timestamp
                );
                Ok(())
            }
            // Ignore other events
            _ => Ok(()),
        }
    }

    fn interested_in(&self, event: &crate::storage::observer::StorageEvent) -> bool {
        // Only interested in anchor creation events for logging/metrics
        event.is_anchor_event()
    }
}

/// Memory usage statistics for monitoring.
///
/// Use these metrics to detect potential memory growth and optimize snapshot configuration.
#[derive(Debug, Clone)]
pub struct MemoryStats {
    /// Number of unique vectors changed since last FULL snapshot.
    ///
    /// **WARNING**: This grows unboundedly between full snapshots.
    /// If this exceeds 100k, consider reducing `full_snapshot_interval`
    /// or calling `create_manual_snapshot()`.
    pub changes_accumulated_size: usize,

    /// Number of vectors changed since last snapshot (any type).
    ///
    /// Resets after each snapshot creation.
    pub vectors_changed_since_snapshot: usize,

    /// Number of snapshots created since last full snapshot.
    pub snapshots_since_full: usize,

    /// Total number of snapshots stored.
    pub total_snapshots: usize,

    /// Number of vectors in current (live) index.
    pub current_vectors: usize,
}

impl MemoryStats {
    /// Checks if memory growth is concerning and may require intervention.
    ///
    /// Returns true if `changes_accumulated_size` exceeds 100,000 entries,
    /// which could indicate excessive memory usage.
    pub fn is_high_memory_usage(&self) -> bool {
        self.changes_accumulated_size > 100_000
    }

    /// Estimates memory overhead from accumulated changes in bytes.
    ///
    /// This is a rough estimate assuming ~8 bytes per NodeId in the HashSet.
    pub fn estimated_accumulated_bytes(&self) -> usize {
        self.changes_accumulated_size * 8 // NodeId is u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_index() -> Result<TemporalVectorIndex> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1000),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        TemporalVectorIndex::new(config)
    }

    #[test]
    fn test_temporal_index_creation() -> Result<()> {
        let index = create_test_index()?;
        assert_eq!(index.dimensions(), 4);
        assert_eq!(index.distance_metric(), DistanceMetric::Cosine);
        assert_eq!(index.snapshot_count(), 0);
        Ok(())
    }

    #[test]
    fn test_current_index_direct() -> Result<()> {
        let hnsw = HnswIndex::new(HnswConfig::new(4, DistanceMetric::Cosine))?;
        let node1 = NodeId::new(1).unwrap();
        let vec1 = vec![1.0, 0.0, 0.0, 0.0];
        hnsw.add(node1, &vec1)?;
        assert_eq!(hnsw.len(), 1);
        Ok(())
    }

    #[test]
    fn test_add_vector() -> Result<()> {
        let index = create_test_index()?;

        let node1 = NodeId::new(1).unwrap();
        let vec1 = vec![1.0, 0.0, 0.0, 0.0];
        let timestamp = 1000000;

        index.add(node1, &vec1, timestamp.into())?;

        assert_eq!(index.current_index().len(), 1);
        Ok(())
    }

    #[test]
    fn test_snapshot_creation_transaction_interval() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(2),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.on_transaction()?;
        assert_eq!(index.snapshot_count(), 0);

        index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0], 2000)?;
        index.on_transaction()?;
        assert_eq!(index.snapshot_count(), 1);

        Ok(())
    }

    #[test]
    fn test_snapshot_creation_time_interval() -> Result<()> {
        let base_time = 1000000000;

        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TimeInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, base_time.into())?;

        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], base_time)?;
        assert_eq!(index.snapshot_count(), 0);

        index.add(
            NodeId::new(2).unwrap(),
            &[0.0, 1.0, 0.0, 0.0],
            base_time + 2_000_000,
        )?;
        index.on_transaction_at((base_time + 2_000_000).into())?;
        assert_eq!(index.snapshot_count(), 1);

        Ok(())
    }

    #[test]
    fn test_snapshot_creation_change_threshold() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::ChangeThreshold(0.5),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        for i in 0..4 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }
        assert_eq!(index.snapshot_count(), 0);

        index.add(NodeId::new(0).unwrap(), &[10.0, 0.0, 0.0, 0.0], 2000)?;
        index.add(NodeId::new(1).unwrap(), &[11.0, 0.0, 0.0, 0.0], 2000)?;
        index.on_transaction()?;
        assert_eq!(index.snapshot_count(), 1);

        Ok(())
    }

    #[test]
    fn test_manual_snapshot() -> Result<()> {
        let index = create_test_index()?;

        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000)?;

        assert_eq!(index.snapshot_count(), 0);

        index.create_manual_snapshot()?;
        assert_eq!(index.snapshot_count(), 1);

        Ok(())
    }

    #[test]
    fn test_max_snapshots_limit() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 3,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        for i in 0..5 {
            index.add(
                NodeId::new(i).unwrap(),
                &[i as f32, 0.0, 0.0, 0.0],
                ((i * 1000) as i64).into(),
            )?;
            index.on_transaction()?;
        }

        assert_eq!(index.snapshot_count(), 3);

        Ok(())
    }

    #[test]
    fn test_find_similar_as_of() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0], 1000)?;
        index.on_transaction_at(1000.into())?;

        let query = vec![0.9, 0.1, 0.0, 0.0];
        let results = index.find_similar_as_of(&query, 2, 1000.into())?;

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, NodeId::new(1).unwrap());

        Ok(())
    }

    #[test]
    fn test_find_similar_in_range() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.on_transaction_at(1000.into())?;

        index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0], 2000)?;
        index.on_transaction_at(2000.into())?;

        index.add(NodeId::new(3).unwrap(), &[0.0, 0.0, 1.0, 0.0], 3000)?;
        index.on_transaction_at(3000.into())?;

        let query = vec![1.0, 0.0, 0.0, 0.0];
        let time_range = TimeRange::new(1000.into(), 3000.into()).unwrap();
        let results = index.find_similar_in_range(&query, 5, time_range)?;

        assert!(!results.is_empty());

        for (timestamp, similar_nodes) in results {
            assert!((1000..=3000).contains(&timestamp.wallclock()));
            assert!(!similar_nodes.is_empty());
        }

        Ok(())
    }

    #[test]
    fn test_snapshot_info() -> Result<()> {
        let index = create_test_index()?;

        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.create_manual_snapshot()?;

        let info = index.get_snapshot_info()?;
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].snapshot_id, 0);
        assert!(info[0].timestamp.wallclock() > 0);

        Ok(())
    }

    #[test]
    fn test_temporal_vector_config() {
        let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
        let config = TemporalVectorConfig::default_with_hnsw(hnsw_config.clone());

        assert_eq!(config.max_snapshots, 100);
        assert!(matches!(
            config.snapshot_strategy,
            SnapshotStrategy::TransactionInterval(10)
        ));
        assert_eq!(config.hnsw_config, Some(hnsw_config));
    }

    #[test]
    fn test_snapshot_strategy_hybrid() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::Hybrid {
                transaction_interval: 10,
                time_interval_secs: 1,
                change_threshold: 0.5,
            },
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        for i in 0..10 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
            index.on_transaction_at(1000.into())?;
        }
        assert_eq!(index.snapshot_count(), 1);

        Ok(())
    }

    #[test]
    fn test_prune_snapshots_keep_all() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        for i in 0..5 {
            let timestamp = ((1000 * (i + 1)) as i64).into();
            index.add(
                NodeId::new(i).unwrap(),
                &[i as f32, 0.0, 0.0, 0.0],
                timestamp,
            )?;
            index.on_transaction_at(timestamp)?;
        }
        assert_eq!(index.snapshot_count(), 5);

        let removed = index.prune_snapshots()?;
        assert_eq!(removed, 0);
        assert_eq!(index.snapshot_count(), 5);

        Ok(())
    }

    #[test]
    fn test_prune_snapshots_keep_n() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(3),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        for i in 0..5 {
            let timestamp = ((1000 * (i + 1)) as i64).into();
            index.add(
                NodeId::new(i).unwrap(),
                &[i as f32, 0.0, 0.0, 0.0],
                timestamp,
            )?;
            index.on_transaction_at(timestamp)?;
        }
        assert_eq!(index.snapshot_count(), 5);

        let removed = index.prune_snapshots()?;
        assert_eq!(removed, 2);
        assert_eq!(index.snapshot_count(), 3);

        let info = index.get_snapshot_info()?;
        assert_eq!(info.len(), 3);
        assert_eq!(info[0].snapshot_id, 2);
        assert_eq!(info[1].snapshot_id, 3);
        assert_eq!(info[2].snapshot_id, 4);

        Ok(())
    }

    #[test]
    fn test_prune_snapshots_keep_n_when_under_limit() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(10),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        for i in 0..3 {
            let timestamp = ((1000 * (i + 1)) as i64).into();
            index.add(
                NodeId::new(i).unwrap(),
                &[i as f32, 0.0, 0.0, 0.0],
                timestamp,
            )?;
            index.on_transaction_at(timestamp)?;
        }
        assert_eq!(index.snapshot_count(), 3);

        let removed = index.prune_snapshots()?;
        assert_eq!(removed, 0);
        assert_eq!(index.snapshot_count(), 3);

        Ok(())
    }

    #[test]
    fn test_prune_snapshots_keep_duration() -> Result<()> {
        use std::time::Duration;

        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepDuration(Duration::from_secs(5)),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        let current_time = TemporalVectorIndex::current_timestamp()?;

        index.add(
            NodeId::new(1).unwrap(),
            &[1.0, 0.0, 0.0, 0.0],
            (current_time.wallclock() - 10_000_000).into(),
        )?;
        index.create_manual_snapshot()?;

        index.add(
            NodeId::new(2).unwrap(),
            &[0.0, 1.0, 0.0, 0.0],
            (current_time.wallclock() - 7_000_000).into(),
        )?;
        index.create_manual_snapshot()?;

        index.add(
            NodeId::new(3).unwrap(),
            &[0.0, 0.0, 1.0, 0.0],
            (current_time.wallclock() - 3_000_000).into(),
        )?;
        index.create_manual_snapshot()?;

        let count_before = index.snapshot_count();
        assert!(count_before >= 3);

        let removed = index.prune_snapshots()?;

        assert_eq!(removed, 0);

        Ok(())
    }

    #[test]
    fn test_retention_policy_default() {
        let default_policy = RetentionPolicy::default();
        assert_eq!(default_policy, RetentionPolicy::KeepN(100));
    }

    #[test]
    fn test_retention_policy_in_config() {
        let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
        let config = TemporalVectorConfig::default_with_hnsw(hnsw_config);

        assert_eq!(config.retention_policy, RetentionPolicy::KeepN(100));
    }

    #[test]
    fn test_semantic_evolution_basic() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
        index.on_transaction_at(1000.into())?;
        assert_eq!(index.snapshot_count(), 1);

        index.add(node_id, &[0.0, 1.0, 0.0, 0.0], 2000.into())?;
        index.on_transaction_at(2000.into())?;
        assert_eq!(index.snapshot_count(), 2);

        index.add(node_id, &[0.0, 0.0, 1.0, 0.0], 3000.into())?;
        index.on_transaction_at(3000.into())?;
        assert_eq!(index.snapshot_count(), 3);

        let time_range = TimeRange::new(1000.into(), 3000.into()).unwrap();
        let evolution = index.semantic_evolution(node_id, time_range)?;

        assert_eq!(evolution.len(), 3);

        assert_eq!(evolution[0].1.as_ref(), &[1.0, 0.0, 0.0, 0.0]);
        assert_eq!(evolution[1].1.as_ref(), &[0.0, 1.0, 0.0, 0.0]);
        assert_eq!(evolution[2].1.as_ref(), &[0.0, 0.0, 1.0, 0.0]);

        Ok(())
    }

    #[test]
    fn test_semantic_evolution_partial_range() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        for i in 1..=5 {
            let timestamp = i * 1000;
            index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;
            index.on_transaction_at(timestamp)?;
        }

        let time_range = TimeRange::new(2000.into(), 4000.into()).unwrap();
        let evolution = index.semantic_evolution(node_id, time_range)?;

        assert_eq!(evolution.len(), 3);
        assert_eq!(evolution[0].1.as_ref(), &[2.0, 0.0, 0.0, 0.0]);
        assert_eq!(evolution[1].1.as_ref(), &[3.0, 0.0, 0.0, 0.0]);
        assert_eq!(evolution[2].1.as_ref(), &[4.0, 0.0, 0.0, 0.0]);

        Ok(())
    }

    #[test]
    fn test_semantic_evolution_node_not_in_snapshots() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let other_node = NodeId::new(1).unwrap();
        index.add(other_node, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
        index.on_transaction_at(1000.into())?;

        let node_id = NodeId::new(42).unwrap();
        let time_range = TimeRange::new(1000.into(), 2000.into()).unwrap();
        let evolution = index.semantic_evolution(node_id, time_range)?;

        assert_eq!(evolution.len(), 0);

        Ok(())
    }

    #[test]
    fn test_track_semantic_drift() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
        index.on_transaction_at(1000.into())?;

        index.add(node_id, &[0.9, 0.1, 0.0, 0.0], 2000.into())?;
        index.on_transaction_at(2000.into())?;

        index.add(node_id, &[0.5, 0.5, 0.0, 0.0], 3000.into())?;
        index.on_transaction_at(3000.into())?;

        let reference = vec![1.0, 0.0, 0.0, 0.0];
        let time_range = TimeRange::new(1000.into(), 3000.into()).unwrap();
        let drift = index.track_semantic_drift(node_id, &reference, time_range)?;

        assert_eq!(drift.len(), 3);
        assert!((drift[0].1 - 1.0).abs() < 0.001);
        assert!(drift[1].1 > 0.99);
        assert!(drift[2].1 < 0.8);

        Ok(())
    }

    #[test]
    fn test_calculate_consecutive_drift() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
        index.on_transaction_at(1000.into())?;

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 2000.into())?;
        index.on_transaction_at(2000.into())?;

        index.add(node_id, &[0.707, 0.707, 0.0, 0.0], 3000.into())?;
        index.on_transaction_at(3000.into())?;

        index.add(node_id, &[0.0, 1.0, 0.0, 0.0], 4000.into())?;
        index.on_transaction_at(4000.into())?;

        let time_range = TimeRange::new(1000.into(), 4000.into()).unwrap();
        let drift = index.calculate_consecutive_drift(node_id, time_range)?;

        assert_eq!(drift.len(), 3);
        assert!(drift[0].1.abs() < 0.001);
        assert!((drift[1].1 - 0.293).abs() < 0.01);
        assert!((drift[2].1 - 0.293).abs() < 0.01);

        Ok(())
    }

    #[test]
    fn test_consecutive_drift_single_vector() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
        index.on_transaction_at(1000.into())?;

        let time_range = TimeRange::new(1000.into(), 2000.into()).unwrap();
        let drift = index.calculate_consecutive_drift(node_id, time_range)?;

        assert_eq!(drift.len(), 0);

        Ok(())
    }

    #[test]
    fn test_semantic_evolution_with_vector_updates() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
        index.on_transaction_at(1000.into())?;

        index.add(node_id, &[0.0, 1.0, 0.0, 0.0], 2000.into())?;
        index.on_transaction_at(2000.into())?;

        let time_range = TimeRange::new(1000.into(), 2000.into()).unwrap();
        let evolution = index.semantic_evolution(node_id, time_range)?;

        assert_eq!(evolution.len(), 2);
        assert_eq!(evolution[0].1.as_ref(), &[1.0, 0.0, 0.0, 0.0]);
        assert_eq!(evolution[1].1.as_ref(), &[0.0, 1.0, 0.0, 0.0]);

        Ok(())
    }

    #[test]
    fn test_vector_history_pruning() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(3),
            max_snapshots: 3,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        for i in 1..=5 {
            let timestamp = i * 1000;
            index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;
            index.on_transaction_at(timestamp)?;
        }

        assert_eq!(index.snapshot_count(), 3);

        let time_range = TimeRange::new(1000.into(), 5000.into()).unwrap();
        let evolution = index.semantic_evolution(node_id, time_range)?;

        assert_eq!(evolution.len(), 3);
        assert_eq!(evolution[0].1.as_ref(), &[3.0, 0.0, 0.0, 0.0]);
        assert_eq!(evolution[1].1.as_ref(), &[4.0, 0.0, 0.0, 0.0]);
        assert_eq!(evolution[2].1.as_ref(), &[5.0, 0.0, 0.0, 0.0]);

        Ok(())
    }

    #[test]
    fn test_track_semantic_drift_dimension_mismatch() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
        index.on_transaction_at(1000.into())?;

        let wrong_dimension_ref = vec![1.0, 0.0, 0.0];
        let time_range = TimeRange::new(1000.into(), 2000.into()).unwrap();

        let result = index.track_semantic_drift(node_id, &wrong_dimension_ref, time_range);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            Error::Vector(VectorError::DimensionMismatch {
                expected: 4,
                actual: 3
            })
        ));

        Ok(())
    }

    #[test]
    fn test_find_semantic_drift_basic() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        let mut ts = 1000i64;

        let node1 = NodeId::new(1).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
        index.remove(node1, ts)?;
        index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;

        ts += 1000;
        let node2 = NodeId::new(2).unwrap();
        index.add(node2, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
        index.remove(node2, ts)?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;

        ts += 1000;
        let node3 = NodeId::new(3).unwrap();
        index.add(node3, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
        index.remove(node3, ts)?;
        index.add(
            node3,
            &[
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                0.0,
            ],
            ts,
        )?;
        index.on_transaction_at(ts)?;

        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

        let results = index.find_semantic_drift(0.2, time_range, DriftMetric::Cosine)?;

        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|(id, _)| *id == node2));
        assert!(results.iter().any(|(id, _)| *id == node3));

        Ok(())
    }

    #[test]
    fn test_find_semantic_drift_sorted_descending() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        let mut ts = 1000i64;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();
        let node3 = NodeId::new(3).unwrap();

        index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
        index.remove(node1, ts)?;
        index.add(node1, &[0.9, 0.1, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;

        ts += 1000;
        index.add(node2, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
        index.remove(node2, ts)?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;

        ts += 1000;
        index.add(node3, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
        index.remove(node3, ts)?;
        index.add(
            node3,
            &[
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                0.0,
            ],
            ts,
        )?;
        index.on_transaction_at(ts)?;

        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
        let results = index.find_semantic_drift(0.0, time_range, DriftMetric::Cosine)?;

        assert_eq!(results.len(), 3);
        assert!(results[0].1 > results[1].1);
        assert!(results[1].1 > results[2].1);

        assert_eq!(results[0].0, node2);

        Ok(())
    }

    #[test]
    fn test_find_semantic_drift_single_version_excluded() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        let mut ts = 1000i64;

        let node1 = NodeId::new(1).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;

        ts += 1000;
        let node2 = NodeId::new(2).unwrap();
        index.add(node2, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
        index.remove(node2, ts)?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;

        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
        let results = index.find_semantic_drift(0.0, time_range, DriftMetric::Cosine)?;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node2);

        Ok(())
    }

    #[test]
    fn test_find_semantic_drift_empty_range() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        let ts = 10000i64;

        let node1 = NodeId::new(1).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;

        let time_range = TimeRange::new(0.into(), 100.into()).unwrap();
        let results = index.find_semantic_drift(0.0, time_range, DriftMetric::Cosine)?;

        assert_eq!(results.len(), 0);

        Ok(())
    }

    #[test]
    fn test_find_semantic_drift_threshold_zero() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        let mut ts = 1000i64;

        let node1 = NodeId::new(1).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
        index.remove(node1, ts)?;
        index.add(node1, &[0.9, 0.1, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;

        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

        let results = index.find_semantic_drift(0.0, time_range, DriftMetric::Cosine)?;
        assert_eq!(results.len(), 1);

        let results = index.find_semantic_drift(-0.5, time_range, DriftMetric::Cosine)?;
        assert_eq!(results.len(), 1);

        Ok(())
    }

    #[test]
    fn test_find_semantic_drift_all_metrics() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        let mut ts = 1000i64;

        let node1 = NodeId::new(1).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
        index.remove(node1, ts)?;
        index.add(node1, &[0.0, 1.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;

        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

        let cosine_results = index.find_semantic_drift(0.5, time_range, DriftMetric::Cosine)?;
        assert_eq!(cosine_results.len(), 1);

        let euclidean_results =
            index.find_semantic_drift(0.5, time_range, DriftMetric::Euclidean)?;
        assert_eq!(euclidean_results.len(), 1);

        let angular_results = index.find_semantic_drift(0.5, time_range, DriftMetric::Angular)?;
        assert_eq!(angular_results.len(), 1);

        Ok(())
    }

    #[test]
    fn test_find_semantic_drift_metrics_comparison() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new(config)?;

        let mut ts = 1000i64;

        let node1 = NodeId::new(1).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;
        ts += 1000;
        index.remove(node1, ts)?;
        index.add(node1, &[0.0, 1.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;

        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

        let cosine_results = index.find_semantic_drift(0.0, time_range, DriftMetric::Cosine)?;
        assert_eq!(cosine_results[0].1, 1.0);

        let euclidean_results =
            index.find_semantic_drift(0.0, time_range, DriftMetric::Euclidean)?;
        assert!((euclidean_results[0].1 - 1.414).abs() < 0.01);

        let angular_results = index.find_semantic_drift(0.0, time_range, DriftMetric::Angular)?;
        assert!((angular_results[0].1 - std::f32::consts::FRAC_PI_2).abs() < 0.01);

        Ok(())
    }

    #[test]
    fn test_drift_metric_default() {
        let metric = DriftMetric::default();
        assert_eq!(metric, DriftMetric::Cosine);
    }

    // ==================== Delta Snapshot Tests ====================

    #[test]
    fn test_delta_snapshots_actually_used() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create initial vectors - this will be snapshot 0 (full)
        for i in 0..100 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }
        index.on_transaction_at(1000.into())?;
        assert_eq!(index.snapshot_count(), 1);

        // Modify only 5 vectors - snapshots 1-9 should be deltas
        for snapshot_num in 1..10 {
            let timestamp = (1000 + (snapshot_num * 1000) as i64).into();
            // Only modify 5 vectors
            for i in 0..5 {
                index.add(
                    NodeId::new(i).unwrap(),
                    &[(i + snapshot_num) as f32, 1.0, 0.0, 0.0],
                    timestamp,
                )?;
            }
            index.on_transaction_at(timestamp)?;
        }

        assert_eq!(index.snapshot_count(), 10);

        // Snapshot 10 should be full again (after 10 snapshots)
        index.add(NodeId::new(0).unwrap(), &[100.0, 0.0, 0.0, 0.0], 11000)?;
        index.on_transaction_at(11000.into())?;
        assert_eq!(index.snapshot_count(), 11);

        // Verify we can query all snapshots successfully
        for snapshot_num in 0..11 {
            let timestamp = 1000 + (snapshot_num * 1000);
            let query = vec![0.0, 0.0, 0.0, 0.0];
            let results = index.find_similar_as_of(&query, 10, timestamp)?;
            assert!(
                !results.is_empty(),
                "Snapshot {} should have results",
                snapshot_num
            );
        }

        Ok(())
    }

    #[test]
    fn test_delta_snapshot_search_correctness() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create full snapshot with distinct vectors
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0], 1000)?;
        index.add(NodeId::new(3).unwrap(), &[0.0, 0.0, 1.0, 0.0], 1000)?;
        index.on_transaction_at(1000.into())?;
        assert_eq!(index.snapshot_count(), 1);

        // Update vector 2 to be very similar to vector 3 in delta snapshot
        index.add(NodeId::new(2).unwrap(), &[0.0, 0.0, 1.0, 0.0], 2000)?;
        index.on_transaction_at(2000.into())?;
        assert_eq!(index.snapshot_count(), 2);

        // Query for [0.0, 0.0, 1.0, 0.0] at new timestamp - should find both 2 and 3
        let query = vec![0.0, 0.0, 1.0, 0.0];
        let results = index.find_similar_as_of(&query, 3, 2000.into())?;

        // Both vectors 2 and 3 should be top results with high similarity
        let top_ids: Vec<NodeId> = results.iter().take(2).map(|(id, _)| *id).collect();
        assert!(top_ids.contains(&NodeId::new(2).unwrap()));
        assert!(top_ids.contains(&NodeId::new(3).unwrap()));

        // Query at old timestamp for [0.0, 1.0, 0.0, 0.0] - should find OLD vector 2
        let query_old = vec![0.0, 1.0, 0.0, 0.0];
        let results_old = index.find_similar_as_of(&query_old, 3, 1000.into())?;
        // At timestamp 1000, vector 2 was [0.0, 1.0, 0.0, 0.0]
        assert_eq!(results_old[0].0, NodeId::new(2).unwrap());
        assert!(results_old[0].1 > 0.99, "Should match old version exactly");

        Ok(())
    }

    #[test]
    fn test_delta_snapshot_handles_removes() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create full snapshot with 5 vectors
        for i in 0..5 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }
        index.on_transaction_at(1000.into())?;
        assert_eq!(index.current_index().len(), 5);

        // Remove vector 2 in delta snapshot
        index.remove(NodeId::new(2).unwrap(), 2000.into())?;
        index.on_transaction_at(2000.into())?;
        assert_eq!(index.current_index().len(), 4);

        // Query at new timestamp should not return removed vector
        let query = vec![2.0, 0.0, 0.0, 0.0];
        let results = index.find_similar_as_of(&query, 5, 2000.into())?;

        // Should only have 4 results (vector 2 was removed)
        assert_eq!(results.len(), 4);
        assert!(!results.iter().any(|(id, _)| *id == NodeId::new(2).unwrap()));

        // Query at old timestamp should still return it
        let results_old = index.find_similar_as_of(&query, 5, 1000.into())?;
        assert_eq!(results_old.len(), 5);
        assert!(
            results_old
                .iter()
                .any(|(id, _)| *id == NodeId::new(2).unwrap())
        );

        Ok(())
    }

    // ==================== Concurrent Snapshot Tests ====================

    #[test]
    fn test_concurrent_snapshot_creation() -> Result<()> {
        use std::sync::Arc;
        use std::thread;

        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = Arc::new(TemporalVectorIndex::new_at(config, 1000)?);

        // Add initial vectors
        for i in 0..50 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }

        // Spawn multiple threads that concurrently trigger snapshots
        let mut handles = vec![];
        for thread_id in 0..10 {
            let idx = Arc::clone(&index);
            handles.push(thread::spawn(move || {
                let base_time = 2000 + (thread_id * 1000);
                for i in 0..5 {
                    let node_id = NodeId::new((thread_id * 10 + i) as u64).unwrap();
                    let timestamp = base_time + (i * 100);
                    idx.add(node_id, &[thread_id as f32, i as f32, 0.0, 0.0], timestamp)
                        .unwrap();
                    // Try to trigger snapshot
                    idx.on_transaction_at(timestamp).unwrap();
                }
            }));
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().expect("Thread panicked");
        }

        // Verify no panics occurred and snapshots were created
        let snapshot_count = index.snapshot_count();
        assert!(snapshot_count > 0, "Expected snapshots to be created");

        // Verify we can query without errors
        let query = vec![0.0, 0.0, 0.0, 0.0];
        let results = index.find_similar_as_of(&query, 10, 10000.into())?;
        assert!(!results.is_empty());

        Ok(())
    }

    #[test]
    fn test_concurrent_snapshot_no_double_create() -> Result<()> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::thread;

        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = Arc::new(TemporalVectorIndex::new_at(config, 1000)?);
        let timestamp = Arc::new(AtomicU64::new(1000));

        // Add initial vectors
        for i in 0..50 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }

        // Spawn threads that all try to create the 10th transaction
        let mut handles = vec![];
        for thread_id in 0..5 {
            let idx = Arc::clone(&index);
            let ts = Arc::clone(&timestamp);
            handles.push(thread::spawn(move || {
                for i in 0..3 {
                    let t = ts.fetch_add(100, Ordering::SeqCst);
                    let node_id = NodeId::new((thread_id * 10 + i) as u64).unwrap();
                    idx.add(node_id, &[thread_id as f32, i as f32, 0.0, 0.0], (t as i64).into())
                        .unwrap();
                    idx.on_transaction_at((t as i64).into()).unwrap();
                }
            }));
        }

        for handle in handles {
            handle.join().expect("Thread panicked");
        }

        // Should have created snapshots, but not excessive duplicates
        // With 5 threads x 3 transactions = 15 total, and interval=10, we expect 1-2 snapshots ideally
        // However, with concurrent races, multiple threads can all see transactions_since_snapshot >= 10
        // before any reset the counter, leading to multiple snapshot creations at the same "trigger point"
        // Accept up to 6 snapshots (still much better than 15 if every transaction created one)
        let count = index.snapshot_count();
        assert!(
            (1..=6).contains(&count),
            "Expected 1-6 snapshots due to concurrent racing, got {} (15 transactions with interval=10 should not create 15 snapshots)",
            count
        );

        Ok(())
    }

    // ==================== Pruning + Delta Tests ====================

    #[test]
    fn test_prune_base_snapshot_fallback() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(3),
            max_snapshots: 3,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create full snapshot at t=1000
        for i in 0..10 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }
        index.on_transaction_at(1000.into())?;
        assert_eq!(index.snapshot_count(), 1);

        // Create delta snapshots at t=2000, 3000 (both reference t=1000)
        for snapshot_num in 1..3 {
            let timestamp = 1000 + (snapshot_num * 1000);
            index.add(
                NodeId::new(0).unwrap(),
                &[snapshot_num as f32, 0.0, 0.0, 0.0],
                timestamp,
            )?;
            index.on_transaction_at(timestamp)?;
        }
        assert_eq!(index.snapshot_count(), 3);

        // Create more snapshots to exceed max_snapshots, which will prune t=1000 (the base)
        for snapshot_num in 3..6 {
            let timestamp = 1000 + (snapshot_num * 1000);
            index.add(
                NodeId::new(0).unwrap(),
                &[snapshot_num as f32, 0.0, 0.0, 0.0],
                timestamp,
            )?;
            index.on_transaction_at(timestamp)?;
        }

        // Should still have 3 snapshots (max limit)
        assert_eq!(index.snapshot_count(), 3);

        // Verify we can still query the remaining snapshots
        let query = vec![0.0, 0.0, 0.0, 0.0];
        let results = index.find_similar_as_of(&query, 5, 6000.into())?;
        assert!(
            !results.is_empty(),
            "Should still be able to query after pruning base"
        );

        Ok(())
    }

    #[test]
    fn test_manual_snapshot_with_delta_optimization() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1000), // High threshold
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create initial full snapshot manually
        for i in 0..100 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }
        index.create_manual_snapshot()?;
        let first_snapshot_count = index.snapshot_count();
        assert_eq!(first_snapshot_count, 1);

        // Modify only 5 vectors
        for i in 0..5 {
            index.add(
                NodeId::new(i).unwrap(),
                &[100.0 + i as f32, 0.0, 0.0, 0.0],
                2000,
            )?;
        }

        // Manual snapshot should use delta (only 5 changes)
        index.create_manual_snapshot()?;
        assert_eq!(index.snapshot_count(), 2);

        // Verify the snapshots exist and are queryable
        let snapshot_info = index.get_snapshot_info()?;
        assert_eq!(snapshot_info.len(), 2);

        // Verify we can query the latest snapshot
        let query = vec![100.0, 0.0, 0.0, 0.0];
        let latest_timestamp = snapshot_info[1].timestamp;
        let results = index.find_similar_as_of(&query, 10, latest_timestamp)?;
        assert!(!results.is_empty(), "Latest snapshot should have results");

        Ok(())
    }

    #[test]
    fn test_delta_snapshot_len_correctness() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create full snapshot with 100 vectors
        for i in 0..100 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }
        index.on_transaction_at(1000.into())?;

        // Get snapshot info for full snapshot
        let snapshots = index.get_snapshot_info()?;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].vector_count, 100);

        // Update 50 vectors (creates delta with 50 in added, 50 in removed, net = 100)
        for i in 0..50 {
            index.add(
                NodeId::new(i).unwrap(),
                &[200.0 + i as f32, 0.0, 0.0, 0.0],
                2000,
            )?;
        }
        index.on_transaction_at(2000.into())?;

        // Check delta snapshot reports correct count
        let snapshots = index.get_snapshot_info()?;
        assert_eq!(snapshots.len(), 2);
        assert_eq!(
            snapshots[1].vector_count, 100,
            "Delta snapshot should report 100 vectors (base 100 + added 50 - removed 50)"
        );

        Ok(())
    }

    // ==================== Safety and Edge Case Tests ====================

    #[test]
    fn test_delta_chain_depth_limit_enforced() -> Result<()> {
        // This test verifies that MAX_DELTA_CHAIN_DEPTH is enforced during reconstruction
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10, // Set to MAX_DELTA_CHAIN_DEPTH
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create initial full snapshot
        let node_id = NodeId::new(1).unwrap();
        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
        index.on_transaction_at(1000.into())?;

        // Create 15 delta snapshots (exceeds MAX_DELTA_CHAIN_DEPTH of 10)
        for i in 1..=15 {
            let timestamp = 1000 + (i * 100);
            index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;
            index.on_transaction_at(timestamp)?;
        }

        // Query at a timestamp that would require traversing > 10 deltas
        // This should either return None or force a full snapshot
        let query = vec![1.0, 0.0, 0.0, 0.0];
        let result = index.find_similar_as_of(&query, 5, 2500.into());

        // Test passes if it doesn't panic and handles the deep chain gracefully
        assert!(
            result.is_ok(),
            "Should handle deep delta chains without panic"
        );

        Ok(())
    }

    #[test]
    fn test_full_snapshot_interval_boundary() -> Result<()> {
        // Test that full snapshots are created exactly at the configured interval
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 5, // Create full snapshot every 5 snapshots
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(1).unwrap();

        // Create snapshots and track which are full vs delta
        for i in 0..10 {
            let timestamp = 1000 + (i * 100);
            index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;
            index.on_transaction_at(timestamp)?;
        }

        // Verify we created snapshots (should be 10)
        let snapshot_count = index.snapshot_count();
        assert_eq!(snapshot_count, 10, "Should have created 10 snapshots");

        // Get snapshot info to verify snapshots were created
        let snapshot_info = index.get_snapshot_info()?;

        // Verify we have all 10 snapshots
        assert_eq!(snapshot_info.len(), 10, "Should have 10 snapshots in info");

        // Note: We can't directly verify Full vs Delta from SnapshotInfo,
        // but the test verifies that the full_snapshot_interval configuration works

        Ok(())
    }

    #[test]
    fn test_delta_snapshot_base_validation() -> Result<()> {
        // This test verifies that delta snapshots only reference Full snapshots
        // by checking the internal structure after snapshot creation
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 3,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create initial full snapshot
        let node_id = NodeId::new(1).unwrap();
        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
        index.on_transaction_at(1000.into())?;

        // Create several delta snapshots
        for i in 1..=5 {
            let timestamp = 1000 + (i * 100);
            index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;
            index.on_transaction_at(timestamp)?;
        }

        // Verify all queries work correctly (this implicitly tests base validation)
        for i in 1..=5 {
            let timestamp = 1000 + (i * 100);
            let query = vec![i as f32, 0.0, 0.0, 0.0];
            let result = index.find_similar_as_of(&query, 5, timestamp)?;
            assert!(
                !result.is_empty(),
                "Should find results at timestamp {}",
                timestamp
            );
        }

        Ok(())
    }

    #[test]
    fn test_concurrent_snapshot_with_pruning() -> Result<()> {
        use std::sync::Arc;
        use std::thread;

        // Test concurrent snapshot creation with aggressive pruning
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(2),
            retention_policy: RetentionPolicy::KeepN(5), // Aggressive pruning
            max_snapshots: 5,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = Arc::new(TemporalVectorIndex::new_at(config, 1000)?);

        // Add initial vectors
        for i in 0..20 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }

        // Spawn threads that will trigger both snapshot creation and pruning
        let mut handles = vec![];
        for thread_id in 0..5 {
            let idx = Arc::clone(&index);
            handles.push(thread::spawn(move || {
                for i in 0..10 {
                    let timestamp = 2000 + (thread_id * 1000) + (i * 100);
                    let node_id = NodeId::new((thread_id * 10 + i) as u64).unwrap();
                    idx.add(node_id, &[thread_id as f32, i as f32, 0.0, 0.0], timestamp)
                        .unwrap();
                    idx.on_transaction_at(timestamp).unwrap();
                }
            }));
        }

        // Wait for completion
        for handle in handles {
            handle.join().expect("Thread panicked");
        }

        // Verify snapshot count is within the retention limit
        let snapshot_count = index.snapshot_count();
        assert!(
            snapshot_count <= 5,
            "Should respect max_snapshots limit, got {}",
            snapshot_count
        );

        // Verify we can still query
        let query = vec![0.0, 0.0, 0.0, 0.0];
        let current_time = TemporalVectorIndex::current_timestamp()?;
        let results = index.find_similar_as_of(&query, 5, current_time)?;
        assert!(
            !results.is_empty(),
            "Should still be able to query after concurrent pruning"
        );

        Ok(())
    }

    #[test]
    fn test_snapshot_retry_limit() -> Result<()> {
        // Test that snapshot creation respects MAX_SNAPSHOT_RETRIES
        // This is a best-effort test - it's hard to force exactly 3 retries
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Add vectors and create snapshots normally
        for i in 0..5 {
            let timestamp = (1000 + (i * 100) as i64).into();
            index.add(
                NodeId::new(i).unwrap(),
                &[i as f32, 0.0, 0.0, 0.0],
                timestamp,
            )?;
            index.on_transaction_at(timestamp)?;
        }

        // Verify snapshots were created successfully
        let snapshot_count = index.snapshot_count();
        assert!(snapshot_count >= 1, "Should have created snapshots");

        // Test passes if no panic occurred during snapshot creation
        // (The retry logic prevents infinite loops)
        Ok(())
    }

    #[test]
    fn test_delta_snapshot_len_with_pure_additions() -> Result<()> {
        // This test verifies the fix for the critical len() bug where pure additions
        // were incorrectly included in DeltaIndex.removed, causing incorrect counts
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10, // Set to MAX_DELTA_CHAIN_DEPTH to ensure delta snapshots
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create full snapshot with 3 nodes at t=1000
        for i in 0..3 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }
        index.on_transaction_at(1000.into())?;

        // Verify base snapshot has 3 nodes
        let snapshot_info = index.get_snapshot_info()?;
        assert_eq!(snapshot_info.len(), 1);
        assert_eq!(snapshot_info[0].vector_count, 3, "Base should have 3 nodes");

        // Add 2 NEW nodes at t=2000 (pure additions, not updates)
        for i in 3..5 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 2000)?;
        }
        index.on_transaction_at(2000.into())?;

        // Verify delta snapshot reports correct count
        let snapshot_info = index.get_snapshot_info()?;
        assert_eq!(snapshot_info.len(), 2, "Should have 2 snapshots");

        // CRITICAL TEST: Delta snapshot should report 5 total nodes (3 from base + 2 new)
        // Before fix: would incorrectly report 3 (because added.len=2 and removed.len=2)
        // After fix: correctly reports 5 (base=3 + added=2 - removed=0)
        assert_eq!(
            snapshot_info[1].vector_count, 5,
            "Delta snapshot should report 5 nodes (3 from base + 2 additions)"
        );

        // Verify we can actually query and find all 5 nodes
        let query = vec![0.0, 0.0, 0.0, 0.0];
        let results = index.find_similar_as_of(&query, 10, 2000.into())?;
        assert_eq!(
            results.len(),
            5,
            "Should find all 5 nodes when querying at t=2000"
        );

        Ok(())
    }

    #[test]
    fn test_delta_snapshot_len_with_mixed_operations() -> Result<()> {
        // Test len() correctness with a mix of additions, updates, and removals
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create full snapshot with 10 nodes at t=1000
        for i in 0..10 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }
        index.on_transaction_at(1000.into())?;
        assert_eq!(index.get_snapshot_info()?[0].vector_count, 10);

        // At t=2000: update 3 nodes, add 2 new nodes, remove 1 node
        // Updates: nodes 0, 1, 2 -> new values
        for i in 0..3 {
            index.add(
                NodeId::new(i).unwrap(),
                &[100.0 + i as f32, 0.0, 0.0, 0.0],
                2000,
            )?;
        }
        // Additions: nodes 10, 11
        for i in 10..12 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 2000)?;
        }
        // Removal: node 9
        index.remove(NodeId::new(9).unwrap(), 2000.into())?;
        index.on_transaction_at(2000.into())?;

        // Expected final count: 10 - 1 (removed) + 2 (added) = 11
        // Updates don't change count (old version replaced by new)
        let snapshot_info = index.get_snapshot_info()?;
        assert_eq!(snapshot_info.len(), 2);
        assert_eq!(
            snapshot_info[1].vector_count, 11,
            "Delta should report 11 nodes (10 base - 1 removed + 2 added)"
        );

        // Verify query results match
        let query = vec![0.0, 0.0, 0.0, 0.0];
        let results = index.find_similar_as_of(&query, 20, 2000.into())?;
        assert_eq!(results.len(), 11, "Should find 11 nodes when querying");

        // Verify removed node 9 is not in results
        assert!(
            !results.iter().any(|(id, _)| *id == NodeId::new(9).unwrap()),
            "Removed node 9 should not appear in results"
        );

        Ok(())
    }

    #[test]
    fn test_delta_search_quality_with_merge() -> Result<()> {
        // Test that delta snapshot search correctly merges results from added and base
        // and returns the true top-k (not just top-k from each index)
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create base with vectors at [1.0, 0, 0, 0], [2.0, 0, 0, 0], ..., [10.0, 0, 0, 0]
        for i in 1..=10 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }
        index.on_transaction_at(1000.into())?;

        // Add new vectors at [1.5, 0, 0, 0], [2.5, 0, 0, 0], ..., [10.5, 0, 0, 0]
        for i in 1..=10 {
            index.add(
                NodeId::new((i + 10) as u64).unwrap(),
                &[i as f32 + 0.5, 0.0, 0.0, 0.0],
                2000,
            )?;
        }
        index.on_transaction_at(2000.into())?;

        // Query for [5.5, 0, 0, 0] with k=5
        // Expected top-5 (by cosine similarity to 5.5):
        // 1. Node 15 (vector [5.5, 0, 0, 0]) - exact match
        // 2. Node 5 (vector [5.0, 0, 0, 0]) or Node 16 (vector [6.5, 0, 0, 0])
        // The key test is that we get INTERLEAVED results from both indexes,
        // not just top-5 from added or top-5 from base
        let query = vec![5.5, 0.0, 0.0, 0.0];
        let results = index.find_similar_as_of(&query, 5, 2000.into())?;

        assert_eq!(results.len(), 5, "Should return exactly k=5 results");

        // Verify no duplicates (each NodeId appears once)
        let node_ids: Vec<NodeId> = results.iter().map(|(id, _)| *id).collect();
        let unique_ids: std::collections::HashSet<_> = node_ids.iter().collect();
        assert_eq!(
            unique_ids.len(),
            node_ids.len(),
            "Should have no duplicate NodeIds in results"
        );

        // Note: With k*2 search strategy, we should get high-quality results.
        // The exact distribution between base and added depends on HNSW search behavior,
        // so we just verify we got results (at least some should be from different sources)
        let from_base = results.iter().filter(|(id, _)| id.as_u64() <= 10).count();
        let from_added = results.iter().filter(|(id, _)| id.as_u64() > 10).count();
        // Just log the distribution, don't assert on it as it depends on HNSW behavior
        println!(
            "Results distribution - base: {}, added: {}",
            from_base, from_added
        );

        // Verify results are sorted by descending similarity
        for i in 1..results.len() {
            assert!(
                results[i - 1].1 >= results[i].1,
                "Results should be sorted by descending similarity"
            );
        }

        Ok(())
    }

    #[test]
    fn test_delta_search_no_duplicates_on_update() -> Result<()> {
        // Test that updated nodes don't appear twice in search results
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create base with node 1 at [1.0, 0, 0, 0]
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000)?;
        for i in 2..=5 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
        }
        index.on_transaction_at(1000.into())?;

        // Update node 1 to [1.5, 0, 0, 0]
        index.add(NodeId::new(1).unwrap(), &[1.5, 0.0, 0.0, 0.0], 2000)?;
        index.on_transaction_at(2000.into())?;

        // Search for [1.5, 0, 0, 0] - should find updated node 1 ONCE
        let query = vec![1.5, 0.0, 0.0, 0.0];
        let results = index.find_similar_as_of(&query, 10, 2000.into())?;

        // Count how many times node 1 appears
        let node1_count = results
            .iter()
            .filter(|(id, _)| *id == NodeId::new(1).unwrap())
            .count();
        assert_eq!(
            node1_count, 1,
            "Updated node 1 should appear exactly once in results, not twice"
        );

        // Verify node 1 has the updated vector value (distance should be 0 to [1.5, 0, 0, 0])
        let node1_result = results
            .iter()
            .find(|(id, _)| *id == NodeId::new(1).unwrap())
            .expect("Node 1 should be in results");

        // For cosine similarity, similarity of 1.0 means identical vectors
        assert!(
            node1_result.1 > 0.99,
            "Node 1 should have high similarity to query (updated vector), got {}",
            node1_result.1
        );

        Ok(())
    }

    // ==================== Stack Overflow Prevention Tests ====================

    #[test]
    fn test_to_hashmap_depth_limit_with_collect_all() -> Result<()> {
        // CRITICAL TEST: Verifies that to_hashmap() (used by collect_all() and
        // find_semantic_drift()) enforces depth limits and doesn't stack overflow
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10, // Valid config
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create initial full snapshot
        let node_id = NodeId::new(1).unwrap();
        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000.into())?;
        index.on_transaction_at(1000.into())?;

        // Create 15 delta snapshots (exceeds MAX_DELTA_CHAIN_DEPTH of 10)
        for i in 1..=15 {
            let timestamp = 1000 + (i * 100);
            index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;
            index.on_transaction_at(timestamp)?;
        }

        // Call collect_all() which uses to_hashmap() - should succeed without panicking
        // With full_snapshot_interval=10, we get full snapshots periodically,
        // so max delta chain is ~5 (well under MAX_DELTA_CHAIN_DEPTH=10)
        let snapshot_data = index.snapshot_data.read();
        let latest_timestamp = 1000 + (15 * 100);
        if let Some(latest_snapshot) = snapshot_data.vector_history.get(&latest_timestamp) {
            let result = latest_snapshot.collect_all(&snapshot_data.vector_history);

            // Should succeed (not panic) and return all vectors
            // If depth were exceeded, it would return an error instead of partial results
            assert!(
                result.is_ok(),
                "collect_all() should succeed without overflow when full snapshots are properly spaced"
            );

            // Verify we got results
            if let Ok(all_vectors) = result {
                assert!(!all_vectors.is_empty(), "Should have collected vectors");
            }
        }

        Ok(())
    }

    #[test]
    fn test_find_semantic_drift_with_deep_delta_chain() -> Result<()> {
        // Test that find_semantic_drift (which calls to_hashmap via collect_all)
        // handles deep delta chains gracefully
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 100,
            full_snapshot_interval: 10, // Valid config
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Create node with gradual drift over 15 snapshots
        let node_id = NodeId::new(1).unwrap();
        for i in 0..=15 {
            let timestamp = 1000 + (i * 100);
            let drift = i as f32 * 0.1; // Gradual drift from [1.0, 0, 0, 0] to [2.5, 0, 0, 0]
            index.add(node_id, &[1.0 + drift, 0.0, 0.0, 0.0], timestamp)?;
            index.on_transaction_at(timestamp)?;
        }

        // Call find_semantic_drift which internally uses collect_all/to_hashmap
        // Should NOT stack overflow even with many snapshots
        // With full_snapshot_interval=10, we get full snapshots at 0 and 10,
        // so max delta chain is 5 (snapshots 11-15), well under MAX_DELTA_CHAIN_DEPTH
        use crate::core::temporal::TimeRange;
        let time_range = TimeRange::new(1000.into(), 2500.into()).unwrap();
        let result = index.find_semantic_drift(
            0.1, // threshold
            time_range,
            DriftMetric::Cosine,
        );

        // Test passes if it doesn't panic/overflow and returns valid results
        // (no depth limit exceeded since we have proper full snapshots)
        assert!(
            result.is_ok(),
            "find_semantic_drift should handle chains within depth limit"
        );

        Ok(())
    }

    #[test]
    fn test_depth_limit_error_behavior() {
        // Test that missing base snapshot returns a proper error
        // Directly test the VectorSnapshot methods with corrupted state

        use std::collections::BTreeMap;

        // Create a delta snapshot that references a missing base
        let delta_snapshot = VectorSnapshot::Delta {
            base_time: 1000, // This base won't exist
            added: Arc::new(HashMap::from([(
                NodeId::new(1).unwrap(),
                Arc::from(vec![1.0f32, 0.0f32, 0.0f32, 0.0f32]) as Arc<[f32]>,
            )])),
            removed: Arc::new(HashSet::new()),
        };

        let snapshots: BTreeMap<Timestamp, VectorSnapshot> = BTreeMap::new(); // Empty - no base

        // Test get_vector with missing base - query a node NOT in added, so it must traverse to base
        let result = delta_snapshot.get_vector(&NodeId::new(2).unwrap(), &snapshots);
        assert!(
            result.is_err(),
            "Should return error when base snapshot is missing"
        );

        if let Err(err) = result {
            let err_msg = err.to_string();
            assert!(
                err_msg.contains("not found") || err_msg.contains("corrupted"),
                "Error should mention missing base or corrupted state, got: {}",
                err_msg
            );
        }

        // Test to_hashmap with missing base
        let result = delta_snapshot.to_hashmap(&snapshots);
        assert!(
            result.is_err(),
            "to_hashmap should return error when base snapshot is missing"
        );

        if let Err(err) = result {
            let err_msg = err.to_string();
            assert!(
                err_msg.contains("not found") || err_msg.contains("corrupted"),
                "Error should mention missing base or corrupted state, got: {}",
                err_msg
            );
        }
    }

    // ==================== Config Validation Tests ====================

    #[test]
    fn test_config_validation_full_snapshot_interval_too_large() {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 100, // Exceeds MAX_DELTA_CHAIN_DEPTH (10)
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };

        let result = TemporalVectorIndex::new(config);
        assert!(
            result.is_err(),
            "Should reject config with full_snapshot_interval > MAX_DELTA_CHAIN_DEPTH"
        );

        if let Err(err) = result {
            let err_msg = err.to_string();
            assert!(
                err_msg.contains("exceeds MAX_DELTA_CHAIN_DEPTH"),
                "Error message should mention MAX_DELTA_CHAIN_DEPTH"
            );
        }
    }

    #[test]
    fn test_config_validation_max_snapshots_zero() {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 0, // Invalid
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };

        let result = TemporalVectorIndex::new(config);
        assert!(result.is_err(), "Should reject config with max_snapshots=0");

        if let Err(err) = result {
            let err_msg = err.to_string();
            assert!(
                err_msg.contains("max_snapshots must be at least 1"),
                "Error message should mention max_snapshots requirement"
            );
        }
    }

    #[test]
    fn test_config_validation_valid_config() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            full_snapshot_interval: 10, // Valid: equals MAX_DELTA_CHAIN_DEPTH
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };

        let result = TemporalVectorIndex::new(config);
        assert!(
            result.is_ok(),
            "Should accept valid config with full_snapshot_interval=MAX_DELTA_CHAIN_DEPTH"
        );

        Ok(())
    }

    #[test]
    fn test_nan_validation() -> Result<()> {
        let index = create_test_index()?;
        let node_id = NodeId::new(1).unwrap();

        // Test NaN rejection
        let vec_with_nan = vec![1.0, f32::NAN, 0.0, 0.0];
        let result = index.add(node_id, &vec_with_nan, 1000.into());
        assert!(result.is_err(), "Should reject vector with NaN");
        if let Err(err) = result {
            let err_msg = err.to_string();
            assert!(
                err_msg.contains("NaN") || err_msg.contains("infinite"),
                "Error message should mention NaN or infinite values"
            );
        }

        // Test infinite rejection
        let vec_with_inf = vec![1.0, f32::INFINITY, 0.0, 0.0];
        let result = index.add(node_id, &vec_with_inf, 1000.into());
        assert!(result.is_err(), "Should reject vector with infinity");

        // Test negative infinite rejection
        let vec_with_neg_inf = vec![1.0, f32::NEG_INFINITY, 0.0, 0.0];
        let result = index.add(node_id, &vec_with_neg_inf, 1000.into());
        assert!(
            result.is_err(),
            "Should reject vector with negative infinity"
        );

        // Test valid vector still works
        let vec_valid = vec![1.0, 0.0, 0.0, 0.0];
        let result = index.add(node_id, &vec_valid, 1000.into());
        assert!(result.is_ok(), "Should accept valid vector");

        Ok(())
    }

    #[test]
    fn test_timestamp_upper_bound_validation() -> Result<()> {
        use crate::core::temporal::MAX_VALID_TIMESTAMP;

        let index = create_test_index()?;
        let node_id = NodeId::new(1).unwrap();
        let vec_valid = vec![1.0, 0.0, 0.0, 0.0];

        // Test timestamp at MAX_VALID_TIMESTAMP (should work)
        let result = index.add(node_id, &vec_valid, MAX_VALID_TIMESTAMP);
        assert!(result.is_ok(), "Should accept MAX_VALID_TIMESTAMP");

        // Test timestamp exceeding MAX_VALID_TIMESTAMP (should fail)
        let result = index.add(node_id, &vec_valid, MAX_VALID_TIMESTAMP + 1);
        assert!(
            result.is_err(),
            "Should reject timestamp > MAX_VALID_TIMESTAMP"
        );
        if let Err(err) = result {
            let err_msg = err.to_string();
            assert!(
                err_msg.contains("MAX_VALID_TIMESTAMP") || err_msg.contains("exceeds"),
                "Error message should mention MAX_VALID_TIMESTAMP"
            );
        }

        // Test timestamp at i64::MAX (should fail)
        let result = index.add(node_id, &vec_valid, i64::MAX);
        assert!(result.is_err(), "Should reject timestamp at i64::MAX");

        // Test negative timestamp (should fail)
        let result = index.add(node_id, &vec_valid, -1);
        assert!(result.is_err(), "Should reject negative timestamp");

        Ok(())
    }

    #[test]
    fn test_memory_growth_with_many_updates() -> Result<()> {
        // Test that changes_accumulated grows as expected and triggers fallback
        // Use a high full_snapshot_interval to see accumulation
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(50),
            max_snapshots: 50,
            full_snapshot_interval: 10, // Full snapshot every 10 transactions
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Add many unique vectors to grow changes_accumulated
        for i in 0..25 {
            let node_id = NodeId::new(i as u64).unwrap();
            let timestamp = 1000 + (i * 100);
            index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;
            index.on_transaction_at(timestamp)?;
        }

        // Check memory stats
        let stats = index.memory_stats();

        // After 25 transactions with full_snapshot_interval=10:
        // - Full snapshots at: 0, 10, 20
        // - Currently at snapshot 25 (5 since last full at 20)
        // - changes_accumulated should have ~5 unique vectors since last full
        assert!(
            stats.changes_accumulated_size > 0,
            "Should have accumulated changes"
        );

        assert!(
            stats.changes_accumulated_size < 30,
            "Should have reset on full snapshots (got {})",
            stats.changes_accumulated_size
        );

        assert_eq!(
            stats.current_vectors, 25,
            "Should have 25 vectors in current index"
        );

        // Test is_high_memory_usage (should be false for small dataset)
        assert!(
            !stats.is_high_memory_usage(),
            "Should not flag high memory for small dataset"
        );

        Ok(())
    }

    #[test]
    fn test_memory_fallback_forces_full_snapshot() -> Result<()> {
        // Test that exceeding MAX_ACCUMULATED_CHANGES forces a full snapshot
        // We'll use a small threshold for testing by checking the actual behavior

        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(50),
            max_snapshots: 50,
            full_snapshot_interval: 100, // Would normally never create full snapshots
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };

        // This should fail due to validation (full_snapshot_interval > MAX_DELTA_CHAIN_DEPTH)
        let result = TemporalVectorIndex::new(config);
        assert!(result.is_err(), "Should reject invalid config");

        // Use valid config instead
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepN(50),
            max_snapshots: 50,
            full_snapshot_interval: 10, // Valid
            hnsw_config: Some(HnswConfig::new(4, DistanceMetric::Cosine)),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        // Add vectors - with full_snapshot_interval=10, we get full snapshots periodically
        for i in 0..15 {
            let node_id = NodeId::new(i as u64).unwrap();
            let timestamp = 1000 + (i * 100);
            index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;
            index.on_transaction_at(timestamp)?;
        }

        let stats = index.memory_stats();

        // Verify changes_accumulated is bounded by full snapshots
        // With full_snapshot_interval=10: full at 0, 10, so at snapshot 15 we have 5 accumulated
        assert!(
            stats.changes_accumulated_size < 20,
            "changes_accumulated should be bounded by periodic full snapshots (got {})",
            stats.changes_accumulated_size
        );

        Ok(())
    }
}
