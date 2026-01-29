//! Configuration types for temporal vector indexing.
//!
//! This module defines snapshot strategies, retention policies,
//! and configuration validation for the temporal vector index.

use crate::index::vector::hnsw::HnswConfig;
use crate::utils::{Result, VectorError};
use std::time::Duration;

/// Maximum number of retries when creating a snapshot due to races (default: 3)
pub const MAX_SNAPSHOT_RETRIES: usize = 3;

/// Maximum depth of delta chain traversal before forcing full snapshot (default: 10)
/// This prevents unbounded delta chains and ensures reconstruction performance.
/// Mirrors the full_snapshot_interval default value.
pub const MAX_DELTA_CHAIN_DEPTH: usize = 10;

/// Minimum capacity estimate for HashMap pre-allocation (default: 100)
/// Used when estimating capacity for vector collections to avoid excessive resizing.
///
/// **Justification**: 100 is a reasonable baseline that:
/// - Avoids excessive allocations for small datasets (most vectors fit in 1 allocation)
/// - Prevents too many resizes for medium datasets
/// - Has negligible overhead (~800 bytes for empty capacity-100 HashMap)
/// - Aligns with common batch sizes in vector databases
pub const MIN_CAPACITY_ESTIMATE: usize = 100;

/// Maximum accumulated changes before forcing a full snapshot (default: 100,000)
///
/// If `changes_accumulated` exceeds this threshold, force a full snapshot
/// regardless of `full_snapshot_interval`. This prevents unbounded memory growth
/// in write-heavy workloads.
///
/// **Justification**: 100k NodeIds × 8 bytes = 800 KB overhead, which is acceptable
/// but starting to impact memory. Forcing a full snapshot resets the accumulator.
pub const MAX_ACCUMULATED_CHANGES: usize = 100_000;

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
