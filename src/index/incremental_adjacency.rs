//! Incremental CSR Adjacency Index
//!
//! LSM-tree inspired adjacency index with O(1) writes and cache-friendly reads.
//!
//! # Architecture
//!
//! Two-tier storage:
//! - **Frozen (L1)**: Immutable CSR for cache-friendly traversals
//! - **Delta (L0)**: Mutable buffer for recent insertions
//! - **Tombstones**: Pending deletions with temporal metadata
//!
//! Compaction periodically merges delta → frozen in background thread.

use crate::core::id::{EdgeId, NodeId};
use crate::index::adjacency::{AdjacencyEntry, AdjacencyIndex};
use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use smallvec::SmallVec;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Incremental CSR adjacency index with O(1) writes and fast reads.
///
/// Uses LSM-tree inspired design:
/// - L1 (frozen): Immutable CSR for bulk reads
/// - L0 (delta): Mutable buffer for recent writes
/// - Tombstones: Tracks deletions until next compaction
pub struct IncrementalAdjacencyIndex {
    /// Immutable CSR index (majority of edges)
    frozen: ArcSwap<AdjacencyIndex>,

    /// Delta buffer for recent insertions
    /// SmallVec<[_; 8]> keeps low-degree nodes on stack
    delta: DashMap<NodeId, SmallVec<[AdjacencyEntry; 8]>>,

    /// Pending deletions with temporal metadata
    tombstones: DashMap<EdgeId, Tombstone>,

    /// Statistics for compaction decisions
    stats: AdjacencyStats,

    /// Configuration
    config: IncrementalConfig,
}

/// Tombstone record for deleted edges with temporal metadata.
#[derive(Debug, Clone)]
pub struct Tombstone {
    pub edge_id: EdgeId,
    pub deleted_at: DateTime<Utc>,
    pub transaction_time: DateTime<Utc>,
}

/// Statistics tracked for compaction decisions.
#[derive(Debug)]
struct AdjacencyStats {
    delta_edge_count: AtomicUsize,
    delta_node_count: AtomicUsize,
    tombstone_count: AtomicUsize,
    frozen_edge_count: AtomicUsize,
    last_compaction: AtomicU64, // timestamp
}

/// Configuration for incremental adjacency index.
#[derive(Debug, Clone)]
pub struct IncrementalConfig {
    /// Compact when delta_edges > frozen_edges * ratio (default: 0.1)
    pub compaction_ratio: f64,

    /// Compact when delta_edges exceeds absolute count
    pub max_delta_edges: usize,

    /// Compact when tombstones exceed threshold
    pub max_tombstones: usize,

    /// SmallVec inline capacity (default: 8)
    pub smallvec_capacity: usize,

    /// Background compaction check interval
    pub check_interval: Duration,
}

impl Default for IncrementalConfig {
    fn default() -> Self {
        Self {
            compaction_ratio: 0.1,       // Compact at 10% growth
            max_delta_edges: 10_000,     // Or 10K edges
            max_tombstones: 1_000,       // Or 1K deletions
            smallvec_capacity: 8,        // 8 edges inline
            check_interval: Duration::from_secs(1), // Check every second
        }
    }
}

impl AdjacencyStats {
    fn new() -> Self {
        Self {
            delta_edge_count: AtomicUsize::new(0),
            delta_node_count: AtomicUsize::new(0),
            tombstone_count: AtomicUsize::new(0),
            frozen_edge_count: AtomicUsize::new(0),
            last_compaction: AtomicU64::new(0),
        }
    }
}

// ============================================================================
// Core Implementation - Phase 1
// ============================================================================

impl IncrementalAdjacencyIndex {
    /// Create a new empty incremental adjacency index.
    pub fn new() -> Self {
        Self::with_config(Arc::new(AdjacencyIndex::new()), IncrementalConfig::default())
    }

    /// Create an index from an existing frozen CSR.
    pub fn from_frozen(frozen: Arc<AdjacencyIndex>) -> Self {
        Self::with_config(frozen, IncrementalConfig::default())
    }

    /// Create an index with custom configuration.
    pub fn with_config(frozen: Arc<AdjacencyIndex>, config: IncrementalConfig) -> Self {
        let frozen_edge_count = frozen.edge_count();

        Self {
            frozen: ArcSwap::from_pointee((*frozen).clone()),
            delta: DashMap::new(),
            tombstones: DashMap::new(),
            stats: AdjacencyStats {
                frozen_edge_count: AtomicUsize::new(frozen_edge_count),
                ..AdjacencyStats::new()
            },
            config,
        }
    }

    /// Get frozen edge count.
    pub fn frozen_edge_count(&self) -> usize {
        self.stats.frozen_edge_count.load(Ordering::Relaxed)
    }

    /// Get delta edge count.
    pub fn delta_edge_count(&self) -> usize {
        self.stats.delta_edge_count.load(Ordering::Relaxed)
    }

    /// Get tombstone count.
    pub fn tombstone_count(&self) -> usize {
        self.stats.tombstone_count.load(Ordering::Relaxed)
    }

    /// Insert an edge into the delta buffer. O(1) amortized.
    ///
    /// The edge is added to the mutable delta layer and will be merged into
    /// the frozen CSR during the next compaction.
    pub fn insert(&self, source: NodeId, entry: AdjacencyEntry) {
        self.delta
            .entry(source)
            .or_insert_with(SmallVec::new)
            .push(entry);

        self.stats.delta_edge_count.fetch_add(1, Ordering::Relaxed);
    }
}

impl Default for IncrementalAdjacencyIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_creates_empty_index() {
        let index = IncrementalAdjacencyIndex::new();
        assert_eq!(index.frozen_edge_count(), 0);
        assert_eq!(index.delta_edge_count(), 0);
        assert_eq!(index.tombstone_count(), 0);
    }
}
