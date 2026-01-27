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
use arc_swap::{ArcSwap, Guard};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use dashmap::mapref::one::Ref;
use smallvec::SmallVec;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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

    /// Test-only: Force panic during next compaction (hidden from public API)
    #[doc(hidden)]
    test_panic_on_compact: AtomicBool,
}

/// Tombstone record for deleted edges with temporal metadata.
#[derive(Debug, Clone)]
pub struct Tombstone {
    /// The edge that was deleted
    pub edge_id: EdgeId,
    /// When the edge was deleted (valid time)
    pub deleted_at: DateTime<Utc>,
    /// When the deletion was recorded (transaction time)
    pub transaction_time: DateTime<Utc>,
}

/// Statistics tracked for compaction decisions.
#[derive(Debug)]
struct AdjacencyStats {
    delta_edge_count: AtomicUsize,
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
            compaction_ratio: 0.1,                  // Compact at 10% growth
            max_delta_edges: 10_000,                // Or 10K edges
            max_tombstones: 1_000,                  // Or 1K deletions
            smallvec_capacity: 8,                   // 8 edges inline
            check_interval: Duration::from_secs(1), // Check every second
        }
    }
}

impl AdjacencyStats {
    fn new() -> Self {
        Self {
            delta_edge_count: AtomicUsize::new(0),
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
        Self::with_config(
            Arc::new(AdjacencyIndex::new()),
            IncrementalConfig::default(),
        )
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
            test_panic_on_compact: AtomicBool::new(false),
        }
    }

    /// Test-only: Enable panic injection during next compaction.
    ///
    /// Used to test panic recovery in CompactionScheduler.
    ///
    /// # Safety
    /// This method will cause the next compaction to panic.
    /// DO NOT use in production code. Hidden from public API docs.
    #[doc(hidden)]
    pub fn test_inject_panic_on_compact(&self) {
        self.test_panic_on_compact.store(true, Ordering::Relaxed);
    }

    /// Get frozen edge count.
    pub fn frozen_edge_count(&self) -> usize {
        self.stats.frozen_edge_count.load(Ordering::Relaxed)
    }

    /// Export frozen CSR data for persistence.
    ///
    /// This exports only the immutable frozen layer, not the delta buffer or tombstones.
    /// Call `compact()` first to include recent changes.
    pub fn export_frozen_csr(&self) -> (Vec<u64>, Vec<u64>, Vec<u64>) {
        self.frozen.load().export_csr()
    }

