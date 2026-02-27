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

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use arc_swap::{ArcSwap, Guard};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use dashmap::mapref::one::Ref;
use smallvec::SmallVec;

use crate::core::id::{EdgeId, NodeId};
use crate::index::adjacency::{AdjacencyEntry, AdjacencyIndex};

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

    /// Test-only: Force panic during next threshold check (hidden from public API)
    #[doc(hidden)]
    test_panic_on_check: AtomicBool,
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
            test_panic_on_check: AtomicBool::new(false),
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

    /// Test-only: Enable panic injection during next threshold check.
    #[doc(hidden)]
    pub fn test_inject_panic_on_check(&self) {
        self.test_panic_on_check.store(true, Ordering::Relaxed);
    }

    /// Get frozen edge count.
    pub fn frozen_edge_count(&self) -> usize {
        self.stats.frozen_edge_count.load(Ordering::Acquire)
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
    ///
    /// # Warning
    ///
    /// This method will clear any existing delta edges and tombstones. This is intentional
    /// when used as part of the full import flow (e.g., `CurrentIndexes::import_csr_data`)
    /// which reconstructs the delta after importing.
    ///
    /// If you need to preserve existing data, use `try_import_frozen_csr` instead which
    /// returns an error if data would be lost.
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

    /// Safely import frozen CSR data, returning an error if data would be lost.
    ///
    /// Unlike `import_frozen_csr`, this method will not clear non-empty delta or
    /// tombstones. Returns an error describing what would be lost.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if import succeeded (delta and tombstones were empty)
    /// - `Err(String)` describing uncommitted data that would be lost
    pub fn try_import_frozen_csr(&self, frozen_csr: Arc<AdjacencyIndex>) -> Result<(), String> {
        let delta_count = self.delta.len();
        let tombstone_count = self.tombstones.len();

        if delta_count > 0 || tombstone_count > 0 {
            return Err(format!(
                "Cannot import: {} uncommitted delta edges and {} tombstones would be lost. \
                 Call compact() first or use import_frozen_csr() to force.",
                delta_count, tombstone_count
            ));
        }

        self.import_frozen_csr(frozen_csr);
        Ok(())
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
        // Increment stats BEFORE insertion to prevent race condition with compaction.
        // If compaction runs concurrently, it might see the edge in `delta.retain()`
        // and subtract from stats. If we increment after, the subtraction could underflow.
        // By incrementing first, we ensure the count is always >= actual items in DashMap.
        self.stats.delta_edge_count.fetch_add(1, Ordering::Relaxed);

        self.delta.entry(source).or_default().push(entry);
    }

    /// Mark an edge as deleted. O(1).
    ///
    /// The edge is added to the tombstone set and will be filtered from reads
    /// until the next compaction. The tombstone includes temporal metadata
    /// for bi-temporal tracking and future GDPR compliance.
    pub fn delete(&self, edge_id: EdgeId) {
        // Increment stats BEFORE insertion to prevent race condition with compaction.
        self.stats.tombstone_count.fetch_add(1, Ordering::Relaxed);

        let tombstone = Tombstone {
            edge_id,
            deleted_at: Utc::now(),
            transaction_time: Utc::now(),
        };

        self.tombstones.insert(edge_id, tombstone);
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
        if self.test_panic_on_check.swap(false, Ordering::Relaxed) {
            panic!("Test-injected panic during should_compact check");
        }

        let delta = self.stats.delta_edge_count.load(Ordering::Relaxed);
        let frozen = self.stats.frozen_edge_count.load(Ordering::Acquire);
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

        // Memory usage note:
        // Compaction temporarily increases memory usage as we build the new frozen index
        // while the old one, delta, and tombstones are still in memory.
        // Peak memory ~ 2 * (frozen + delta).

        // 1. Drain Tombstones
        // We use a local set to filter edges during this compaction.
        // Using DashMap::retain to remove them from the main set.
        let mut local_tombstones = HashSet::new();
        self.tombstones.retain(|edge_id, _| {
            local_tombstones.insert(*edge_id);
            false // Remove from main map
        });
        // Subtract drained tombstones from stats
        self.stats
            .tombstone_count
            .fetch_sub(local_tombstones.len(), Ordering::Relaxed);

        // 2. Drain Delta
        // Use retain to atomically move entries from DashMap to local buffer.
        // This prevents race conditions where new edges inserted during iteration
        // would be lost by a subsequent clear().
        let delta_count_estimate = self.stats.delta_edge_count.load(Ordering::Relaxed);
        let mut local_delta = Vec::with_capacity(delta_count_estimate);
        let mut drained_edge_count = 0;

        self.delta.retain(|source, edges| {
            for entry in edges.iter() {
                local_delta.push((*source, *entry));
                drained_edge_count += 1;
            }
            false // Remove from main map
        });
        // Subtract drained delta edges from stats
        self.stats
            .delta_edge_count
            .fetch_sub(drained_edge_count, Ordering::Relaxed);

        // Estimate capacity: frozen + delta (we don't subtract tombstones to stay safe)
        let estimated_capacity = frozen.edge_count() + drained_edge_count;
        let mut all_edges = Vec::with_capacity(estimated_capacity);

        // 3. Collect edges from frozen (excluding tombstones)
        // Use iter_nodes() for efficient sparse graph iteration
        for node_id in frozen.iter_nodes() {
            let frozen_slice = frozen.get_adjacency(node_id);
            for adj in frozen_slice {
                if !local_tombstones.contains(&adj.edge_id) {
                    all_edges.push((node_id, adj.target, adj.edge_id, adj.label));
                }
            }
        }

        // 4. Collect edges from delta (filtering with local_tombstones)
        // Note: Delta might contain edges that were also tombstoned just before drain
        for (source, adj) in local_delta {
            if !local_tombstones.contains(&adj.edge_id) {
                all_edges.push((source, adj.target, adj.edge_id, adj.label));
            }
        }

        // 5. Build new frozen CSR
        let new_frozen = AdjacencyIndex::build(all_edges);
        let new_edge_count = new_frozen.edge_count();

        // 6. Atomic swap (lock-free for readers!)
        self.frozen.store(Arc::new(new_frozen));

        // 7. Update statistics
        self.stats
            .frozen_edge_count
            .store(new_edge_count, Ordering::Release);
        self.stats
            .last_compaction
            .store(Utc::now().timestamp() as u64, Ordering::Release);
    }

    /// Get a frozen-only view for read transactions (hot path optimization).
    ///
    /// Returns `Some(FrozenAdjacencyView)` if delta and tombstones are empty,
    /// allowing direct slice access without iterator overhead.
    ///
    /// Returns `None` if delta or tombstones exist, meaning callers should
    /// use `get_adjacency()` instead for correct merged results.
    ///
    /// # Performance
    ///
    /// When available, `FrozenAdjacencyView::get_adjacency()` returns a direct
    /// `&[AdjacencyEntry]` slice with ~10ns overhead vs ~80ns for the merged path.
    /// This is ideal for read-heavy workloads after compaction.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // At start of read transaction, try to get frozen view
    /// if let Some(view) = index.frozen_view() {
    ///     // Hot path: direct slice access
    ///     let edges = view.get_adjacency(node_id);
    /// } else {
    ///     // Fallback: use merged iterator
    ///     let guard = index.get_adjacency(node_id);
    /// }
    /// ```
    pub fn frozen_view(&self) -> Option<FrozenAdjacencyView> {
        // Only available when delta and tombstones are empty
        if self.stats.delta_edge_count.load(Ordering::Relaxed) > 0 {
            return None;
        }
        if self.stats.tombstone_count.load(Ordering::Relaxed) > 0 {
            return None;
        }

        Some(FrozenAdjacencyView {
            frozen: self.frozen.load(),
        })
    }
}

