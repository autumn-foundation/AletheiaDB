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
//!     hnsw_config,
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
//! let time_range = TimeRange::new(1000000, 2000000);
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
///     hnsw_config: HnswConfig::new(384, DistanceMetric::Cosine),
/// };
/// ```
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

    /// Base HNSW configuration for all indexes (current + snapshots)
    pub hnsw_config: HnswConfig,
}

impl TemporalVectorConfig {
    /// Creates a default configuration with the given HNSW config.
    ///
    /// Defaults:
    /// - Strategy: TransactionInterval(10) - mirrors anchor+delta pattern
    /// - Retention: KeepN(100)
    /// - Max snapshots: 100
    pub fn default_with_hnsw(hnsw_config: HnswConfig) -> Self {
        TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            hnsw_config,
        }
    }

    /// Creates a configuration for time-based snapshots.
    pub fn with_time_interval(hnsw_config: HnswConfig, interval_secs: u64) -> Self {
        TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TimeInterval(interval_secs),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            hnsw_config,
        }
    }

    /// Creates a configuration for change-based snapshots.
    pub fn with_change_threshold(hnsw_config: HnswConfig, threshold: f64) -> Self {
        TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::ChangeThreshold(threshold),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            hnsw_config,
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
type VectorSnapshot = Arc<HashMap<NodeId, Arc<[f32]>>>;

/// Storage structure for snapshot data.
/// Can be either a full HNSW index or a delta index.
#[derive(Clone)]
enum SnapshotIndex {
    Full(Arc<HnswIndex>),
    Delta(Arc<DeltaIndex>),
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
    ) -> Result<Vec<(NodeId, f32)>>
    {
        match self {
            SnapshotIndex::Full(index) => index.search_with_filter(query, k, predicate),
            SnapshotIndex::Delta(delta) => delta.search_with_filter(query, k, predicate),
        }
    }

    fn len(&self) -> usize {
        match self {
            SnapshotIndex::Full(index) => index.len(),
            SnapshotIndex::Delta(delta) => {
                // Approximation: Base len + added len.
                // We ignore removed count for approximation speed.
                delta.base.len() + delta.added.len()
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
struct DeltaIndex {
    /// The base snapshot this delta is built upon (usually a Full snapshot)
    base: Arc<SnapshotIndex>,
    /// Vectors added or updated since the base snapshot
    added: Arc<HnswIndex>,
    /// IDs of vectors that were removed or updated (invalidating the base version)
    removed: Arc<HashSet<NodeId>>,
}

impl DeltaIndex {
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>> {
        // 1. Search added vectors
        let mut results = self.added.search(query, k)?;

        // 2. Search base vectors with filter
        // Filter out any ID that is in the 'removed' set
        // (This includes updated nodes, which are present in 'added')
        let removed = &self.removed;
        let base_results = self.base.search_with_filter(query, k, &|id| !removed.contains(id))?;

        // 3. Merge results
        results.extend(base_results);
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);

        Ok(results)
    }

    fn search_with_filter(
        &self,
        query: &[f32],
        k: usize,
        predicate: &(dyn Fn(&NodeId) -> bool + Send + Sync),
    ) -> Result<Vec<(NodeId, f32)>>
    {
        // Combine user predicate with our removed set
        let removed = &self.removed;
        let combined_predicate = |id: &NodeId| predicate(id) && !removed.contains(id);

        // Search added (using user predicate)
        let mut results = self.added.search_with_filter(query, k, predicate)?;

        // Search base (using combined predicate)
        let base_results = self.base.search_with_filter(query, k, &combined_predicate)?;

        // Merge
        results.extend(base_results);
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
        Self::new_at(config, Self::current_timestamp())
    }

    /// Creates a new temporal vector index with an explicit initial timestamp (for testing).
    pub fn new_at(config: TemporalVectorConfig, initial_time: Timestamp) -> Result<Self> {
        // Create current HNSW index
        let current = Arc::new(HnswIndex::new(config.hnsw_config.clone())?);

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
    fn current_timestamp() -> Timestamp {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as Timestamp
    }

    /// Adds a vector to the current index and tracks it for snapshot creation.
    pub fn add(&self, id: NodeId, vector: &[f32], timestamp: Timestamp) -> Result<()> {
        // Store vector data (for snapshot copying)
        let vector_arc: Arc<[f32]> = Arc::from(vector);
        self.vectors.insert(id, vector_arc);

        if timestamp < 0 {
            return Err(Error::Temporal(TemporalError::InvalidTimeRange {
                start: timestamp,
                end: 0,
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
        self.on_transaction_at(Self::current_timestamp())
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
        let snapshot = HnswIndex::new(self.config.hnsw_config.clone())?;
        let mut vector_snapshot = HashMap::with_capacity(self.vectors.len());

        for entry in self.vectors.iter() {
            let node_id = *entry.key();
            let vector = entry.value().clone();
            snapshot.add(node_id, vector.as_ref())?;
            vector_snapshot.insert(node_id, vector);
        }

        Ok((
            SnapshotIndex::Full(Arc::new(snapshot)),
            Arc::new(vector_snapshot),
        ))
    }

    /// Builds a DELTA snapshot relative to a base snapshot.
    fn build_delta_snapshot(
        &self,
        base: Arc<SnapshotIndex>,
        changes: &HashSet<NodeId>,
    ) -> Result<(SnapshotIndex, VectorSnapshot)> {
        // Create small HNSW for added/updated vectors
        let added_config = self.config.hnsw_config.clone();
        let added = HnswIndex::new(added_config)?;

        // We also need the full vector snapshot for history
        // TODO: Optimize this to be delta-based too (Phase 4)
        let mut vector_snapshot = HashMap::with_capacity(self.vectors.len());
        for entry in self.vectors.iter() {
            let node_id = *entry.key();
            let vector = entry.value().clone();
            vector_snapshot.insert(node_id, vector);
        }

        // Populate 'added' HNSW with changed vectors
        for &node_id in changes {
            if let Some(vector) = self.vectors.get(&node_id) {
                added.add(node_id, vector.as_ref())?;
            }
        }

        let delta = DeltaIndex {
            base,
            added: Arc::new(added),
            removed: Arc::new(changes.clone()),
        };

        Ok((
            SnapshotIndex::Delta(Arc::new(delta)),
            Arc::new(vector_snapshot),
        ))
    }

    /// Checks if a snapshot should be created and creates it if needed.
    fn check_and_create_snapshot(&self, current_time: Timestamp) -> Result<()> {
        // Step 1: Quick check with read lock
        let (_should_snapshot, is_full) = {
            let metadata = self.metadata.read();
            let should = self.should_create_snapshot(&metadata, current_time)?;
            if !should {
                return Ok(());
            }

            // Create FULL snapshot every 10 snapshots (configurable in future)
            let is_full = metadata.snapshots_since_full >= 10 || metadata.total_snapshots == 0;
            (true, is_full)
        };

        // Step 2: Build snapshot outside locks
        let (snapshot, vector_snapshot) = if is_full {
            self.build_full_snapshot()?
        } else {
            // Get base and changes for delta
            // We need to be careful with concurrency here.
            // Using changes_accumulated from read lock is safe because:
            // 1. It only grows (monotonic) until reset
            // 2. We only care about changes up to THIS point
            // 3. Any concurrent new changes will be captured in NEXT delta

            let (base, changes) = {
                let data = self.snapshot_data.read();
                let meta = self.metadata.read();

                let base = data
                    .snapshots
                    .get(&meta.last_full_snapshot_time)
                    .map(|(_, snap)| Arc::new(snap.clone()));

                let changes = meta.changes_accumulated.clone();
                (base, changes)
            };

            if let Some(base) = base {
                self.build_delta_snapshot(base, &changes)?
            } else {
                // Fallback: if base is missing (e.g. pruned), force full snapshot
                self.build_full_snapshot()?
            }
        };

        // Step 3: Acquire locks and perform final check before inserting
        {
            let mut metadata = self.metadata.write();

            // Double-check race condition
            if !self.should_create_snapshot(&metadata, current_time)? {
                return Ok(());
            }

            // If we built a Delta but decided we need a Full (e.g. forced by prune),
            // or vice versa due to race, we might have mismatch.
            // But since 'is_full' determination is deterministic based on counts,
            // and counts only increase under write lock, it should be stable.
            // However, strictly speaking, if another thread committed a snapshot
            // between Step 1 and 3, 'snapshots_since_full' changed.
            // So we should re-evaluate 'is_full' here.

            let _actual_is_full = metadata.snapshots_since_full >= 10 || metadata.total_snapshots == 0;

            // If our built snapshot type doesn't match what's needed now, we must discard and retry.
            // This is rare. For simplicity, we just use what we built if it's safe.
            // If we built Full, it's always safe to use (just maybe redundant).
            // If we built Delta, we need to ensure the Base is still valid.

            // Simpler approach: Just insert what we built, and update metadata accordingly.
            // If we built Full, we reset accumulation.
            // If we built Delta, we don't.
            // The only risk is if we built Delta but logic says "Full".
            // Then we insert Delta, and next time we might trigger Full.
            // Or we check `match snapshot { Full => true, Delta => false }`.

            let built_is_full = matches!(snapshot, SnapshotIndex::Full(_));

            let stable_id = metadata.total_snapshots;
            let mut snapshot_data = self.snapshot_data.write();

            snapshot_data.insert(current_time, stable_id, snapshot, vector_snapshot);

            // Update metadata based on what we actually built
            metadata.reset(current_time, built_is_full);

            // Enforce snapshot limit
            while snapshot_data.len() > self.config.max_snapshots {
                snapshot_data.remove_oldest();
            }
        }

        Ok(())
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
                let elapsed_micros = current_time - metadata.last_snapshot_time;
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

                let elapsed_micros = current_time - metadata.last_snapshot_time;
                let elapsed_secs = elapsed_micros / 1_000_000;
                let by_time = elapsed_secs >= *time_interval_secs as i64;

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

    /// Creates a snapshot of the current index.
    fn create_snapshot(&self, timestamp: Timestamp) -> Result<()> {
        // Manual snapshot creation - force FULL snapshot for simplicity/safety
        // in manual mode, or reuse the check logic?
        // To respect "avoid full copy" even in manual mode, we should use the smart logic.
        // But create_snapshot is usually forced.

        // Let's use build_full_snapshot to ensure independence for manual snapshots
        // or check metadata.

        // Better: Use the same logic as check_and_create_snapshot but force "should=true"
        // But here we'll just build a FULL snapshot to be safe and simple.
        // Manual snapshots are rare and usually imply "I want a clean checkpoint".

        let (snapshot, vector_snapshot) = self.build_full_snapshot()?;

        {
            let mut metadata = self.metadata.write();
            let mut snapshot_data = self.snapshot_data.write();

            let stable_id = metadata.total_snapshots;
            snapshot_data.insert(timestamp, stable_id, snapshot, vector_snapshot);

            metadata.reset(timestamp, true); // True because we built FULL

            while snapshot_data.len() > self.config.max_snapshots {
                snapshot_data.remove_oldest();
            }
        }

        Ok(())
    }

    /// Manually creates a snapshot at the current time.
    pub fn create_manual_snapshot(&self) -> Result<()> {
        let timestamp = Self::current_timestamp();
        // Use check_and_create_snapshot logic to benefit from Delta optimization
        // We simulate a trigger by setting a flag? No, we can just call internal method
        // that forces creation.

        // We'll reimplement smart logic here but forced.

        let (is_full, base, changes) = {
             let data = self.snapshot_data.read();
             let meta = self.metadata.read();
             let is_full = meta.snapshots_since_full >= 10 || meta.total_snapshots == 0;
             let base = data.snapshots.get(&meta.last_full_snapshot_time).map(|(_, s)| Arc::new(s.clone()));
             let changes = meta.changes_accumulated.clone();
             (is_full, base, changes)
        };

        let (snapshot, vector_snapshot) = if is_full || base.is_none() {
             self.build_full_snapshot()?
        } else {
             self.build_delta_snapshot(base.unwrap(), &changes)?
        };

        {
            let mut metadata = self.metadata.write();
            let mut snapshot_data = self.snapshot_data.write();

            let stable_id = metadata.total_snapshots;
            let built_is_full = matches!(snapshot, SnapshotIndex::Full(_));

            snapshot_data.insert(timestamp, stable_id, snapshot, vector_snapshot);
            metadata.reset(timestamp, built_is_full);

             while snapshot_data.len() > self.config.max_snapshots {
                snapshot_data.remove_oldest();
            }
        }

        Ok(())
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
                let current_time = Self::current_timestamp();
                let duration_micros = duration.as_micros() as Timestamp;
                let cutoff_time = current_time.saturating_sub(duration_micros);

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
            if let Some(vector) = snapshot_vectors.get(&node_id) {
                evolution.push((timestamp, Arc::clone(vector)));
            }
        }

        Ok(evolution)
    }

    /// Tracks semantic drift: how a node's similarity to a reference changed over time.
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
            for (node_id, curr_vector) in snapshot_vectors.iter() {
                if let Some(prev_vector) = last_vectors.get(node_id) {
                    let drift = Self::compute_drift_distance(prev_vector, curr_vector, metric)?;
                    let max_drift_entry = max_drifts.entry(*node_id).or_insert(0.0);
                    *max_drift_entry = max_drift_entry.max(drift);
                }
                last_vectors.insert(*node_id, Arc::clone(curr_vector));
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
    pub fn get_snapshot_info(&self) -> Vec<SnapshotInfo> {
        let snapshot_data = self.snapshot_data.read();

        snapshot_data
            .snapshots
            .iter()
            .map(|(&timestamp, (stable_id, snapshot))| {
                let current_time = Self::current_timestamp();
                let age_micros = current_time - timestamp;

                SnapshotInfo {
                    snapshot_id: *stable_id,
                    timestamp,
                    vector_count: snapshot.len(),
                    // Size estimation: approximate
                    size_bytes: snapshot.len() * snapshot.dimensions() * 4 + 1024,
                    age: Duration::from_micros(age_micros as u64),
                }
            })
            .collect()
    }

    /// Returns the number of snapshots currently stored.
    pub fn snapshot_count(&self) -> usize {
        self.snapshot_data.read().snapshots.len()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_index() -> Result<TemporalVectorIndex> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1000),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
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

        index.add(node1, &vec1, timestamp)?;

        assert_eq!(index.current_index().len(), 1);
        Ok(())
    }

    #[test]
    fn test_snapshot_creation_transaction_interval() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(2),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new_at(config, base_time)?;

        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], base_time)?;
        assert_eq!(index.snapshot_count(), 0);

        index.add(
            NodeId::new(2).unwrap(),
            &[0.0, 1.0, 0.0, 0.0],
            base_time + 2_000_000,
        )?;
        index.on_transaction_at(base_time + 2_000_000)?;
        assert_eq!(index.snapshot_count(), 1);

        Ok(())
    }

    #[test]
    fn test_snapshot_creation_change_threshold() -> Result<()> {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::ChangeThreshold(0.5),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new(config)?;

        for i in 0..5 {
            index.add(
                NodeId::new(i).unwrap(),
                &[i as f32, 0.0, 0.0, 0.0],
                (i * 1000) as Timestamp,
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new(config)?;

        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0], 1000)?;
        index.on_transaction_at(1000)?;

        let query = vec![0.9, 0.1, 0.0, 0.0];
        let results = index.find_similar_as_of(&query, 2, 1000)?;

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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new(config)?;

        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.on_transaction_at(1000)?;

        index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0], 2000)?;
        index.on_transaction_at(2000)?;

        index.add(NodeId::new(3).unwrap(), &[0.0, 0.0, 1.0, 0.0], 3000)?;
        index.on_transaction_at(3000)?;

        let query = vec![1.0, 0.0, 0.0, 0.0];
        let time_range = TimeRange::new(1000, 3000);
        let results = index.find_similar_in_range(&query, 5, time_range)?;

        assert!(!results.is_empty());

        for (timestamp, similar_nodes) in results {
            assert!((1000..=3000).contains(&timestamp));
            assert!(!similar_nodes.is_empty());
        }

        Ok(())
    }

    #[test]
    fn test_snapshot_info() -> Result<()> {
        let index = create_test_index()?;

        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.create_manual_snapshot()?;

        let info = index.get_snapshot_info();
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].snapshot_id, 0);
        assert!(info[0].timestamp > 0);

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
        assert_eq!(config.hnsw_config, hnsw_config);
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new(config)?;

        for i in 0..10 {
            index.add(NodeId::new(i).unwrap(), &[i as f32, 0.0, 0.0, 0.0], 1000)?;
            index.on_transaction_at(1000)?;
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new(config)?;

        for i in 0..5 {
            let timestamp = (1000 * (i + 1)) as i64;
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        for i in 0..5 {
            let timestamp = (1000 * (i + 1)) as i64;
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

        let info = index.get_snapshot_info();
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new(config)?;

        for i in 0..3 {
            let timestamp = (1000 * (i + 1)) as i64;
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new(config)?;

        let current_time = TemporalVectorIndex::current_timestamp();

        index.add(
            NodeId::new(1).unwrap(),
            &[1.0, 0.0, 0.0, 0.0],
            current_time - 10_000_000,
        )?;
        index.create_manual_snapshot()?;

        index.add(
            NodeId::new(2).unwrap(),
            &[0.0, 1.0, 0.0, 0.0],
            current_time - 7_000_000,
        )?;
        index.create_manual_snapshot()?;

        index.add(
            NodeId::new(3).unwrap(),
            &[0.0, 0.0, 1.0, 0.0],
            current_time - 3_000_000,
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.on_transaction_at(1000)?;
        assert_eq!(index.snapshot_count(), 1);

        index.add(node_id, &[0.0, 1.0, 0.0, 0.0], 2000)?;
        index.on_transaction_at(2000)?;
        assert_eq!(index.snapshot_count(), 2);

        index.add(node_id, &[0.0, 0.0, 1.0, 0.0], 3000)?;
        index.on_transaction_at(3000)?;
        assert_eq!(index.snapshot_count(), 3);

        let time_range = TimeRange::new(1000, 3000);
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        for i in 1..=5 {
            let timestamp = i * 1000;
            index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;
            index.on_transaction_at(timestamp)?;
        }

        let time_range = TimeRange::new(2000, 4000);
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let other_node = NodeId::new(1).unwrap();
        index.add(other_node, &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.on_transaction_at(1000)?;

        let node_id = NodeId::new(42).unwrap();
        let time_range = TimeRange::new(1000, 2000);
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.on_transaction_at(1000)?;

        index.add(node_id, &[0.9, 0.1, 0.0, 0.0], 2000)?;
        index.on_transaction_at(2000)?;

        index.add(node_id, &[0.5, 0.5, 0.0, 0.0], 3000)?;
        index.on_transaction_at(3000)?;

        let reference = vec![1.0, 0.0, 0.0, 0.0];
        let time_range = TimeRange::new(1000, 3000);
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.on_transaction_at(1000)?;

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 2000)?;
        index.on_transaction_at(2000)?;

        index.add(node_id, &[0.707, 0.707, 0.0, 0.0], 3000)?;
        index.on_transaction_at(3000)?;

        index.add(node_id, &[0.0, 1.0, 0.0, 0.0], 4000)?;
        index.on_transaction_at(4000)?;

        let time_range = TimeRange::new(1000, 4000);
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.on_transaction_at(1000)?;

        let time_range = TimeRange::new(1000, 2000);
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.on_transaction_at(1000)?;

        index.add(node_id, &[0.0, 1.0, 0.0, 0.0], 2000)?;
        index.on_transaction_at(2000)?;

        let time_range = TimeRange::new(1000, 2000);
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        for i in 1..=5 {
            let timestamp = i * 1000;
            index.add(node_id, &[i as f32, 0.0, 0.0, 0.0], timestamp)?;
            index.on_transaction_at(timestamp)?;
        }

        assert_eq!(index.snapshot_count(), 3);

        let time_range = TimeRange::new(1000, 5000);
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new_at(config, 1000)?;

        let node_id = NodeId::new(42).unwrap();

        index.add(node_id, &[1.0, 0.0, 0.0, 0.0], 1000)?;
        index.on_transaction_at(1000)?;

        let wrong_dimension_ref = vec![1.0, 0.0, 0.0];
        let time_range = TimeRange::new(1000, 2000);

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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
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

        let time_range = TimeRange::new(0, i64::MAX);

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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
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

        let time_range = TimeRange::new(0, i64::MAX);
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
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

        let time_range = TimeRange::new(0, i64::MAX);
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let index = TemporalVectorIndex::new(config)?;

        let ts = 10000i64;

        let node1 = NodeId::new(1).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0], ts)?;
        index.on_transaction_at(ts)?;

        let time_range = TimeRange::new(0, 100);
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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
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

        let time_range = TimeRange::new(0, i64::MAX);

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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
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

        let time_range = TimeRange::new(0, i64::MAX);

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
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
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

        let time_range = TimeRange::new(0, i64::MAX);

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
}