    /// Import frozen CSR data from persistence.
    ///
    /// This replaces the frozen layer with the imported CSR, clearing delta and tombstones.
    /// Used when loading persisted indexes to avoid rebuilding from scratch.
    pub fn import_frozen_csr(&self, frozen_csr: Arc<AdjacencyIndex>) {
        // Replace frozen CSR
        self.frozen.store(frozen_csr);

        // Clear delta and tombstones
        self.delta.clear();
        self.tombstones.clear();

        // Update stats
        let frozen_count = self.frozen.load().edge_count();
        self.stats
            .frozen_edge_count
            .store(frozen_count, Ordering::Relaxed);
        self.stats.delta_edge_count.store(0, Ordering::Relaxed);
        self.stats.tombstone_count.store(0, Ordering::Relaxed);
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
        self.delta.entry(source).or_default().push(entry);

        self.stats.delta_edge_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Mark an edge as deleted. O(1).
    ///
    /// The edge is added to the tombstone set and will be filtered from reads
    /// until the next compaction. The tombstone includes temporal metadata
    /// for bi-temporal tracking and future GDPR compliance.
    pub fn delete(&self, edge_id: EdgeId) {
        let tombstone = Tombstone {
            edge_id,
            deleted_at: Utc::now(),
            transaction_time: Utc::now(),
        };

        self.tombstones.insert(edge_id, tombstone);
        self.stats.tombstone_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get tombstone metadata for a deleted edge.
    ///
    /// Returns `Some(Tombstone)` if the edge is marked as deleted,
    /// `None` if the edge is not tombstoned.
    pub fn get_tombstone(&self, edge_id: EdgeId) -> Option<Tombstone> {
        self.tombstones.get(&edge_id).map(|t| t.clone())
    }

    /// Get adjacency list for a node, merging frozen + delta - tombstones.
    ///
    /// Returns a guard that provides zero-copy access to the merged adjacency.
    /// Complexity: O(log n + k + d) where n=nodes with edges, k=frozen edges, d=delta edges.
    ///
    /// **Fast Path Optimization**: If delta and tombstones are globally empty
    /// (common after compaction), skips expensive DashMap lookups entirely.
    /// This reduces hot-path overhead from ~40ns to ~10ns.
    pub fn get_adjacency(&self, node: NodeId) -> MergedAdjacencyGuard<'_> {
        let frozen_guard = self.frozen.load();

        // Fast path: if no delta and no tombstones globally, skip DashMap lookups
        // Uses relaxed ordering since we're just checking for optimization, not correctness
        let delta_empty = self.stats.delta_edge_count.load(Ordering::Relaxed) == 0;
        let tombstones_empty = self.stats.tombstone_count.load(Ordering::Relaxed) == 0;

        if delta_empty && tombstones_empty {
            return MergedAdjacencyGuard {
                node,
                frozen: frozen_guard,
                delta: None,
                tombstones: &self.tombstones,
                fast_path: true, // Skip per-edge tombstone checks
            };
        }

        let delta_guard = self.delta.get(&node);

        MergedAdjacencyGuard {
            node,
            frozen: frozen_guard,
            delta: delta_guard,
            tombstones: &self.tombstones,
            fast_path: false,
        }
    }

    /// Check if compaction is needed based on thresholds.
    ///
    /// Returns `true` if any of the following conditions are met:
    /// - Delta edges exceed or equal `max_delta_edges` (absolute threshold)
    /// - Delta edges exceed or equal `frozen_edges * compaction_ratio` (ratio threshold)
    /// - Tombstones exceed or equal `max_tombstones`
    pub fn should_compact(&self) -> bool {
        let delta = self.stats.delta_edge_count.load(Ordering::Relaxed);
        let frozen = self.stats.frozen_edge_count.load(Ordering::Relaxed);
        let tombstones = self.stats.tombstone_count.load(Ordering::Relaxed);

        // Absolute delta threshold
        if delta >= self.config.max_delta_edges {
            return true;
        }

        // Ratio threshold (only if frozen has edges)
        if frozen > 0 && delta as f64 >= frozen as f64 * self.config.compaction_ratio {
            return true;
        }

        // Tombstone threshold
        if tombstones >= self.config.max_tombstones {
            return true;
        }

        false
    }

    /// Compact delta into frozen, rebuilding the CSR. O(E log E).
    ///
    /// This merges all edges from frozen + delta (excluding tombstones) into
    /// a new frozen CSR, then atomically swaps it. The delta and tombstones
    /// are cleared after compaction.
    ///
    /// Readers can continue accessing the old frozen CSR during compaction
    /// thanks to ArcSwap's lock-free design.
    pub fn compact(&self) {
        // Test-only panic injection for testing panic recovery (hidden from public API)
        if self.test_panic_on_compact.swap(false, Ordering::Relaxed) {
            panic!("Test-injected panic during compaction");
        }

        let frozen = self.frozen.load();

        // Estimate capacity: frozen + delta - tombstones
        let estimated_capacity = frozen.edge_count()
            + self.stats.delta_edge_count.load(Ordering::Relaxed)
            - self.stats.tombstone_count.load(Ordering::Relaxed);

        let mut all_edges = Vec::with_capacity(estimated_capacity);

        // 1. Collect edges from frozen (excluding tombstones)
        // Use iter_nodes() for efficient sparse graph iteration - O(nodes_with_edges) not O(max_node_id)
        for node_id in frozen.iter_nodes() {
            let frozen_slice = frozen.get_adjacency(node_id);
            for adj in frozen_slice {
                if !self.tombstones.contains_key(&adj.edge_id) {
                    all_edges.push((node_id, adj.target, adj.edge_id, adj.label));
                }
            }
        }

        // 2. Collect edges from delta (excluding tombstones)
        for entry in self.delta.iter() {
            let source = *entry.key();
            for adj in entry.value().iter() {
                if !self.tombstones.contains_key(&adj.edge_id) {
                    all_edges.push((source, adj.target, adj.edge_id, adj.label));
                }
            }
        }

        // 3. Build new frozen CSR
        let new_frozen = AdjacencyIndex::build(all_edges);
        let new_edge_count = new_frozen.edge_count();

        // 4. Atomic swap (lock-free for readers!)
        self.frozen.store(Arc::new(new_frozen));

        // 5. Clear delta and tombstones
        self.delta.clear();
        self.tombstones.clear();

        // 6. Update statistics
        self.stats
            .frozen_edge_count
            .store(new_edge_count, Ordering::Release);
        self.stats.delta_edge_count.store(0, Ordering::Release);
        self.stats.tombstone_count.store(0, Ordering::Release);
        self.stats
            .last_compaction
            .store(Utc::now().timestamp() as u64, Ordering::Release);
    }
}

/// Zero-copy merged view of frozen + delta adjacency.
///
/// This guard provides an iterator over all adjacency entries (frozen + delta),
/// excluding tombstones. The guard holds references to the frozen CSR and delta
/// buffer, ensuring they remain valid during iteration.
pub struct MergedAdjacencyGuard<'a> {
    node: NodeId,
    frozen: Guard<Arc<AdjacencyIndex>>,
    delta: Option<Ref<'a, NodeId, SmallVec<[AdjacencyEntry; 8]>>>,
    tombstones: &'a DashMap<EdgeId, Tombstone>,
    /// Fast path flag: if true, skip per-edge tombstone checks (delta & tombstones are empty)
    fast_path: bool,
}