/// Read-only view of frozen adjacency for hot path access.
///
/// This view captures the frozen CSR at creation time and provides
/// direct slice access without delta/tombstone checks. Only available
/// when delta and tombstones are empty (typically after compaction).
///
/// # Performance
///
/// `get_adjacency()` is ~8x faster than `MergedAdjacencyGuard::iter()`:
/// - FrozenAdjacencyView: ~10ns (direct slice)
/// - MergedAdjacencyGuard: ~80ns (guard + iterator + filter)
///
/// # Thread Safety
///
/// The view holds an Arc guard to the frozen CSR, so it remains valid
/// even if compaction occurs after view creation. However, the view
/// will not see edges added after it was created.
pub struct FrozenAdjacencyView {
    frozen: Guard<Arc<AdjacencyIndex>>,
}

impl FrozenAdjacencyView {
    /// Get adjacency list as a direct slice (zero-copy, no iterator overhead).
    #[inline]
    pub fn get_adjacency(&self, node: NodeId) -> &[AdjacencyEntry] {
        self.frozen.get_adjacency(node)
    }
}

/// Zero-copy merged view of frozen + delta adjacency.
///
/// This guard provides an iterator over all adjacency entries (frozen + delta),
/// excluding tombstones. The guard holds references to the frozen CSR and delta
/// buffer, ensuring they remain valid during iteration.
pub struct MergedAdjacencyGuard<'a> {
    pub(crate) node: NodeId,
    pub(crate) frozen: Guard<Arc<AdjacencyIndex>>,
    pub(crate) delta: Option<Ref<'a, NodeId, SmallVec<[AdjacencyEntry; 8]>>>,
    pub(crate) tombstones: &'a DashMap<EdgeId, Tombstone>,
    /// Fast path flag: if true, skip per-edge tombstone checks (delta & tombstones are empty)
    pub(crate) fast_path: bool,
}

