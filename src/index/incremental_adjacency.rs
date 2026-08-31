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
use parking_lot::Mutex;
use smallvec::SmallVec;

use crate::core::hasher::IdHashBuilder;
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
    ///
    /// Keyed by `NodeId`, an already-unique internal u64, so identity-hashed
    /// via [`IdHashBuilder`] to avoid SipHash overhead on `get_adjacency`'s
    /// non-empty-delta path.
    delta: DashMap<NodeId, SmallVec<[AdjacencyEntry; 8]>, IdHashBuilder>,

    /// Pending deletions with temporal metadata
    ///
    /// Identity-hashed for the same reason as `delta` — `EdgeId` is already a
    /// unique internal u64.
    tombstones: DashMap<EdgeId, Tombstone, IdHashBuilder>,

    /// Statistics for compaction decisions
    stats: AdjacencyStats,

    /// Configuration
    config: IncrementalConfig,

    /// True while compaction has published a new frozen CSR whose absorbed
    /// delta entries have not been retired yet (Issue #3810).
    ///
    /// Compaction publishes the new CSR **before** retiring the delta entries it
    /// merged (see [`compact`](Self::compact) for why the opposite order tears
    /// reads). For that window an entry can be in *both* layers, so readers
    /// must not emit it twice. This flag is a pure performance gate: when it is
    /// set, a delta entry is emitted only if the frozen slice being read does
    /// not already contain it. A stale `true` costs one extra binary search; a
    /// stale `false` is impossible for a reader that has already loaded the new
    /// CSR, because the flag is set before that CSR is published and cleared
    /// only after every absorbed entry has been retired.
    publish_window: AtomicBool,

    /// Serializes [`compact`](Self::compact) with itself.
    ///
    /// Compaction snapshots `frozen`, drains `delta`/`tombstones` into local
    /// buffers, rebuilds a new CSR from that snapshot, and stores it. Two
    /// compactions running concurrently would each snapshot the *same* frozen
    /// CSR while only one of them drains a given delta entry -- the later store
    /// then overwrites the earlier one and the drained edges are **lost**
    /// (Issue #3810). Reads and writes are unaffected: this lock is taken only
    /// by compaction, never on the read or insert path.
    compaction_lock: Mutex<()>,

    /// Test-only: Force panic during next compaction (hidden from public API)
    #[doc(hidden)]
    test_panic_on_compact: AtomicBool,
}

/// Occupancy of one adjacency index's two layers (Issue #3810).
///
/// The frozen-CSR read fast path ([`IncrementalAdjacencyIndex::frozen_view`])
/// is available exactly when `delta_edges == 0 && tombstones == 0`, so this
/// snapshot is the direct observable for "are reads on the fast path?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdjacencyLayerStats {
    /// Edges in the immutable frozen CSR.
    pub frozen_edges: usize,
    /// Edges in the mutable delta buffer, not yet merged into the frozen CSR.
    pub delta_edges: usize,
    /// Pending deletions not yet applied to the frozen CSR.
    pub tombstones: usize,
}

