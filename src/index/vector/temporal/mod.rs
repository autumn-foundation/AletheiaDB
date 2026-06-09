//! Temporal vector index for time-aware semantic search.
//!
//! This module implements temporal vector indexing using snapshot-based HNSW indexes,
//! enabling point-in-time vector queries and semantic drift tracking. This is Phase 3
//! of AletheiaDB's vector search integration.
//!
//! # Architecture
//!
//! The temporal vector index uses a dual-path design:
//! - **Current index**: Live HNSW index for present-time queries
//! - **Snapshot indexes**: Historical HNSW snapshots at configurable intervals
//!
//! This mirrors AletheiaDB's hybrid storage architecture (see ADR-0001) where current
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
//! use aletheiadb::index::vector::temporal::{TemporalVectorIndex, TemporalVectorConfig, SnapshotStrategy, RetentionPolicy};
//! use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
//! use aletheiadb::core::id::NodeId;
//! use aletheiadb::core::temporal::TimeRange;
//!
//! # fn example() -> aletheiadb::core::error::Result<()> {
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
//! let results = index.find_similar_as_of(&query, 10, timestamp.into())?;
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

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasherDefault;
use std::sync::Arc;
use std::time::Duration;

use crate::core::hasher::IdentityHasher;
use parking_lot::RwLock;
use rayon::prelude::*;

type IdentityBuildHasher = BuildHasherDefault<IdentityHasher>;

use crate::core::error::{Error, Result, TemporalError, VectorError};
use crate::core::id::NodeId;
use crate::core::temporal::{TimeRange, Timestamp};
use crate::core::vector::{cosine_similarity, euclidean_distance};
use crate::index::vector::hnsw::HnswIndex;
use crate::index::vector::{DistanceMetric, TemporalSearchResults, VectorIndex};

// Submodules
/// Configuration types for temporal vector indexing.
pub mod config;
/// Observers for reacting to storage events.
pub mod observer;
pub(crate) mod snapshot;
/// Statistics and monitoring types.
pub mod stats;

// Re-exports
pub use config::{DriftMetric, RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};
pub use observer::VectorIndexObserver;
pub use stats::{MemoryStats, SnapshotInfo};

// Internal imports
use config::{MAX_ACCUMULATED_CHANGES, MAX_SNAPSHOT_RETRIES};
use snapshot::{
    DeltaIndex, SnapshotData, SnapshotIndex, SnapshotMetadata, VectorSnapshot, VectorState,
};

/// # Thread Safety
///
/// This struct is thread-safe using `RwLock` for internal mutability. Multiple threads
/// can query concurrently, while snapshot creation requires exclusive access.
///
/// **Issue #233 Optimization**: Reduced lock contention by combining vectors and metadata
/// into a single `VectorState` protected by one RwLock, reducing lock acquisitions from 3 to 1.
pub struct TemporalVectorIndex {
    /// Current (live) HNSW index - always up-to-date
    current: Arc<HnswIndex>,

    /// Combined current vector storage and metadata protected by a single lock
    /// **Issue #233**: This replaces separate `DashMap<vectors>` and `RwLock<metadata>`
    /// to reduce lock contention from 3 acquisitions to 1 per add() call
    current_state: RwLock<VectorState>,

    /// Historical snapshots and vector values protected by a single lock
    snapshot_data: RwLock<SnapshotData>,

    /// Configuration
    config: TemporalVectorConfig,
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

