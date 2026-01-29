use super::config::MAX_ACCUMULATED_CHANGES;
use crate::core::temporal::Timestamp;
use std::time::Duration;

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
        self.changes_accumulated_size > MAX_ACCUMULATED_CHANGES
    }

    /// Estimates memory overhead from accumulated changes in bytes.
    ///
    /// This is a rough estimate assuming ~8 bytes per NodeId in the HashSet.
    pub fn estimated_accumulated_bytes(&self) -> usize {
        self.changes_accumulated_size * 8 // NodeId is u64
    }
}