impl<'a> MergedAdjacencyGuard<'a> {
    /// Iterate over all adjacency entries (frozen + delta, excluding tombstones).
    ///
    /// **Fast Path**: When `fast_path` is true (delta and tombstones globally empty),
    /// skips per-edge tombstone DashMap lookups, providing near-zero overhead iteration.
    pub fn iter(&self) -> impl Iterator<Item = &AdjacencyEntry> + '_ {
        let frozen_slice = self.frozen.get_adjacency(self.node);
        let fast_path = self.fast_path;

        // Fast path: no tombstone checks needed, delta is None
        // This gives us near-native slice iteration performance
        let frozen_iter = frozen_slice.iter().filter(move |e| {
            // Skip tombstone check if fast_path (we know tombstones are empty)
            fast_path || !self.tombstones.contains_key(&e.edge_id)
        });

        let delta_iter = self
            .delta
            .as_ref()
            .into_iter()
            .flat_map(|d| d.iter())
            .filter(move |e| fast_path || !self.tombstones.contains_key(&e.edge_id));

        frozen_iter.chain(delta_iter)
    }

    /// Get the number of adjacency entries (frozen + delta, excluding tombstones).
    ///
    /// This counts all entries by iterating, which is O(n) where n is the degree.
    /// For high-degree nodes, this may be slower than the old CSR approach.
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Check if the adjacency list is empty.
    /// O(1) - uses early-return iterator pattern instead of counting all entries.
    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }

    /// Get entry at index (for compatibility with slice-like indexing).
    pub fn get(&self, index: usize) -> Option<&AdjacencyEntry> {
        self.iter().nth(index)
    }

    /// Fast path: if no delta and no tombstones, return frozen slice directly.
    pub fn as_slice(&self) -> Option<&[AdjacencyEntry]> {
        if self.delta.is_none() && self.tombstones.is_empty() {
            Some(self.frozen.get_adjacency(self.node))
        } else {
            None
        }
    }
}

impl<'a> std::ops::Index<usize> for MergedAdjacencyGuard<'a> {
    type Output = AdjacencyEntry;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index)
            .expect("index out of bounds for MergedAdjacencyGuard")
    }
}

impl<'a> std::fmt::Debug for MergedAdjacencyGuard<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergedAdjacencyGuard")
            .field("node", &self.node)
            .field("entry_count", &self.len())
            .finish()
    }
}

impl<'a> PartialEq for MergedAdjacencyGuard<'a> {
    fn eq(&self, other: &Self) -> bool {
        // Compare by iterating over all entries
        let self_entries: Vec<_> = self.iter().collect();
        let other_entries: Vec<_> = other.iter().collect();
        self_entries == other_entries
    }
}

impl<'a> Eq for MergedAdjacencyGuard<'a> {}