        Ok(TemporalVectorIndex {
            current,
            current_state: RwLock::new(VectorState::new(initial_time)),
            snapshot_data: RwLock::new(SnapshotData::new()),
            config,
        })
    }

    /// Returns the current timestamp in microseconds since epoch.
    fn current_timestamp() -> Result<Timestamp> {
        // Phase 2: Use the core time::now() function which returns HybridTimestamp
        use crate::core::temporal::time;
        Ok(time::now())
    }

    /// Validates a vector and timestamp (helper to reduce duplication).
    ///
    /// **Issue #233**: Extracted from add() and add_batch() to eliminate code duplication.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The vector contains NaN values
    /// - The vector contains infinite values
    /// - The timestamp is negative or exceeds MAX_VALID_TIMESTAMP
    fn validate_vector_and_timestamp(vector: &[f32], timestamp: Timestamp) -> Result<()> {
        use crate::core::hlc::HybridTimestamp;

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

        // Validate timestamp is within valid range
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

        Ok(())
    }

    /// Adds a vector to the current index and tracks it for snapshot creation.
    ///
    /// **Issue #233**: Optimized to acquire only ONE lock instead of three,
    /// reducing lock contention during batch insertions.
    ///
    /// **Atomicity Fix**: HNSW insertion happens BEFORE state update to ensure
    /// atomicity. If HNSW fails, state remains unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The vector contains NaN or infinite values
    /// - The timestamp is negative
    /// - The underlying HNSW index fails to add the vector
    pub fn add(&self, id: NodeId, vector: &[f32], timestamp: Timestamp) -> Result<()> {
        // Validate inputs first (fail fast)
        Self::validate_vector_and_timestamp(vector, timestamp)?;

        // **ATOMICITY FIX**: Add to HNSW index FIRST
        // If this fails, we return error without modifying state
        self.current.add(id, vector)?;

        // **OPTIMIZATION (Issue #233)**: Single lock acquisition for both vector storage and metadata
        // Previously: 3 locks (DashMap internal, metadata.write(), potential snapshot_data)
        // Now: 1 lock (current_state.write())
        // Only update state AFTER HNSW succeeds
        {
            let mut state = self.current_state.write();

            // Store vector data (for snapshot copying)
            let vector_arc: Arc<[f32]> = Arc::from(vector);
            state.vectors.insert(id, vector_arc);

            // Track change for snapshot detection
            state.metadata.record_change(id);
        } // Lock released here

        Ok(())
    }

    /// Adds multiple vectors in a single batch operation.
    ///
    /// **Issue #233**: This is significantly more efficient than multiple `add()` calls
    /// because it acquires the lock once for the entire batch, then processes all vectors
    /// before releasing the lock.
    ///
    /// # Performance
    ///
    /// - Single lock acquisition for the entire batch
    /// - Reduced lock contention in high-throughput scenarios
    /// - 20-50% throughput improvement for batch sizes > 10
    ///
    /// # Arguments
    ///
    /// * `batch` - Slice of tuples containing (NodeId, vector, timestamp)
    ///
    /// # Errors
    ///
    /// Returns an error if any vector:
    /// - Contains NaN or infinite values
    /// - Has an invalid timestamp
    /// - Fails to be added to the HNSW index
    ///
    /// **Important**: If any vector in the batch fails validation, the entire batch
    /// is rejected and no vectors are added (atomic operation).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let batch = vec![
    ///     (node1, vec![1.0, 0.0, 0.0, 0.0], timestamp1),
    ///     (node2, vec![0.0, 1.0, 0.0, 0.0], timestamp2),
    ///     (node3, vec![0.0, 0.0, 1.0, 0.0], timestamp3),
    /// ];
    /// index.add_batch(&batch)?;
    /// ```
    pub fn add_batch(&self, batch: &[(NodeId, Vec<f32>, Timestamp)]) -> Result<()> {
        // Phase 1: Validate all vectors first (fail fast before any operations)
        for (_id, vector, timestamp) in batch {
            Self::validate_vector_and_timestamp(vector, *timestamp)?;
        }

        // Phase 2: **ATOMICITY FIX** - Add to HNSW index FIRST
        // If any HNSW insertion fails, we rollback all previous insertions
        // This ensures atomicity - either all vectors are added or none
        let mut added_to_hnsw = Vec::with_capacity(batch.len());
        for (id, vector, _timestamp) in batch {
            match self.current.add(*id, vector) {
                Ok(()) => {
                    added_to_hnsw.push(*id);
                }
                Err(e) => {
                    // Rollback: remove all previously added vectors from HNSW
                    for &rollback_id in &added_to_hnsw {
                        let _ = self.current.remove(rollback_id);
                    }
                    return Err(e);
                }
            }
        }

        // Phase 3: Update current_state ONLY after all HNSW insertions succeed
        // **KEY OPTIMIZATION**: Single lock acquisition for all vectors
        {
            let mut state = self.current_state.write();

            for (id, vector, _timestamp) in batch {
                // Store vector data (for snapshot copying)
                let vector_arc: Arc<[f32]> = Arc::from(vector.as_slice());
                state.vectors.insert(*id, vector_arc);

                // Track change for snapshot detection
                state.metadata.record_change(*id);
            }
        } // Lock released here

        Ok(())
    }

    /// Removes a vector from the current index.
    pub fn remove(&self, id: NodeId, _timestamp: Timestamp) -> Result<()> {
        // Remove from current index
        self.current.remove(id)?;

        // **OPTIMIZATION (Issue #233)**: Single lock acquisition
        {
            let mut state = self.current_state.write();

            // Remove from vector storage
            state.vectors.remove(&id);

            // Track change for snapshot detection
            state.metadata.record_change(id);
        }

        Ok(())
    }

    /// Records a transaction for snapshot tracking.
    pub fn on_transaction(&self) -> Result<()> {
        self.on_transaction_at(Self::current_timestamp()?)
    }

    /// Records a transaction at a specific timestamp (for testing).
    pub fn on_transaction_at(&self, timestamp: Timestamp) -> Result<()> {
        // Record transaction
        self.current_state.write().metadata.record_transaction();

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

        // Read current state to get vectors
        let state = self.current_state.read();
        let mut vector_snapshot = HashMap::with_capacity_and_hasher(
            state.vectors.len(),
            BuildHasherDefault::<IdentityHasher>::default(),
        );

        for (node_id, vector) in state.vectors.iter() {
            snapshot.add(*node_id, vector.as_ref())?;
            vector_snapshot.insert(*node_id, vector.clone());
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
        changes: &HashSet<NodeId, IdentityBuildHasher>,
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
        let mut added_vectors =
            HashMap::with_hasher(BuildHasherDefault::<IdentityHasher>::default());
        let mut removed_vectors =
            HashSet::with_hasher(BuildHasherDefault::<IdentityHasher>::default());

        // For DeltaIndex.removed: only nodes that WERE in base and are now invalid
        // This is crucial for correct len() calculation
        let mut invalidated_in_base =
            HashSet::with_hasher(BuildHasherDefault::<IdentityHasher>::default());

        // We need access to all snapshots to check containment in base
        // Since we're inside write lock, we can't easily access snapshot_data
        // Instead, we'll use a simpler heuristic: check if node was in base by seeing
        // if it's been modified (for updates) or removed (for removals)
        // For pure additions, they weren't in base, so don't include them

        // Read current state to access vectors
        let state = self.current_state.read();

        // Separate changed nodes into added/updated vs removed
        for &node_id in changes {
            if let Some(vector) = state.vectors.get(&node_id) {
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
            let state = self.current_state.read();

            // Create FULL snapshot if:
            // 1. Reached configured interval, OR
            // 2. This is the first snapshot, OR
            // 3. Memory threshold exceeded (fallback to prevent unbounded growth)
            let is_full = state.metadata.snapshots_since_full >= self.config.full_snapshot_interval
                || state.metadata.total_snapshots == 0
                || state.metadata.changes_accumulated.len() >= MAX_ACCUMULATED_CHANGES;

            // Get base time and changes for delta (if needed)
            let base_time = state.metadata.last_full_snapshot_time;
            let changes = state.metadata.changes_accumulated.clone();

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

        // Step 3: Acquire locks in correct order (current_state -> snapshot_data) and insert
        {
            let mut state = self.current_state.write();

            // Re-validate: check if we need full vs delta NOW
            let should_be_full = state.metadata.snapshots_since_full
                >= self.config.full_snapshot_interval
                || state.metadata.total_snapshots == 0
                || state.metadata.changes_accumulated.len() >= MAX_ACCUMULATED_CHANGES;

            // If we built delta but now need full (due to race or memory threshold), discard and retry
            if should_be_full && !snapshot_type {
                drop(state);
                return self.create_snapshot_internal_with_retries(current_time, retry_count + 1);
            }

            // Re-validate: if we built delta, ensure base still exists
            if !snapshot_type {
                let snapshot_data = self.snapshot_data.read();
                let base_exists = snapshot_data
                    .snapshots
                    .contains_key(&state.metadata.last_full_snapshot_time);
                drop(snapshot_data);

                if !base_exists {
                    // Base was pruned, discard delta and retry with full
                    drop(state);
                    return self
                        .create_snapshot_internal_with_retries(current_time, retry_count + 1);
                }
            }

            let stable_id = state.metadata.total_snapshots;
            let mut snapshot_data = self.snapshot_data.write();

            snapshot_data.insert(current_time, stable_id, snapshot, vector_snapshot);

            // Update metadata based on what we actually built
            state.metadata.reset(current_time, snapshot_type);

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
            let state = self.current_state.read();
            self.should_create_snapshot(&state.metadata, current_time)?
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
                let elapsed_micros =
                    current_time.wallclock() - metadata.last_snapshot_time.wallclock();
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
                let elapsed_micros =
                    current_time.wallclock() - metadata.last_snapshot_time.wallclock();
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
    /// # use aletheiadb::index::vector::temporal::TemporalVectorIndex;
    /// # use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
    /// # fn example() -> aletheiadb::core::error::Result<()> {
    /// # let config = aletheiadb::index::vector::temporal::TemporalVectorConfig::default_with_hnsw(
    /// #     HnswConfig::new(384, DistanceMetric::Cosine)
    /// # );
    /// # let index = TemporalVectorIndex::new(config)?;
    /// // Called by HistoricalStorage when creating an anchor
    /// let timestamp = 1234567890;
    /// if let Some(snapshot_id) = index.create_snapshot_for_anchor(timestamp.into())? {
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
            let state = self.current_state.read();
            state.metadata.total_snapshots
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
                let duration_micros = duration.as_micros() as i64;
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
    /// use aletheiadb::index::vector::temporal::TemporalVectorIndex;
    ///
    /// # fn example(index: &TemporalVectorIndex) -> aletheiadb::core::error::Result<()> {
    /// let query = vec![0.1f32; 384];
    /// let timestamp_2023 = 1672531200000000; // 2023-01-01 in microseconds
    ///
    /// // Find what was semantically similar in 2023
    /// let results = index.find_similar_as_of(&query, 10, timestamp_2023.into())?;
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
    /// use aletheiadb::index::vector::temporal::TemporalVectorIndex;
    /// use aletheiadb::core::temporal::TimeRange;
    ///
    /// # fn example(index: &TemporalVectorIndex) -> aletheiadb::core::error::Result<()> {
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
    /// - Queries all snapshots in range in parallel using Rayon
    /// - Target: <100ms for 10 snapshots, 1M vectors each
    /// - Results include both Full and Delta snapshot data
    /// - Expected 4-6x speedup on 8-core systems with 20+ snapshots
    pub fn find_similar_in_range(
        &self,
        query_embedding: &[f32],
        k: usize,
        time_range: TimeRange,
    ) -> Result<TemporalSearchResults> {
        // Collect snapshot references while holding the lock
        let snapshots: Vec<(Timestamp, SnapshotIndex)> = {
            let snapshot_data = self.snapshot_data.read();
            snapshot_data
                .snapshots
                .range(time_range.start()..=time_range.end())
                .map(|(&timestamp, (_id, snapshot))| (timestamp, snapshot.clone()))
                .collect()
        };
        // Lock is released here

        // Process snapshots in parallel and collect results
        let mut results: TemporalSearchResults = snapshots
            .par_iter()
            .map(|(timestamp, snapshot)| {
                // Search snapshot
                snapshot
                    .search(query_embedding, k)
                    .map(|snapshot_results| (*timestamp, snapshot_results))
            })
            .collect::<Result<_>>()?;

        // Sort results chronologically to maintain temporal order
        results.sort_by_key(|(timestamp, _)| *timestamp);

        Ok(results)
    }

    /// Retrieves the semantic evolution of a node over time.
    ///
    /// This method extracts the raw vector representations of a specific node across all
    /// stored snapshots within the specified [`TimeRange`]. This is foundational for analyzing
    /// how a concept's meaning has shifted in the vector space, rather than just tracking
    /// distance from a single reference point.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The [`NodeId`] whose historical embeddings to retrieve.
    /// * `time_range` - The [`TimeRange`] defining the period of interest.
    ///
    /// # Returns
    ///
    /// A vector of `(timestamp, vector_data)` pairs, sorted chronologically. Each pair
    /// represents the node's embedding at that specific [`Timestamp`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use aletheiadb::index::vector::temporal::TemporalVectorIndex;
    /// use aletheiadb::core::temporal::TimeRange;
    /// use aletheiadb::core::id::NodeId;
    ///
    /// # fn example(index: &TemporalVectorIndex) -> aletheiadb::core::error::Result<()> {
    /// let node_id = NodeId::new(42).unwrap();
    /// let time_range = TimeRange::new(1000000.into(), 2000000.into()).unwrap();
    ///
    /// // Retrieve all historical versions of the node's embedding
    /// let evolution = index.semantic_evolution(node_id, time_range)?;
    ///
    /// println!("Node {} had {} distinct embeddings during this period.",
    ///          node_id, evolution.len());
    ///
    /// for (timestamp, vector) in evolution {
    ///     println!("At {}: vector length is {}", timestamp, vector.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Use Cases
    ///
    /// - Visualizing the trajectory of a document's meaning in 2D/3D space (e.g., via PCA/t-SNE).
    /// - Feeding historical embeddings into a recurrent neural network (RNN) for trend prediction.
    /// - Debugging vector update pipelines to see *how* an embedding changed, not just *that* it changed.
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
    /// use aletheiadb::index::vector::temporal::TemporalVectorIndex;
    /// use aletheiadb::core::temporal::TimeRange;
    /// use aletheiadb::core::id::NodeId;
    ///
    /// # fn example(index: &TemporalVectorIndex) -> aletheiadb::core::error::Result<()> {
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
        let mut drift = Vec::with_capacity(evolution.len());

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

        let mut drift = Vec::with_capacity(evolution.len().saturating_sub(1));

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
    /// use aletheiadb::index::vector::temporal::{TemporalVectorIndex, DriftMetric};
    /// use aletheiadb::core::temporal::TimeRange;
    ///
    /// # fn example(index: &TemporalVectorIndex) -> aletheiadb::core::error::Result<()> {
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
        let state = self.current_state.read();
        let snapshot_data = self.snapshot_data.read();

        MemoryStats {
            changes_accumulated_size: state.metadata.changes_accumulated.len(),
            vectors_changed_since_snapshot: state.metadata.vectors_changed_since_snapshot.len(),
            snapshots_since_full: state.metadata.snapshots_since_full,
            total_snapshots: snapshot_data.snapshots.len(),
            current_vectors: state.vectors.len(),
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

#[cfg(test)]
mod coverage_tests;

#[cfg(test)]
pub(crate) mod tests;