impl<'a> MergedAdjacencyGuard<'a> {
    /// Iterate over all adjacency entries (frozen + delta, excluding tombstones).
    ///
    /// **Fast Path**: When `fast_path` is true (delta and tombstones globally empty),
    /// skips per-edge tombstone DashMap lookups, providing near-zero overhead iteration.
    #[inline]
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
    #[inline]
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Check if the adjacency list is empty.
    /// O(1) - uses early-return iterator pattern instead of counting all entries.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }

    /// Get entry at index (for compatibility with slice-like indexing).
    ///
    /// # Warning
    ///
    /// This method is O(n) as it must iterate through the combined frozen and
    /// delta layers. For high-degree nodes, prefer using `iter()` directly.
    #[inline]
    pub fn get(&self, index: usize) -> Option<&AdjacencyEntry> {
        self.iter().nth(index)
    }

    /// Get an upper bound on the number of entries (frozen + delta).
    ///
    /// This is O(1) and suitable for pre-allocating vectors. The actual
    /// number of entries may be smaller if tombstones exist.
    #[inline]
    pub fn capacity_hint(&self) -> usize {
        let frozen_len = self.frozen.get_adjacency(self.node).len();
        let delta_len = self.delta.as_ref().map(|d| d.len()).unwrap_or(0);
        frozen_len + delta_len
    }

    /// Get the exact length if it can be determined in O(1).
    ///
    /// Returns `Some(len)` if in fast path (no tombstones or delta), otherwise `None`.
    #[inline]
    pub fn fast_len(&self) -> Option<usize> {
        if self.fast_path {
            Some(self.frozen.get_adjacency(self.node).len())
        } else {
            None
        }
    }

    /// Fast path: if no delta and no tombstones, return frozen slice directly.
    #[inline]
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
        self.get(index).unwrap_or_else(|| {
            panic!(
                "index {} out of bounds for MergedAdjacencyGuard (len: {})",
                index,
                self.len()
            )
        })
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

            // Graceful shutdown: perform one final compaction if there are pending changes
            // This ensures tests and application shutdown leave the index in a clean state
            if index.delta_edge_count() > 0 || index.tombstone_count() > 0 {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    index.compact();
                }));
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

    #[test]
    fn test_guard_capacity_hint_and_fast_len() {
        use crate::core::interning::InternedString;
        let index = IncrementalAdjacencyIndex::new();
        let node = NodeId::new(1).unwrap();

        {
            let guard = index.get_adjacency(node);
            // Empty index
            assert_eq!(guard.capacity_hint(), 0);
            assert_eq!(guard.fast_len(), Some(0));
        }

        // Add to delta
        let entry = AdjacencyEntry::new(
            NodeId::new(2).unwrap(),
            EdgeId::new(1).unwrap(),
            InternedString::from_raw(1),
        );
        index.insert(node, entry);

        {
            let guard = index.get_adjacency(node);
            assert_eq!(guard.capacity_hint(), 1);
            assert_eq!(guard.fast_len(), None); // Not fast path anymore because delta is not empty
        }

        // Compact
        index.compact();

        {
            let guard = index.get_adjacency(node);
            assert_eq!(guard.capacity_hint(), 1);
            assert_eq!(guard.fast_len(), Some(1));
        }

        // Add tombstone
        index.delete(EdgeId::new(1).unwrap());

        {
            let guard = index.get_adjacency(node);
            assert_eq!(guard.capacity_hint(), 1); // capacity_hint is upper bound, doesn't account for tombstones
            assert_eq!(guard.fast_len(), None); // Not fast path anymore because tombstones exist
        }
    }
}