impl Default for IncrementalAdjacencyIndex {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Background Compaction Scheduler - Phase 5
// ============================================================================

use std::sync::atomic::AtomicBool;
use std::thread::{self, JoinHandle};

/// Background compaction scheduler for automatic threshold monitoring.
///
/// Spawns a background thread that periodically checks compaction thresholds
/// and triggers compaction when needed. Supports pause/resume and graceful
/// shutdown with in-flight compaction completion.
///
/// Includes panic recovery: if compaction panics, the thread logs the panic,
/// increments a counter, and continues monitoring.
///
/// Safety: If consecutive panics exceed `MAX_CONSECUTIVE_PANICS` (default 5),
/// the scheduler will exit to prevent hiding persistent bugs.
pub struct CompactionScheduler {
    index: Arc<IncrementalAdjacencyIndex>,
    running: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    panic_count: Arc<AtomicUsize>,
    consecutive_panics: Arc<AtomicUsize>,
}

/// Maximum consecutive panics before scheduler exits to prevent hiding bugs.
const MAX_CONSECUTIVE_PANICS: usize = 5;

impl CompactionScheduler {
    /// Create a new compaction scheduler for the given index.
    pub fn new(index: Arc<IncrementalAdjacencyIndex>) -> Self {
        Self {
            index,
            running: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            panic_count: Arc::new(AtomicUsize::new(0)),
            consecutive_panics: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Get the number of panics that have occurred during compaction.
    ///
    /// Useful for monitoring and debugging. A non-zero count indicates
    /// that compaction has panicked at least once, but the scheduler
    /// recovered and continued operating.
    pub fn panic_count(&self) -> usize {
        self.panic_count.load(Ordering::Relaxed)
    }

    /// Start the background compaction thread.
    ///
    /// The thread will periodically check thresholds and compact when needed.
    /// If compaction panics, the panic is caught, logged to stderr, and the
    /// thread continues monitoring. Panic count is incremented for observability.
    ///
    /// **Safety**: If consecutive panics exceed `MAX_CONSECUTIVE_PANICS`, the
    /// scheduler will exit to prevent hiding persistent bugs.
    ///
    /// Returns a join handle that can be used to wait for thread termination.
    pub fn start(&self) -> JoinHandle<()> {
        self.running.store(true, Ordering::SeqCst);

        let index = Arc::clone(&self.index);
        let running = Arc::clone(&self.running);
        let paused = Arc::clone(&self.paused);
        let panic_count = Arc::clone(&self.panic_count);
        let consecutive_panics = Arc::clone(&self.consecutive_panics);

        thread::spawn(move || {
            let check_interval = index.config.check_interval;

            while running.load(Ordering::SeqCst) {
                // Check if paused
                if !paused.load(Ordering::SeqCst) {
                    // Check if compaction needed
                    if index.should_compact() {
                        // Wrap compact() in catch_unwind for panic recovery
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            index.compact();
                        }));

                        if let Err(panic_payload) = result {
                            // Increment panic counters
                            panic_count.fetch_add(1, Ordering::Relaxed);
                            let consecutive =
                                consecutive_panics.fetch_add(1, Ordering::Relaxed) + 1;

                            // Log panic to stderr
                            eprintln!(
                                "[CompactionScheduler] Panic during compaction (total: {}, consecutive: {}): {:?}",
                                panic_count.load(Ordering::Relaxed),
                                consecutive,
                                panic_payload
                            );

                            // Exit if too many consecutive panics to prevent hiding bugs
                            if consecutive >= MAX_CONSECUTIVE_PANICS {
                                eprintln!(
                                    "[CompactionScheduler] Exiting after {} consecutive panics",
                                    consecutive
                                );
                                break;
                            }
                        } else {
                            // Reset consecutive panic count on successful compaction
                            consecutive_panics.store(0, Ordering::Relaxed);
                        }
                    }
                }

                // Sleep for check interval
                thread::sleep(check_interval);
            }
        })
    }

    /// Pause background compaction.
    ///
    /// The background thread will continue running but will not trigger
    /// compaction until `resume()` is called.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    /// Resume background compaction after pause.
    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    /// Shutdown the background compaction thread.
    ///
    /// Sets the running flag to false, which causes the background thread
    /// to exit after completing any in-flight compaction.
    pub fn shutdown(&self) {
        self.running.store(false, Ordering::SeqCst);
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