impl AdjacencyLayerStats {
    /// Whether reads of this index take the frozen-CSR fast path.
    #[inline]
    pub fn is_compacted(&self) -> bool {
        self.delta_edges == 0 && self.tombstones == 0
    }
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
            delta: DashMap::with_hasher(IdHashBuilder::default()),
            tombstones: DashMap::with_hasher(IdHashBuilder::default()),
            stats: AdjacencyStats {
                frozen_edge_count: AtomicUsize::new(frozen_edge_count),
                ..AdjacencyStats::new()
            },
            config,
            publish_window: AtomicBool::new(false),
            compaction_lock: Mutex::new(()),
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
        // Serialized against compaction (Issue #3810): a background compaction
        // publishing a CSR rebuilt from the pre-import layers would otherwise
        // overwrite the imported one.
        let _serialize = self.compaction_lock.lock();
        self.import_frozen_csr_locked(frozen_csr);
    }

    /// The import body. Callers must hold `compaction_lock`.
    fn import_frozen_csr_locked(&self, frozen_csr: Arc<AdjacencyIndex>) {
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
        // Hold the compaction lock across the check AND the import so a
        // concurrent compaction cannot empty the delta between them
        // (Issue #3810). `import_frozen_csr` re-locks; the lock is not
        // re-entrant, so the check is inlined here instead.
        let _serialize = self.compaction_lock.lock();
        let delta_count = self.delta.len();
        let tombstone_count = self.tombstones.len();

        if delta_count > 0 || tombstone_count > 0 {
            return Err(format!(
                "Cannot import: {} uncommitted delta edges and {} tombstones would be lost. \
                 Call compact() first or use import_frozen_csr() to force.",
                delta_count, tombstone_count
            ));
        }

        self.import_frozen_csr_locked(frozen_csr);
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

    /// Snapshot this index's layer occupancy (Issue #3810).
    ///
    /// The three counters are read independently and are not a consistent
    /// snapshot under concurrent writes -- they are progress/observability
    /// counters, not a transactional view.
    pub fn layer_stats(&self) -> AdjacencyLayerStats {
        AdjacencyLayerStats {
            frozen_edges: self.frozen_edge_count(),
            delta_edges: self.delta_edge_count(),
            tombstones: self.tombstone_count(),
        }
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
    ///
    /// # Read order is load-bearing (Issue #3810)
    ///
    /// Compaction moves edges from the delta buffer into the frozen CSR --
    /// two structures a reader observes with separate loads. The order of those
    /// loads is what keeps the pair consistent:
    ///
    /// - **Fast path: counters before frozen.** Compaction decrements the layer
    ///   counters only *after* publishing the new CSR, so observing zero here
    ///   (acquire) proves the CSR loaded next already contains everything the
    ///   delta held. Loading frozen first would let a reader pair a
    ///   pre-compaction CSR with a post-retire (empty) delta and return an
    ///   adjacency list missing every edge -- on a freshly built graph, an
    ///   **empty** one.
    /// - **Merged path: delta before frozen.** Same reasoning without the
    ///   counters: an entry missing from the delta read must already have been
    ///   retired, which happens strictly after the CSR carrying it was
    ///   published, so the CSR loaded afterwards has it. Taking the delta
    ///   reference first also pins that shard against retirement for as long as
    ///   the guard lives.
    /// - **Merged path: publish window after frozen.** The window flag is set
    ///   before the new CSR is published, so a reader holding the new CSR is
    ///   guaranteed to see the flag and de-duplicate the entries the CSR and
    ///   the delta momentarily share.
    pub fn get_adjacency(&self, node: NodeId) -> MergedAdjacencyGuard<'_> {
        // Fast path: if no delta and no tombstones globally, skip DashMap lookups.
        // Acquire (not relaxed): this is the synchronisation edge described
        // above, not just an optimisation hint.
        let delta_empty = self.stats.delta_edge_count.load(Ordering::Acquire) == 0;
        let tombstones_empty = self.stats.tombstone_count.load(Ordering::Acquire) == 0;

        if delta_empty && tombstones_empty {
            return MergedAdjacencyGuard {
                node,
                frozen: self.frozen.load(),
                delta: None,
                tombstones: &self.tombstones,
                publish_window: false,
                fast_path: true, // Skip per-edge tombstone checks
                tombstones_empty: true,
            };
        }

        let delta_guard = self.delta.get(&node);
        let frozen_guard = self.frozen.load();
        let publish_window = self.publish_window.load(Ordering::Acquire);

        MergedAdjacencyGuard {
            node,
            frozen: frozen_guard,
            delta: delta_guard,
            tombstones: &self.tombstones,
            publish_window,
            fast_path: false,
            // Delta may be non-empty (forcing the merged path below), but
            // tombstones can independently still be globally empty -- e.g.
            // a freshly-loaded or freshly-imported graph with fewer than
            // `max_delta_edges` uncompacted edges and zero deletes ever
            // issued. Tracking this separately from `fast_path` lets the
            // per-edge tombstone DashMap lookup in `iter()` be skipped in
            // that case instead of probing a provably-empty map on every
            // edge of every traversal.
            tombstones_empty,
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
    /// Merges frozen + delta (excluding tombstones) into a new CSR, publishes
    /// it, then retires the delta entries and tombstones that went into it.
    /// Readers stay lock-free throughout: they keep using the previous CSR
    /// until the new one is published, and never block on this call.
    ///
    /// # Publish order (Issue #3810)
    ///
    /// The order below is a correctness contract, not an implementation
    /// detail. Compaction moves edges between two structures a reader observes
    /// separately, so *some* window is unavoidable; the question is which one:
    ///
    /// - **Retire the delta first, publish the CSR second** (the original
    ///   order) leaves a window in which the delta no longer holds an edge and
    ///   the published CSR does not hold it yet. A reader landing there returns
    ///   an adjacency list **missing** those edges -- on a freshly built graph,
    ///   where every edge is still in the delta, it returns an **empty** list.
    ///   That window is as long as the O(E log E) rebuild.
    /// - **Publish the CSR first, retire the delta second** (this order) leaves
    ///   a window in which an edge is in *both* -- a duplicate, not a loss --
    ///   and a duplicate is filterable: [`merged_into_frozen`] names exactly
    ///   those edge ids for the length of the window, and readers filter them
    ///   the same way they filter tombstones.
    ///
    /// Retiring is therefore selective (only the entries this compaction
    /// merged), so writes that land mid-compaction stay in the delta rather
    /// than being dropped.
    ///
    /// Serialized with itself by `compaction_lock`: two concurrent compactions
    /// would each rebuild from the same frozen CSR while only one of them
    /// retired a given delta entry, and the later publish would drop the
    /// other's edges.
    pub fn compact(&self) {
        let _serialize = self.compaction_lock.lock();
        self.compact_locked();
    }

    /// Compact only if no other compaction is in flight. Returns whether this
    /// call performed the compaction.
    ///
    /// Used by the background maintenance worker (Issue #3810): when an
    /// explicit [`compact`](Self::compact) is already running the delta is
    /// already being drained, and queueing behind it would only delay the
    /// worker's other indexes.
    pub fn try_compact(&self) -> bool {
        match self.compaction_lock.try_lock() {
            Some(_serialize) => {
                self.compact_locked();
                true
            }
            None => false,
        }
    }

    /// The compaction body. Callers must hold `compaction_lock`.
    fn compact_locked(&self) {
        // Test-only panic injection for testing panic recovery (hidden from public API)
        if self.test_panic_on_compact.swap(false, Ordering::Relaxed) {
            panic!("Test-injected panic during compaction");
        }

        let frozen = self.frozen.load();

        // Memory usage note:
        // Compaction temporarily increases memory usage as we build the new frozen index
        // while the old one, delta, and tombstones are still in memory.
        // Peak memory ~ 2 * (frozen + delta).

        // 1. SNAPSHOT tombstones (copy, do not remove yet).
        //    Removing them here would resurrect a deleted edge for the length of
        //    the rebuild: the old CSR still holds it and nothing would filter it.
        let mut local_tombstones = HashSet::new();
        for entry in self.tombstones.iter() {
            local_tombstones.insert(*entry.key());
        }

        // 2. SNAPSHOT delta (copy, do not remove yet). Entries inserted after
        //    this snapshot are simply not part of this compaction and stay in
        //    the delta, visible to readers throughout.
        let delta_count_estimate = self.stats.delta_edge_count.load(Ordering::Relaxed);
        let mut local_delta = Vec::with_capacity(delta_count_estimate);
        for entry in self.delta.iter() {
            let source = *entry.key();
            for adj in entry.value().iter() {
                local_delta.push((source, *adj));
            }
        }

        // Estimate capacity: frozen + delta (we don't subtract tombstones to stay safe)
        let estimated_capacity = frozen.edge_count() + local_delta.len();
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

        // 4. Collect edges from the delta snapshot (filtering with local_tombstones)
        // Note: Delta might contain edges that were also tombstoned just before the snapshot
        for (source, adj) in &local_delta {
            if !local_tombstones.contains(&adj.edge_id) {
                all_edges.push((*source, adj.target, adj.edge_id, adj.label));
            }
        }

        // 5. Build new frozen CSR
        let new_frozen = AdjacencyIndex::build(all_edges);
        let new_edge_count = new_frozen.edge_count();

        // 6. Open the publish window BEFORE swapping the CSR in, so any reader
        //    that observes the new CSR also observes the instruction to
        //    de-duplicate the delta entries that CSR already contains.
        let window = !local_delta.is_empty();
        if window {
            self.publish_window.store(true, Ordering::Release);
        }

        // 7. Atomic swap (lock-free for readers!)
        self.frozen.store(Arc::new(new_frozen));
        self.stats
            .frozen_edge_count
            .store(new_edge_count, Ordering::Release);

        // 8. Retire exactly what this compaction merged. Selective, so a write
        //    that landed after the snapshot survives in the delta.
        let retired_delta = self.retire_delta(&local_delta);
        let retired_tombstones = self.retire_tombstones(&local_tombstones);

        // 9. Only now drop the counters. A reader that sees them at zero is
        //    guaranteed (release/acquire) to see the published CSR *and* a
        //    delta with nothing left to merge, which is what makes the
        //    `get_adjacency` / `frozen_view` fast path safe.
        if retired_delta > 0 {
            self.stats
                .delta_edge_count
                .fetch_sub(retired_delta, Ordering::Release);
        }
        if retired_tombstones > 0 {
            self.stats
                .tombstone_count
                .fetch_sub(retired_tombstones, Ordering::Release);
        }

        // 10. Close the publish window: the delta no longer holds anything the
        //     CSR also holds.
        if window {
            self.publish_window.store(false, Ordering::Release);
        }

        self.stats
            .last_compaction
            .store(Utc::now().timestamp() as u64, Ordering::Release);
    }

    /// Remove exactly the delta entries listed in `merged` (compaction step 8).
    ///
    /// Returns how many entries were actually removed, which is what the delta
    /// counter is decremented by -- never the snapshot's length, so a
    /// concurrently retried/duplicated removal can never underflow the counter.
    fn retire_delta(&self, merged: &[(NodeId, AdjacencyEntry)]) -> usize {
        let mut retired = 0usize;
        let mut per_node: Vec<(NodeId, HashSet<EdgeId, IdHashBuilder>)> = Vec::new();
        for (source, adj) in merged {
            match per_node.last_mut() {
                Some((node, ids)) if *node == *source => {
                    ids.insert(adj.edge_id);
                }
                _ => {
                    let mut ids = HashSet::with_hasher(IdHashBuilder::default());
                    ids.insert(adj.edge_id);
                    per_node.push((*source, ids));
                }
            }
        }

        for (source, ids) in per_node {
            // The `get_mut` guard is dropped before `remove_if`, which re-checks
            // emptiness under the shard write lock, so a concurrent insert can
            // never lose its entry (same pattern as the namespace index).
            let now_empty = match self.delta.get_mut(&source) {
                Some(mut entries) => {
                    let before = entries.len();
                    entries.retain(|entry| !ids.contains(&entry.edge_id));
                    retired += before - entries.len();
                    entries.is_empty()
                }
                None => false,
            };
            if now_empty {
                self.delta
                    .remove_if(&source, |_, entries| entries.is_empty());
            }
        }
        retired
    }

    /// Remove exactly the tombstones listed in `merged` (compaction step 8).
    ///
    /// Returns how many were actually removed.
    fn retire_tombstones(&self, merged: &HashSet<EdgeId>) -> usize {
        let mut retired = 0usize;
        for edge_id in merged {
            if self.tombstones.remove(edge_id).is_some() {
                retired += 1;
            }
        }
        retired
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
        // Only available when delta and tombstones are empty.
        //
        // Acquire, and strictly BEFORE loading `frozen`: compaction drops these
        // counters only after publishing the new CSR, so reading zero here is
        // what proves the CSR loaded below is not the pre-compaction one.
        // Loading `frozen` first would let this return a view missing every
        // edge a concurrent compaction had just retired from the delta
        // (Issue #3810).
        if self.stats.delta_edge_count.load(Ordering::Acquire) > 0 {
            return None;
        }
        if self.stats.tombstone_count.load(Ordering::Acquire) > 0 {
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
    node: NodeId,
    frozen: Guard<Arc<AdjacencyIndex>>,
    delta: Option<Ref<'a, NodeId, SmallVec<[AdjacencyEntry; 8]>>>,
    tombstones: &'a DashMap<EdgeId, Tombstone, IdHashBuilder>,
    /// Whether a compaction publish window was open when this guard was built
    /// (Issue #3810): the frozen CSR below may already contain entries the
    /// delta below still holds, so the delta half is de-duplicated against the
    /// frozen slice. False in the overwhelmingly common case -- see
    /// [`IncrementalAdjacencyIndex::compact`].
    publish_window: bool,
    /// True if both delta and tombstones are globally empty, allowing
    /// `fast_len()` to return the frozen count in O(1) without iterating.
    fast_path: bool,
    /// True if tombstones are globally empty, independent of `fast_path`
    /// (which additionally requires delta to be empty). Lets `iter()` skip
    /// the per-edge tombstone lookup even when the merged (non-frozen-view)
    /// path is taken because delta has pending edges.
    tombstones_empty: bool,
}

/// Whether `slice` (one node's frozen adjacency run) already contains `entry`.
///
/// Frozen runs are produced by [`AdjacencyIndex::build`], which sorts by
/// `(source, target, edge_id)`, so within one node's run the key
/// `(target, edge_id)` is sorted and this is a binary search. The CSR import
/// path preserves that order, so the invariant holds for persisted indexes too.
/// Should a run ever be unsorted, the search can only fail to find a present
/// entry (it never reports a match that is not there), so the worst case is a
/// duplicate inside a compaction publish window, never a lost edge.
#[inline]
fn frozen_slice_contains(slice: &[AdjacencyEntry], entry: &AdjacencyEntry) -> bool {
    slice
        .binary_search_by(|probe| (probe.target, probe.edge_id).cmp(&(entry.target, entry.edge_id)))
        .is_ok()
}

/// Two-variant iterator letting `MergedAdjacencyGuard::iter()` pick, once per
/// call, between an unfiltered chain (no per-edge tombstone check) and a
/// filtered one — instead of a single `Filter` adapter whose predicate is a
/// per-edge `tombstones_empty || ...` branch.
enum AdjacencyIter<U, F> {
    Unfiltered(U),
    Filtered(F),
}

impl<'a, U, F> Iterator for AdjacencyIter<U, F>
where
    U: Iterator<Item = &'a AdjacencyEntry>,
    F: Iterator<Item = &'a AdjacencyEntry>,
{
    type Item = &'a AdjacencyEntry;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            AdjacencyIter::Unfiltered(it) => it.next(),
            AdjacencyIter::Filtered(it) => it.next(),
        }
    }
}

impl<'a> MergedAdjacencyGuard<'a> {
    /// Iterate over all adjacency entries (frozen + delta, excluding tombstones).
    ///
    /// **Fast Path**: whenever tombstones are globally empty and no compaction
    /// publish window is open (even if delta is
    /// not — e.g. a graph with pending uncompacted edges but zero deletes ever
    /// issued), this returns the raw frozen+delta chain with no `Filter`
    /// adapter at all, rather than a `Filter` whose predicate is a
    /// provably-always-true `tombstones_empty || ...` branch. The choice is
    /// made once per call, outside the per-edge loop.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &AdjacencyEntry> + '_ {
        let frozen_slice = self.frozen.get_adjacency(self.node);
        let delta_slice = self.delta_slice();

        if self.tombstones_empty && !self.publish_window {
            return AdjacencyIter::Unfiltered(frozen_slice.iter().chain(delta_slice.iter()));
        }

        let tombstones = self.tombstones;
        let tombstones_empty = self.tombstones_empty;
        let publish_window = self.publish_window;

        let live_frozen = frozen_slice
            .iter()
            .filter(move |e| tombstones_empty || !tombstones.contains_key(&e.edge_id));
        // The delta half additionally drops entries the frozen slice already
        // carries: inside a compaction publish window the two layers overlap
        // for exactly the entries that were just absorbed.
        let live_delta = delta_slice.iter().filter(move |e| {
            (tombstones_empty || !tombstones.contains_key(&e.edge_id))
                && !(publish_window && frozen_slice_contains(frozen_slice, e))
        });

        AdjacencyIter::Filtered(live_frozen.chain(live_delta))
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
    ///
    /// A compaction publish window does not disqualify this: the window only
    /// duplicates entries that are in *both* layers, and this path is taken
    /// only when this node has no delta entries at all.
    #[inline]
    pub fn as_slice(&self) -> Option<&[AdjacencyEntry]> {
        if self.delta.is_none() && self.tombstones.is_empty() {
            Some(self.frozen.get_adjacency(self.node))
        } else {
            None
        }
    }

    /// Get the frozen adjacency slice for this node (O(log V), binary search over CSR node_ids).
    #[inline]
    pub fn frozen_slice(&self) -> &[AdjacencyEntry] {
        self.frozen.get_adjacency(self.node)
    }

    /// Get the delta adjacency slice for this node (O(1)).
    ///
    /// Returns an empty slice when no delta entries exist.
    #[inline]
    pub fn delta_slice(&self) -> &[AdjacencyEntry] {
        self.delta.as_ref().map(|d| d.as_slice()).unwrap_or(&[])
    }

    /// Check if an edge has been tombstoned (deleted) (O(1)).
    ///
    /// Returns `false` without a lookup when tombstones are known to be
    /// globally empty.
    #[inline]
    pub fn is_tombstoned(&self, edge_id: EdgeId) -> bool {
        !self.tombstones_empty && self.tombstones.contains_key(&edge_id)
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

    /// Two compactions running at once must not lose edges (Issue #3810).
    ///
    /// Each compaction snapshots `frozen`, drains the delta into a *local*
    /// buffer, and stores a rebuilt CSR. Without serialization, both snapshot
    /// the same frozen CSR while only one drains a given delta entry, and the
    /// later store silently drops the other's edges. Background maintenance
    /// makes this interleaving reachable in production (the worker can run
    /// while an explicit `compact_adjacency()` is in flight).
    #[test]
    fn concurrent_compaction_does_not_lose_edges() {
        use crate::core::interning::InternedString;
        use std::sync::atomic::AtomicBool;

        const EDGES: u64 = 4_000;

        let index = Arc::new(IncrementalAdjacencyIndex::new());
        let node = NodeId::new(1).unwrap();
        let done = Arc::new(AtomicBool::new(false));

        let writer = {
            let index = Arc::clone(&index);
            let done = Arc::clone(&done);
            thread::spawn(move || {
                for i in 1..=EDGES {
                    index.insert(
                        node,
                        AdjacencyEntry::new(
                            NodeId::new(i + 1).unwrap(),
                            EdgeId::new(i).unwrap(),
                            InternedString::from_raw(1),
                        ),
                    );
                }
                done.store(true, Ordering::SeqCst);
            })
        };

        let compactors: Vec<_> = (0..3)
            .map(|_| {
                let index = Arc::clone(&index);
                let done = Arc::clone(&done);
                thread::spawn(move || {
                    while !done.load(Ordering::SeqCst) {
                        index.compact();
                    }
                })
            })
            .collect();

        writer.join().unwrap();
        for c in compactors {
            c.join().unwrap();
        }
        index.compact();

        let guard = index.get_adjacency(node);
        let mut seen: Vec<u64> = guard.iter().map(|e| e.edge_id.as_u64()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len() as u64,
            EDGES,
            "concurrent compaction lost {} edges",
            EDGES - seen.len() as u64
        );
    }

    /// `try_compact` reports whether it did the work, and never blocks.
    #[test]
    fn try_compact_reports_whether_it_ran() {
        use crate::core::interning::InternedString;

        let index = IncrementalAdjacencyIndex::new();
        let node = NodeId::new(1).unwrap();
        index.insert(
            node,
            AdjacencyEntry::new(
                NodeId::new(2).unwrap(),
                EdgeId::new(1).unwrap(),
                InternedString::from_raw(1),
            ),
        );
        assert!(index.try_compact());
        assert_eq!(index.delta_edge_count(), 0);
        assert_eq!(index.frozen_edge_count(), 1);
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

    #[test]
    fn test_guard_frozen_slice() {
        use crate::core::interning::InternedString;
        let index = IncrementalAdjacencyIndex::new();
        let node = NodeId::new(1).unwrap();

        // Empty index: frozen_slice returns empty slice
        let guard = index.get_adjacency(node);
        assert!(guard.frozen_slice().is_empty());
        drop(guard);

        // Insert and compact so the entry lands in the frozen layer
        let edge = EdgeId::new(1).unwrap();
        let entry = AdjacencyEntry::new(NodeId::new(2).unwrap(), edge, InternedString::from_raw(1));
        index.insert(node, entry);
        index.compact();

        let guard = index.get_adjacency(node);
        assert_eq!(guard.frozen_slice().len(), 1);
        assert_eq!(guard.frozen_slice()[0].edge_id, edge);
    }

    #[test]
    fn test_guard_delta_slice() {
        use crate::core::interning::InternedString;
        let index = IncrementalAdjacencyIndex::new();
        let node = NodeId::new(1).unwrap();

        // No delta yet: delta_slice returns empty slice (unwrap_or path)
        let guard = index.get_adjacency(node);
        assert!(guard.delta_slice().is_empty());
        drop(guard);

        // After insert (before compact): delta_slice returns the entry
        let edge = EdgeId::new(1).unwrap();
        let entry = AdjacencyEntry::new(NodeId::new(2).unwrap(), edge, InternedString::from_raw(1));
        index.insert(node, entry);

        let guard = index.get_adjacency(node);
        assert_eq!(guard.delta_slice().len(), 1);
        assert_eq!(guard.delta_slice()[0].edge_id, edge);
    }

    #[test]
    fn test_guard_is_tombstoned() {
        use crate::core::interning::InternedString;
        let index = IncrementalAdjacencyIndex::new();
        let node = NodeId::new(1).unwrap();
        let edge = EdgeId::new(1).unwrap();
        let other_edge = EdgeId::new(2).unwrap();

        let entry = AdjacencyEntry::new(NodeId::new(2).unwrap(), edge, InternedString::from_raw(1));
        index.insert(node, entry);
        index.compact();

        // Fast path: no tombstones, is_tombstoned always returns false
        let guard = index.get_adjacency(node);
        assert!(!guard.is_tombstoned(edge));
        drop(guard);

        // Delete the edge: now is_tombstoned returns true for that edge
        index.delete(edge);

        let guard = index.get_adjacency(node);
        assert!(guard.is_tombstoned(edge));
        assert!(!guard.is_tombstoned(other_edge));
    }
}
