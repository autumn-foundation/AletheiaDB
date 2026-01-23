//! Temporal indexes for efficient time-based queries.
//!
//! This module implements a Timeline Index per entity, storing versions in sorted
//! vectors to enable efficient binary search and cache-friendly scanning.
//! It uses `DashMap` for fine-grained concurrency, allowing parallel writes to
//! different entities without global locking bottlenecks.
//!
//! # Complexity Analysis
//!
//! | Operation | Time Complexity | Space Complexity | Notes |
//! |-----------|----------------|------------------|-------|
//! | Insert (append) | O(1) amortized | O(N) per entity | Common case: chronological |
//! | Insert (retroactive) | O(N) | O(N) per entity | Binary search + shift |
//! | Batch insert | O(M log M + N) | O(N) per entity | M = batch size, N = total versions |
//! | Query (point) | O(log N + K) | O(K) | Binary search + scan K overlaps |
//! | Query (range) | O(log N + K) | O(K) | Same as point query |
//!
//! Where:
//! - **N** = number of versions per entity
//! - **M** = batch size for bulk inserts
//! - **K** = number of overlapping versions (typically 1-2 for point queries)
//!
//! # Concurrency & Performance Tradeoffs
//!
//! **Optimal Workload**: Chronological appends with writes to different entities.
//! DashMap provides excellent scalability for this common case.
//!
//! **Retroactive Insertions Under Contention**: When multiple threads insert
//! retroactively into the *same* entity, each insert requires O(N) vector shifting.
//! High contention can cause Vec reallocation thrashing. However, benchmarks show
//! this is acceptable in practice: 8 threads × 500 retroactive inserts (4000 total)
//! complete in <2 seconds with correct results.
//!
//! **Recommendation**: For extreme retroactive bulk loads (100K+ versions) to the
//! same entity, consider batching inserts per-thread and using `insert_batch()`
//! to amortize sorting cost.

use crate::core::id::{EdgeId, EntityId, NodeId, VersionId};
use crate::core::temporal::{BiTemporalInterval, TimeRange, Timestamp};
use crate::utils::error::{Result, StorageError};
use dashmap::DashMap;

/// Threshold for distinguishing point queries from range queries (in ticks).
/// Queries with range < POINT_QUERY_THRESHOLD are considered "point queries"
/// and pre-allocate less capacity (typically return 1-2 versions).
const POINT_QUERY_THRESHOLD_TICKS: i64 = 1000;

/// Configuration for temporal indexes.
#[derive(Debug, Clone)]
pub struct TemporalIndexConfig {
    /// Maximum versions allowed per entity (default: 1,000,000).
    /// Prevents OOM attacks from malicious or buggy clients creating
    /// unbounded version histories.
    pub max_versions_per_entity: usize,
}

impl Default for TemporalIndexConfig {
    fn default() -> Self {
        Self {
            max_versions_per_entity: 1_000_000,
        }
    }
}

/// Index into the consolidated version metadata storage.
///
/// This type represents a reference to version metadata stored centrally
/// in `EntityTimelines`, eliminating duplication between valid-time and
/// transaction-time indexes. Using `u32` saves 4 bytes per `TimelineEntry`
/// compared to storing `VersionId` (u64) directly.
///
/// # Memory Savings (Issue #196)
///
/// Previously, `VersionId` was stored in both valid and tx timelines,
/// causing 8 bytes of duplication per version. With consolidated storage:
/// - Timeline entries store a 4-byte index instead of 8-byte `VersionId`
/// - Version metadata is stored once in a central `Vec<VersionMetadata>`
/// - Net savings: 4 bytes per timeline entry × 2 timelines - 8 bytes metadata = 0 bytes
///   (but enables storing additional metadata without proportional cost increase)
pub type VersionMetadataIndex = u32;

/// Consolidated version metadata storage.
///
/// Stores version information in a single authoritative location,
/// eliminating duplication between valid-time and transaction-time indexes.
/// Both timelines reference this metadata via `VersionMetadataIndex`.
///
/// # Future Extensions
///
/// This structure can be extended to include additional metadata without
/// increasing storage per timeline entry:
/// - Entity ID (for cross-entity queries)
/// - BiTemporalInterval (for interval queries, see Issue #194)
/// - Provenance/audit information
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionMetadata {
    /// The unique identifier for this version.
    version_id: VersionId,
}

impl VersionMetadata {
    /// Create new version metadata.
    #[inline]
    pub const fn new(version_id: VersionId) -> Self {
        Self { version_id }
    }

    /// Get the version ID.
    #[inline]
    pub const fn version_id(&self) -> VersionId {
        self.version_id
    }
}

/// Entry in the timeline index.
///
/// Stores temporal bounds and a reference to version metadata.
/// The actual `VersionId` is stored in the consolidated `VersionMetadata`
/// storage, eliminating duplication between valid and tx timelines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimelineEntry {
    start: Timestamp,
    end: Timestamp,
    /// Index into the consolidated version metadata storage.
    metadata_idx: VersionMetadataIndex,
}

impl TimelineEntry {
    /// Get the metadata index for this entry.
    ///
    /// Used by tests to verify consolidated storage behavior.
    #[inline]
    #[cfg(test)]
    pub const fn metadata_index(&self) -> VersionMetadataIndex {
        self.metadata_idx
    }
}

/// Timeline for a specific entity.
#[derive(Debug, Clone, Default)]
struct EntityTimeline {
    /// Versions sorted by start time.
    versions: Vec<TimelineEntry>,
}

impl EntityTimeline {
    /// Insert a new version into the timeline, maintaining sorted order by start time.
    ///
    /// # Arguments
    ///
    /// * `start` - Start timestamp (inclusive)
    /// * `end` - End timestamp (exclusive)
    /// * `metadata_idx` - Index into the consolidated version metadata storage
    fn insert(&mut self, start: Timestamp, end: Timestamp, metadata_idx: VersionMetadataIndex) {
        let entry = TimelineEntry {
            start,
            end,
            metadata_idx,
        };

        // Optimization: if this version starts after the last one (common case), just push it.
        if self.versions.last().is_none_or(|last| last.start <= start) {
            self.versions.push(entry);
            return;
        }

        let idx = self.versions.partition_point(|e| e.start < start);
        self.versions.insert(idx, entry);
    }

    /// Insert multiple versions at once and sort. Efficient for large retroactive updates.
    ///
    /// # Performance
    /// - Use this for bulk inserts (retroactive history, migrations, recovery)
    /// - Single inserts via `insert()` are better for one-off updates
    /// - Timsort (Rust's default) is O(N) for already-sorted data,
    ///   O(N log N) worst case for unsorted retroactive updates
    /// - Deduplication prevents memory leaks from duplicate metadata indices
    /// - Pre-allocates capacity to avoid multiple reallocations during append
    ///
    /// # Deduplication Policy for Recovery
    ///
    /// After merging and sorting by `start` time, consecutive entries with duplicate
    /// `metadata_idx` are removed. The **first occurrence in the sorted vector** is kept,
    /// which corresponds to the version with the earliest `start` time.
    ///
    /// **Rationale**: This is correct for idempotent WAL replay. If a version is
    /// replayed multiple times, all replayed entries have identical start times, so
    /// keeping the first occurrence (arbitrary among identical entries) is safe.
    ///
    /// **Important**: This method assumes duplicate `metadata_idx` values represent
    /// the same logical version being inserted multiple times. If duplicates represent
    /// different versions (corrections), callers MUST use unique metadata indices or
    /// deduplicate before calling this method.
    ///
    /// **Future**: If use cases emerge requiring "latest-wins" semantics, we may add
    /// a `DeduplicationPolicy` enum. See `docs/DEDUPLICATION_POLICY.md` for analysis.
    fn insert_batch(&mut self, mut entries: Vec<TimelineEntry>) {
        if entries.is_empty() {
            return;
        }
        // Pre-allocate capacity for single reallocation during append.
        // Critical for bulk recovery/migration (10K+ versions per entity).
        self.versions.reserve(entries.len());
        self.versions.append(&mut entries);
        // Sort by start time. Timsort exploits existing order in the timeline.
        self.versions.sort_by_key(|e| e.start);
        // Deduplicate by metadata_idx to prevent memory leaks during recovery or bulk updates.
        // Keeps the first occurrence when duplicates exist.
        self.versions.dedup_by_key(|e| e.metadata_idx);
    }

    /// Find all metadata indices in this timeline that overlap with the given time range.
    ///
    /// Returns indices into the consolidated version metadata storage.
    /// Callers must resolve these indices to `VersionId` using `EntityTimelines::get_version_metadata`.
    fn find_indices_in_range(&self, range: TimeRange) -> Vec<VersionMetadataIndex> {
        // Find versions starting before the query range ends.
        let cutoff = self.versions.partition_point(|e| e.start < range.end());

        // Adaptive pre-allocation heuristic:
        // - Point/small range queries (< POINT_QUERY_THRESHOLD_TICKS) typically return 1-2 versions → cap at 4
        // - Large range queries can return many versions → cap at 16
        // Phase 2: Use wallclock components for arithmetic
        let range_size = range.end().wallclock() - range.start().wallclock();
        let estimated_capacity = if range_size < POINT_QUERY_THRESHOLD_TICKS {
            cutoff.min(4) // Point query or small range
        } else {
            cutoff.min(16) // Large range query
        };
        let mut results = Vec::with_capacity(estimated_capacity);
        for entry in &self.versions[..cutoff] {
            if entry.end > range.start() {
                results.push(entry.metadata_idx);
            }
        }

        results
    }

    /// Find all metadata indices that contain a specific point in time.
    ///
    /// A version [start, end) contains timestamp T if: start <= T < end
    ///
    /// Returns indices into the consolidated version metadata storage.
    /// Callers must resolve these indices to `VersionId` using `EntityTimelines::get_version_metadata`.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(log N + K) where N = versions, K = overlapping versions
    /// - For typical bi-temporal databases with non-overlapping intervals, K = 1
    fn find_indices_at_point(&self, timestamp: Timestamp) -> Vec<VersionMetadataIndex> {
        // Find all entries where start <= timestamp (these could potentially contain T)
        let cutoff = self.versions.partition_point(|e| e.start <= timestamp);

        // Pre-allocate for typical case (1-2 versions at a point)
        let mut results = Vec::with_capacity(cutoff.min(4));

        // Filter to entries where end > timestamp (completing the containment check)
        for entry in &self.versions[..cutoff] {
            if entry.end > timestamp {
                results.push(entry.metadata_idx);
            }
        }

        results
    }
}

/// Grouped timelines for valid and transaction dimensions.
///
/// # Consolidated Version Metadata Storage (Issue #196)
///
/// This structure implements a centralized version metadata storage that
/// eliminates duplication between valid-time and transaction-time indexes.
/// Instead of storing `VersionId` directly in each `TimelineEntry`, entries
/// store a `VersionMetadataIndex` that references the consolidated storage.
///
/// ## Memory Layout
///
/// ```text
/// EntityTimelines {
///     version_metadata: [V0, V1, V2, ...]  // Consolidated storage (8 bytes each)
///     valid:  [Entry(start, end, idx=0), Entry(start, end, idx=1), ...]
///     tx:     [Entry(start, end, idx=0), Entry(start, end, idx=1), ...]
/// }
/// ```
///
/// Both `valid` and `tx` timelines reference the same metadata via index,
/// eliminating the need to store `VersionId` twice per version.
#[derive(Debug, Clone, Default)]
struct EntityTimelines {
    /// Consolidated version metadata storage.
    /// Both valid and tx timelines reference this via `VersionMetadataIndex`.
    version_metadata: Vec<VersionMetadata>,
    /// Valid-time timeline index.
    valid: EntityTimeline,
    /// Transaction-time timeline index.
    tx: EntityTimeline,
}

impl EntityTimelines {
    /// Get the number of unique versions stored (not duplicated).
    #[inline]
    pub fn version_metadata_count(&self) -> usize {
        self.version_metadata.len()
    }

    /// Get version metadata by index.
    ///
    /// Returns `None` if the index is out of bounds.
    /// Used by tests to verify consolidated storage behavior.
    #[inline]
    #[cfg(test)]
    pub fn get_version_metadata(&self, index: VersionMetadataIndex) -> Option<&VersionMetadata> {
        self.version_metadata.get(index as usize)
    }

    /// Add new version metadata and return its index.
    ///
    /// # Panics
    ///
    /// Panics if the number of versions exceeds `u32::MAX`.
    #[inline]
    fn add_version_metadata(&mut self, metadata: VersionMetadata) -> VersionMetadataIndex {
        let index = self.version_metadata.len();
        debug_assert!(
            index <= u32::MAX as usize,
            "Too many versions for u32 index"
        );
        self.version_metadata.push(metadata);
        index as VersionMetadataIndex
    }

    /// Resolve a metadata index to a `VersionId`.
    ///
    /// # Panics
    ///
    /// Panics in debug mode if the index is out of bounds.
    #[inline]
    fn resolve_version_id(&self, index: VersionMetadataIndex) -> VersionId {
        debug_assert!(
            (index as usize) < self.version_metadata.len(),
            "Invalid metadata index"
        );
        self.version_metadata[index as usize].version_id()
    }

    /// Resolve multiple metadata indices to `VersionId`s.
    #[inline]
    fn resolve_version_ids(&self, indices: Vec<VersionMetadataIndex>) -> Vec<VersionId> {
        indices
            .into_iter()
            .map(|idx| self.resolve_version_id(idx))
            .collect()
    }
}

/// Temporal indexes for efficient time-based lookups.
///
/// This implementation uses a per-entity timeline index with sorted vectors,
/// providing O(log N) lookup and cache-friendly scanning. It leverages `DashMap`
/// for fine-grained concurrency, avoiding global bottlenecks during writes.
///
/// # Concurrency Design
///
/// The current implementation uses `DashMap<EntityId, EntityTimelines>` directly.
/// An alternative considered was `DashMap<EntityId, Arc<RwLock<EntityTimelines>>>`,
/// which would allow multiple concurrent readers of the same entity.
///
/// **Decision**: We chose the current direct storage approach because:
///
/// 1. **DashMap already provides concurrent reads**: DashMap uses shard-level `RwLock`
///    internally, so multiple readers can access different entities in the same shard
///    concurrently without blocking each other.
///
/// 2. **Memory efficiency**: Direct storage avoids per-entity `Arc` and `RwLock`
///    allocations, reducing memory overhead significantly (especially for databases
///    with millions of entities).
///
/// 3. **API simplicity**: Direct access via `.get()` is simpler than nested locking
///    with `.get()?.read()`.
///
/// 4. **Rare benefit**: The scenario where per-entity RwLock helps (many threads
///    reading the exact same entity simultaneously) is uncommon. Most workloads
///    access different entities or different shards.
///
/// **When to revisit**: If profiling shows >50% of queries targeting the same entity
/// with >8 concurrent readers causing measurable contention, consider the RwLock pattern.
/// See `docs/RWLOCK_ANALYSIS.md` for detailed analysis.
#[derive(Debug)]
pub struct TemporalIndexes {
    /// Combined index for both valid and transaction timelines.
    index: DashMap<EntityId, EntityTimelines>,
    /// Configuration for temporal indexes.
    config: TemporalIndexConfig,
}

impl Default for TemporalIndexes {
    fn default() -> Self {
        Self::new()
    }
}

impl TemporalIndexes {
    /// Create a new empty temporal index with default configuration.
    pub fn new() -> Self {
        Self::with_config(TemporalIndexConfig::default())
    }

    /// Create a new empty temporal index with custom configuration.
    pub fn with_config(config: TemporalIndexConfig) -> Self {
        Self {
            index: DashMap::new(),
            config,
        }
    }

    /// Insert a node version into the temporal indexes.
    ///
    /// Returns an error if the entity exceeds the configured version limit.
    ///
    /// # Performance Notes
    ///
    /// Retroactive inserts (inserting versions out of chronological order) require
    /// O(N) vector shifting to maintain sorted order. Under high contention to the
    /// same entity, consider using `insert_node_versions_batch()` to amortize
    /// sorting costs across multiple versions.
    pub fn insert_node_version(
        &self,
        node_id: NodeId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
    ) -> Result<()> {
        self.insert_version(EntityId::Node(node_id), version_id, temporal)
    }

    /// Insert an edge version into the temporal indexes.
    ///
    /// Returns an error if the entity exceeds the configured version limit.
    ///
    /// # Performance Notes
    ///
    /// Retroactive inserts (inserting versions out of chronological order) require
    /// O(N) vector shifting to maintain sorted order. Under high contention to the
    /// same entity, consider using `insert_edge_versions_batch()` to amortize
    /// sorting costs across multiple versions.
    pub fn insert_edge_version(
        &self,
        edge_id: EdgeId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
    ) -> Result<()> {
        self.insert_version(EntityId::Edge(edge_id), version_id, temporal)
    }

    /// Insert multiple node versions into the temporal indexes efficiently.
    ///
    /// Returns an error if the entity exceeds the configured version limit.
    pub fn insert_node_versions_batch(
        &self,
        node_id: NodeId,
        versions: Vec<(VersionId, BiTemporalInterval)>,
    ) -> Result<()> {
        self.insert_versions_batch(EntityId::Node(node_id), versions)
    }

    /// Insert multiple edge versions into the temporal indexes efficiently.
    ///
    /// Returns an error if the entity exceeds the configured version limit.
    pub fn insert_edge_versions_batch(
        &self,
        edge_id: EdgeId,
        versions: Vec<(VersionId, BiTemporalInterval)>,
    ) -> Result<()> {
        self.insert_versions_batch(EntityId::Edge(edge_id), versions)
    }

    /// Insert a version into both temporal indexes.
    ///
    /// Version metadata is stored once in the consolidated storage, and both
    /// valid-time and transaction-time indexes reference it via index.
    fn insert_version(
        &self,
        entity_id: EntityId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
    ) -> Result<()> {
        let mut timelines = self.index.entry(entity_id).or_default();

        // Check version limit before inserting (DoS protection)
        // Use version_metadata count as the authoritative source
        let current_count = timelines.version_metadata_count();
        if current_count >= self.config.max_versions_per_entity {
            return Err(StorageError::CapacityExceeded {
                resource: format!("versions for entity {:?}", entity_id),
                current: current_count,
                limit: self.config.max_versions_per_entity,
            }
            .into());
        }

        // Store version metadata once in consolidated storage
        let metadata = VersionMetadata::new(version_id);
        let metadata_idx = timelines.add_version_metadata(metadata);

        // Both timelines reference the same metadata via index
        let valid = temporal.valid_time();
        timelines
            .valid
            .insert(valid.start(), valid.end(), metadata_idx);

        let tx = temporal.transaction_time();
        timelines.tx.insert(tx.start(), tx.end(), metadata_idx);

        Ok(())
    }

    /// Helper for batch insertion of versions.
    ///
    /// Version metadata is stored once in the consolidated storage, and both
    /// valid-time and transaction-time indexes reference it via index.
    fn insert_versions_batch(
        &self,
        entity_id: EntityId,
        versions: Vec<(VersionId, BiTemporalInterval)>,
    ) -> Result<()> {
        if versions.is_empty() {
            return Ok(());
        }

        let mut timelines = self.index.entry(entity_id).or_default();

        // Check version limit before batch insert (DoS protection)
        // Use version_metadata count as the authoritative source
        let current_count = timelines.version_metadata_count();
        let new_count = current_count + versions.len();
        if new_count > self.config.max_versions_per_entity {
            return Err(StorageError::CapacityExceeded {
                resource: format!("versions for entity {:?}", entity_id),
                current: new_count,
                limit: self.config.max_versions_per_entity,
            }
            .into());
        }

        // Pre-allocate capacity for metadata storage
        timelines.version_metadata.reserve(versions.len());

        let mut valid_entries = Vec::with_capacity(versions.len());
        let mut tx_entries = Vec::with_capacity(versions.len());

        for (v_id, temporal) in versions {
            // Store version metadata once in consolidated storage
            let metadata = VersionMetadata::new(v_id);
            let metadata_idx = timelines.add_version_metadata(metadata);

            let valid = temporal.valid_time();
            let tx = temporal.transaction_time();

            // Both timeline entries reference the same metadata via index
            valid_entries.push(TimelineEntry {
                start: valid.start(),
                end: valid.end(),
                metadata_idx,
            });
            tx_entries.push(TimelineEntry {
                start: tx.start(),
                end: tx.end(),
                metadata_idx,
            });
        }

        timelines.valid.insert_batch(valid_entries);
        timelines.tx.insert_batch(tx_entries);

        Ok(())
    }

    /// Find all node versions that overlap with the given valid time range.
    pub fn find_node_versions_in_valid_time_range(
        &self,
        node_id: NodeId,
        time_range: TimeRange,
    ) -> Vec<VersionId> {
        self.index
            .get(&EntityId::Node(node_id))
            .map(|t| {
                let indices = t.valid.find_indices_in_range(time_range);
                t.resolve_version_ids(indices)
            })
            .unwrap_or_default()
    }

    /// Find all edge versions that overlap with the given valid time range.
    pub fn find_edge_versions_in_valid_time_range(
        &self,
        edge_id: EdgeId,
        time_range: TimeRange,
    ) -> Vec<VersionId> {
        self.index
            .get(&EntityId::Edge(edge_id))
            .map(|t| {
                let indices = t.valid.find_indices_in_range(time_range);
                t.resolve_version_ids(indices)
            })
            .unwrap_or_default()
    }

    /// Find all node versions recorded in the given transaction time range.
    pub fn find_node_versions_in_transaction_time_range(
        &self,
        node_id: NodeId,
        time_range: TimeRange,
    ) -> Vec<VersionId> {
        self.index
            .get(&EntityId::Node(node_id))
            .map(|t| {
                let indices = t.tx.find_indices_in_range(time_range);
                t.resolve_version_ids(indices)
            })
            .unwrap_or_default()
    }

    /// Find all edge versions recorded in the given transaction time range.
    pub fn find_edge_versions_in_transaction_time_range(
        &self,
        edge_id: EdgeId,
        time_range: TimeRange,
    ) -> Vec<VersionId> {
        self.index
            .get(&EntityId::Edge(edge_id))
            .map(|t| {
                let indices = t.tx.find_indices_in_range(time_range);
                t.resolve_version_ids(indices)
            })
            .unwrap_or_default()
    }

    /// Find node versions visible at a specific bi-temporal point.
    ///
    /// This is the efficient O(log n) replacement for the linear scan in
    /// `HistoricalStorage::find_node_version_at_time`. It queries both
    /// the valid time and transaction time indexes, returning only versions
    /// that are visible at BOTH temporal coordinates.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(log N + K) where N = versions per entity, K = overlapping versions
    /// - For point queries, K is typically 1-2 versions
    /// - This replaces O(N) linear scan through version chains
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node to query
    /// * `valid_time` - The valid time coordinate (when the fact was true in reality)
    /// * `transaction_time` - The transaction time coordinate (when the fact was recorded)
    ///
    /// # Returns
    ///
    /// Version IDs visible at the given bi-temporal point. For typical bi-temporal
    /// databases with non-overlapping intervals, this returns 0-1 versions.
    pub fn find_node_version_at_point(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Vec<VersionId> {
        self.find_version_at_point_impl(EntityId::Node(node_id), valid_time, transaction_time)
    }

    /// Find edge versions visible at a specific bi-temporal point.
    ///
    /// This is the efficient O(log n) replacement for the linear scan in
    /// `HistoricalStorage::find_edge_version_at_time`. It queries both
    /// the valid time and transaction time indexes, returning only versions
    /// that are visible at BOTH temporal coordinates.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(log N + K) where N = versions per entity, K = overlapping versions
    /// - For point queries, K is typically 1-2 versions
    /// - This replaces O(N) linear scan through version chains
    ///
    /// # Arguments
    ///
    /// * `edge_id` - The edge to query
    /// * `valid_time` - The valid time coordinate (when the fact was true in reality)
    /// * `transaction_time` - The transaction time coordinate (when the fact was recorded)
    ///
    /// # Returns
    ///
    /// Version IDs visible at the given bi-temporal point. For typical bi-temporal
    /// databases with non-overlapping intervals, this returns 0-1 versions.
    pub fn find_edge_version_at_point(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Vec<VersionId> {
        self.find_version_at_point_impl(EntityId::Edge(edge_id), valid_time, transaction_time)
    }

    /// Internal implementation for bi-temporal point queries.
    ///
    /// Shared logic for both node and edge lookups to avoid code duplication.
    fn find_version_at_point_impl(
        &self,
        entity_id: EntityId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Vec<VersionId> {
        let Some(timelines) = self.index.get(&entity_id) else {
            return Vec::new();
        };

        // Query both temporal dimensions with point-in-time queries
        // Using find_indices_at_point for correct boundary handling (start <= T < end)
        let valid_indices = timelines.valid.find_indices_at_point(valid_time);
        let tx_indices = timelines.tx.find_indices_at_point(transaction_time);

        // Intersect metadata indices: version must be visible in BOTH dimensions
        let intersected_indices = Self::intersect_metadata_indices(valid_indices, tx_indices);

        // Resolve intersected indices to VersionIds
        timelines.resolve_version_ids(intersected_indices)
    }

    /// Efficiently intersect two sets of metadata indices.
    ///
    /// Uses linear intersection for small sets (K < threshold) and HashSet
    /// for larger sets to avoid O(K²) complexity when K is large.
    ///
    /// # Performance
    ///
    /// - Small K (< 16): O(K²) but with low constant factor
    /// - Large K (>= 16): O(K) using HashSet
    fn intersect_metadata_indices(
        a: Vec<VersionMetadataIndex>,
        b: Vec<VersionMetadataIndex>,
    ) -> Vec<VersionMetadataIndex> {
        // Threshold for switching to HashSet-based intersection.
        // Below this, linear scan is faster due to cache locality and no allocation.
        const HASH_THRESHOLD: usize = 16;

        let max_len = a.len().max(b.len());

        if max_len < HASH_THRESHOLD {
            // Small sets: linear intersection is efficient due to cache locality
            if a.len() <= b.len() {
                a.into_iter().filter(|v| b.contains(v)).collect()
            } else {
                b.into_iter().filter(|v| a.contains(v)).collect()
            }
        } else {
            // Large sets: use HashSet for O(K) intersection instead of O(K²)
            use std::collections::HashSet;
            let b_set: HashSet<_> = b.into_iter().collect();
            a.into_iter().filter(|v| b_set.contains(v)).collect()
        }
    }

    /// Get the total number of indexed version entries.
    ///
    /// Returns the count of unique versions across all entities.
    /// With consolidated storage, this equals the total metadata entries,
    /// not the sum of timeline entries (which would double-count).
    pub fn version_count(&self) -> usize {
        self.index
            .iter()
            .map(|entry| entry.value().version_metadata_count())
            .sum()
    }

    /// Clear all indexes.
    pub fn clear(&self) {
        self.index.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX};

    #[test]
    fn test_insert_and_find_node_versions() {
        let indexes = TemporalIndexes::new();

        let node_id = NodeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();
        let v3 = VersionId::new(102).unwrap();

        // v1: [0, 1000)
        indexes
            .insert_node_version(
                node_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), 1000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // v2: [1000, 2000)
        indexes
            .insert_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 2000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // v3: [2000, 3000)
        indexes
            .insert_node_version(
                node_id,
                v3,
                BiTemporalInterval::new(
                    TimeRange::new(2000.into(), 3000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // Test overlap logic: Query [500, 1500)
        // v1 overlaps (500 to 1000)
        // v2 overlaps (1000 to 1500)
        let results = indexes.find_node_versions_in_valid_time_range(
            node_id,
            TimeRange::new(500.into(), 1500.into()).unwrap(),
        );

        assert_eq!(results.len(), 2);
        assert!(results.contains(&v1));
        assert!(results.contains(&v2));
        assert!(!results.contains(&v3));
    }

    #[test]
    fn test_edge_cases_overlap() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();

        // Overlapping intervals (should be possible in valid time)
        let v1 = VersionId::new(1).unwrap();
        let v2 = VersionId::new(2).unwrap();
        indexes
            .insert_node_version(
                node_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), 2000.into()).unwrap(),
                    TimeRange::from(0.into()),
                ),
            )
            .unwrap();
        indexes
            .insert_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 3000.into()).unwrap(),
                    TimeRange::from(0.into()),
                ),
            )
            .unwrap();

        // Query point at 1500 (both should match)
        let results =
            indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::at(1500.into()));
        assert_eq!(results.len(), 2);
        assert!(results.contains(&v1));
        assert!(results.contains(&v2));

        // Query point at 500 (only v1)
        let results =
            indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::at(500.into()));
        assert_eq!(results.len(), 1);
        assert!(results.contains(&v1));

        // Query point at 2500 (only v2)
        let results =
            indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::at(2500.into()));
        assert_eq!(results.len(), 1);
        assert!(results.contains(&v2));
    }

    #[test]
    fn test_adjacent_intervals() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();

        let v1 = VersionId::new(1).unwrap();
        let v2 = VersionId::new(2).unwrap();
        // v1: [0, 1000), v2: [1000, 2000)
        indexes
            .insert_node_version(
                node_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), 1000.into()).unwrap(),
                    TimeRange::from(0.into()),
                ),
            )
            .unwrap();
        indexes
            .insert_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 2000.into()).unwrap(),
                    TimeRange::from(0.into()),
                ),
            )
            .unwrap();

        // Query point at 1000 (only v2 because [start, end) is inclusive-exclusive)
        // Use [1000, 1001) to represent the point 1000
        let results = indexes.find_node_versions_in_valid_time_range(
            node_id,
            TimeRange::new(1000.into(), 1001.into()).unwrap(),
        );
        assert_eq!(results.len(), 1);
        assert!(results.contains(&v2));

        // Query range [500, 1000) (only v1 because 1000 is exclusive)
        let results = indexes.find_node_versions_in_valid_time_range(
            node_id,
            TimeRange::new(500.into(), 1000.into()).unwrap(),
        );
        assert_eq!(results.len(), 1);
        assert!(results.contains(&v1));
    }

    #[test]
    fn test_batch_insertion() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();

        let versions = vec![
            (
                VersionId::new(1).unwrap(),
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), 10.into()).unwrap(),
                    TimeRange::from(0.into()),
                ),
            ),
            (
                VersionId::new(3).unwrap(),
                BiTemporalInterval::new(
                    TimeRange::new(20.into(), 30.into()).unwrap(),
                    TimeRange::from(0.into()),
                ),
            ),
            (
                VersionId::new(2).unwrap(),
                BiTemporalInterval::new(
                    TimeRange::new(10.into(), 20.into()).unwrap(),
                    TimeRange::from(0.into()),
                ),
            ),
        ];

        indexes
            .insert_node_versions_batch(node_id, versions)
            .unwrap();

        let results = indexes.find_node_versions_in_valid_time_range(
            node_id,
            TimeRange::new(5.into(), 25.into()).unwrap(),
        );
        assert_eq!(results.len(), 3);

        // Verify sort order internally (though opaque to API)
        let timelines = indexes.index.get(&EntityId::Node(node_id)).unwrap();
        assert_eq!(timelines.valid.versions[0].start, 0.into());
        assert_eq!(timelines.valid.versions[1].start, 10.into());
        assert_eq!(timelines.valid.versions[2].start, 20.into());
    }

    #[test]
    fn test_transaction_time_range_query() {
        let indexes = TemporalIndexes::new();

        let edge_id = EdgeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();

        // v1: tx [1000, MAX)
        indexes
            .insert_edge_version(
                edge_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                    TimeRange::new(1000.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // v2: tx [2000, MAX)
        indexes
            .insert_edge_version(
                edge_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                    TimeRange::new(2000.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // Query: [1500, 2500)
        let results = indexes.find_edge_versions_in_transaction_time_range(
            edge_id,
            TimeRange::new(1500.into(), 2500.into()).unwrap(),
        );

        assert_eq!(results.len(), 2);
        assert!(results.contains(&v1));
        assert!(results.contains(&v2));
    }

    #[test]
    fn test_multiple_entities() {
        let indexes = TemporalIndexes::new();

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();

        indexes
            .insert_node_version(node1, v1, BiTemporalInterval::current(1000.into()))
            .unwrap();
        indexes
            .insert_node_version(node2, v2, BiTemporalInterval::current(1000.into()))
            .unwrap();

        let results = indexes.find_node_versions_in_valid_time_range(
            node1,
            TimeRange::new(0.into(), 2000.into()).unwrap(),
        );

        assert_eq!(results.len(), 1);
        assert!(results.contains(&v1));
        assert!(!results.contains(&v2));
    }

    #[test]
    fn test_clear() {
        let indexes = TemporalIndexes::new();

        indexes
            .insert_node_version(
                NodeId::new(1).unwrap(),
                VersionId::new(100).unwrap(),
                BiTemporalInterval::current(1000.into()),
            )
            .unwrap();

        assert!(indexes.version_count() > 0);

        indexes.clear();

        assert_eq!(indexes.version_count(), 0);
    }

    #[test]
    fn test_empty_timeline_query() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();

        // Query an entity with no versions
        let results = indexes.find_node_versions_in_valid_time_range(
            node_id,
            TimeRange::new(0.into(), 1000.into()).unwrap(),
        );

        assert_eq!(results.len(), 0, "Empty timeline should return no results");
    }

    #[test]
    fn test_retroactive_single_inserts() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();
        let v3 = VersionId::new(102).unwrap();

        // Insert in non-chronological order to test retroactive insertion
        // v1 at t=0
        indexes
            .insert_node_version(
                node_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), 1000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // v3 at t=20000 (far future)
        indexes
            .insert_node_version(
                node_id,
                v3,
                BiTemporalInterval::new(
                    TimeRange::new(20000.into(), 21000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // v2 at t=10000 (retroactive - inserted between v1 and v3)
        indexes
            .insert_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(10000.into(), 11000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // Verify correct sort order is maintained
        let entity_id = EntityId::Node(node_id);
        let timelines = indexes.index.get(&entity_id).unwrap();

        assert_eq!(timelines.valid.versions.len(), 3, "Should have 3 versions");
        assert_eq!(
            timelines.valid.versions[0].start,
            0.into(),
            "First version should start at 0"
        );
        // Verify version via metadata resolution
        let idx0 = timelines.valid.versions[0].metadata_index();
        assert_eq!(
            timelines.get_version_metadata(idx0).unwrap().version_id(),
            v1,
            "First version should be v1"
        );
        assert_eq!(
            timelines.valid.versions[1].start,
            10000.into(),
            "Second version should start at 10000"
        );
        let idx1 = timelines.valid.versions[1].metadata_index();
        assert_eq!(
            timelines.get_version_metadata(idx1).unwrap().version_id(),
            v2,
            "Second version should be v2"
        );
        assert_eq!(
            timelines.valid.versions[2].start,
            20000.into(),
            "Third version should start at 20000"
        );
        let idx2 = timelines.valid.versions[2].metadata_index();
        assert_eq!(
            timelines.get_version_metadata(idx2).unwrap().version_id(),
            v3,
            "Third version should be v3"
        );

        // Verify queries work correctly with retroactively inserted versions
        let results = indexes.find_node_versions_in_valid_time_range(
            node_id,
            TimeRange::new(9000.into(), 11000.into()).unwrap(),
        );
        assert_eq!(results.len(), 1, "Should find 1 version in range");
        assert_eq!(results[0], v2, "Should find v2 in the middle");
    }

    #[test]
    fn test_batch_insert_deduplication() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();

        // Insert v1 normally (gets metadata_idx = 0)
        indexes
            .insert_node_version(
                node_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), 1000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // Insert v2 normally (gets metadata_idx = 1)
        indexes
            .insert_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 2000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // Simulate recovery scenario: batch insert including duplicate metadata_idx=0 (with different timing)
        // This would cause memory leak without deduplication
        let mut timelines = indexes.index.get_mut(&EntityId::Node(node_id)).unwrap();

        // Add a new metadata entry for the new version (idx=2)
        let v3 = VersionId::new(102).unwrap();
        let new_metadata_idx = timelines.add_version_metadata(VersionMetadata::new(v3));

        let duplicate_entries = vec![
            TimelineEntry {
                start: 0.into(),
                end: 1500.into(), // Different end time (recovery scenario)
                metadata_idx: 0,  // Duplicate of the first entry
            },
            TimelineEntry {
                start: 2000.into(),
                end: 3000.into(),
                metadata_idx: new_metadata_idx,
            },
        ];
        timelines.valid.insert_batch(duplicate_entries);

        // Verify deduplication worked - should have 3 unique entries, not 4
        assert_eq!(
            timelines.valid.versions.len(),
            3,
            "Deduplication should remove duplicate entry with metadata_idx=0"
        );

        // Verify metadata_idx=0 appears only once
        let idx0_count = timelines
            .valid
            .versions
            .iter()
            .filter(|e| e.metadata_idx == 0)
            .count();
        assert_eq!(
            idx0_count, 1,
            "metadata_idx=0 should appear exactly once after dedup"
        );

        // Verify the kept entry with metadata_idx=0 has the first data (end=1000, not end=1500)
        // dedup_by_key keeps the first occurrence
        let idx0_entry = timelines
            .valid
            .versions
            .iter()
            .find(|e| e.metadata_idx == 0)
            .unwrap();
        assert_eq!(
            idx0_entry.end,
            1000.into(),
            "Should keep first occurrence (end=1000)"
        );
    }

    #[test]
    fn test_concurrent_entity_writes() {
        use std::sync::Arc;
        use std::thread;

        let indexes = Arc::new(TemporalIndexes::new());
        let num_threads = 10;
        let versions_per_thread = 1000;

        // Spawn multiple threads writing to different entities
        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let idx = Arc::clone(&indexes);
                thread::spawn(move || {
                    let node_id = NodeId::new(thread_id + 1).unwrap();
                    for v in 0..versions_per_thread {
                        let version_id =
                            VersionId::new(thread_id * versions_per_thread + v).unwrap();
                        idx.insert_node_version(
                            node_id,
                            version_id,
                            BiTemporalInterval::new(
                                TimeRange::new(
                                    ((v * 1000) as i64).into(),
                                    (((v + 1) * 1000) as i64).into(),
                                )
                                .unwrap(),
                                TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                            ),
                        )
                        .unwrap();
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for h in handles {
            h.join().unwrap();
        }

        // Verify all versions were indexed correctly
        for thread_id in 0..num_threads {
            let node_id = NodeId::new(thread_id + 1).unwrap();
            let results = indexes.find_node_versions_in_valid_time_range(
                node_id,
                TimeRange::new(0.into(), ((versions_per_thread * 1000) as i64).into()).unwrap(),
            );
            assert_eq!(
                results.len(),
                versions_per_thread as usize,
                "Thread {} should have {} versions",
                thread_id,
                versions_per_thread
            );
        }

        // Verify total version count (only counts valid timeline)
        assert_eq!(
            indexes.version_count(),
            (num_threads * versions_per_thread) as usize,
            "Total version count should match number of inserts"
        );
    }

    #[test]
    fn test_concurrent_same_entity_contention() {
        use std::sync::Arc;
        use std::thread;
        use std::time::Instant;

        let indexes = Arc::new(TemporalIndexes::new());
        let node_id = NodeId::new(1).unwrap(); // Same entity for all threads
        let num_threads = 8;
        let versions_per_thread = 500;

        let start = Instant::now();

        // Multiple threads writing to THE SAME entity - tests DashMap lock contention
        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let idx = Arc::clone(&indexes);
                thread::spawn(move || {
                    for v in 0..versions_per_thread {
                        let version_id =
                            VersionId::new(thread_id * versions_per_thread + v).unwrap();
                        idx.insert_node_version(
                            node_id, // Same entity!
                            version_id,
                            BiTemporalInterval::new(
                                TimeRange::new(
                                    (((thread_id * versions_per_thread + v) * 100) as i64).into(),
                                    ((((thread_id * versions_per_thread + v) + 1) * 100) as i64)
                                        .into(),
                                )
                                .unwrap(),
                                TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                            ),
                        )
                        .unwrap();
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for h in handles {
            h.join().unwrap();
        }

        let elapsed = start.elapsed();

        // Verify all versions were indexed correctly
        let results = indexes.find_node_versions_in_valid_time_range(
            node_id,
            TimeRange::new(
                0.into(),
                (((num_threads * versions_per_thread) * 100) as i64).into(),
            )
            .unwrap(),
        );

        assert_eq!(
            results.len(),
            (num_threads * versions_per_thread) as usize,
            "All versions should be indexed despite contention"
        );

        // Verify no deadlocks occurred and performance is reasonable
        // With 8 threads × 500 ops = 4000 total inserts, should complete in < 1 second
        assert!(
            elapsed.as_secs() < 2,
            "Should complete in reasonable time despite contention, took {:?}",
            elapsed
        );

        // Verify timeline is correctly sorted despite concurrent inserts
        let entity_id = EntityId::Node(node_id);
        let timelines = indexes.index.get(&entity_id).unwrap();
        let versions = &timelines.valid.versions;

        for i in 1..versions.len() {
            assert!(
                versions[i - 1].start <= versions[i].start,
                "Timeline must remain sorted despite concurrent writes"
            );
        }
    }

    #[test]
    fn test_very_large_history() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();
        let version_count = 100_000;

        // Insert 100K versions
        for i in 0..version_count {
            let version_id = VersionId::new(i).unwrap();
            indexes
                .insert_node_version(
                    node_id,
                    version_id,
                    BiTemporalInterval::new(
                        TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into())
                            .unwrap(),
                        TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                    ),
                )
                .unwrap();
        }

        // Query a small range in the middle - should be fast
        let results = indexes.find_node_versions_in_valid_time_range(
            node_id,
            TimeRange::new(5_000_000.into(), 5_001_000.into()).unwrap(),
        );

        // Should find ~10 versions in this range
        assert!(
            results.len() >= 10 && results.len() <= 11,
            "Should find ~10 versions in 1000-tick range, found {}",
            results.len()
        );

        // Query the entire timeline - should return all versions
        let all_results = indexes.find_node_versions_in_valid_time_range(
            node_id,
            TimeRange::new(0.into(), ((version_count * 100) as i64).into()).unwrap(),
        );

        assert_eq!(
            all_results.len(),
            version_count as usize,
            "Should find all versions in full range query"
        );
    }

    #[test]
    fn test_same_start_time_different_versions() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();
        let v1 = VersionId::new(1).unwrap();
        let v2 = VersionId::new(2).unwrap();

        // Both versions start at time 1000 (e.g., two corrections recorded simultaneously)
        indexes
            .insert_node_version(
                node_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 2000.into()).unwrap(),
                    TimeRange::from(0.into()),
                ),
            )
            .unwrap();
        indexes
            .insert_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 3000.into()).unwrap(),
                    TimeRange::from(0.into()),
                ),
            )
            .unwrap();

        // Query at time 1500 should return both versions
        let results =
            indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::at(1500.into()));
        assert_eq!(
            results.len(),
            2,
            "Both versions should be found when they have the same start time"
        );
        assert!(results.contains(&v1), "Version 1 should be in results");
        assert!(results.contains(&v2), "Version 2 should be in results");

        // Verify timeline is sorted and both entries exist
        let entity_id = EntityId::Node(node_id);
        let timelines = indexes.index.get(&entity_id).unwrap();
        assert_eq!(
            timelines.valid.versions.len(),
            2,
            "Timeline should contain both versions"
        );
        // Both should have start time 1000
        assert_eq!(timelines.valid.versions[0].start, 1000.into());
        assert_eq!(timelines.valid.versions[1].start, 1000.into());
    }

    #[test]
    fn test_dos_protection_version_limit_node() {
        // Create index with very low limit for testing
        let config = TemporalIndexConfig {
            max_versions_per_entity: 10,
        };
        let indexes = TemporalIndexes::with_config(config);
        let node_id = NodeId::new(1).unwrap();

        // Insert up to the limit - should succeed
        for i in 0..10 {
            let version_id = VersionId::new(i).unwrap();
            let result = indexes.insert_node_version(
                node_id,
                version_id,
                BiTemporalInterval::new(
                    TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into())
                        .unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            );
            assert!(result.is_ok(), "Insert {} should succeed", i);
        }

        // Insert one more - should fail with CapacityExceeded
        let version_id = VersionId::new(10).unwrap();
        let result = indexes.insert_node_version(
            node_id,
            version_id,
            BiTemporalInterval::new(
                TimeRange::new(1000.into(), 1100.into()).unwrap(),
                TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
            ),
        );

        assert!(result.is_err(), "Insert beyond limit should fail");
        let err = result.unwrap_err();
        let err_str = err.to_string();
        assert!(
            err_str.contains("Capacity") || err_str.contains("exceeded"),
            "Error should mention capacity: {}",
            err_str
        );
        assert!(
            err_str.contains("versions for entity"),
            "Error should identify the entity: {}",
            err_str
        );

        // Verify the entity still has exactly 10 versions
        let entity_id = EntityId::Node(node_id);
        let timelines = indexes.index.get(&entity_id).unwrap();
        assert_eq!(
            timelines.valid.versions.len(),
            10,
            "Should have exactly 10 versions after rejection"
        );
    }

    #[test]
    fn test_dos_protection_version_limit_edge() {
        // Create index with very low limit for testing
        let config = TemporalIndexConfig {
            max_versions_per_entity: 5,
        };
        let indexes = TemporalIndexes::with_config(config);
        let edge_id = EdgeId::new(1).unwrap();

        // Insert up to the limit - should succeed
        for i in 0..5 {
            let version_id = VersionId::new(i).unwrap();
            let result = indexes.insert_edge_version(
                edge_id,
                version_id,
                BiTemporalInterval::new(
                    TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into())
                        .unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            );
            assert!(result.is_ok(), "Insert {} should succeed", i);
        }

        // Insert one more - should fail
        let version_id = VersionId::new(5).unwrap();
        let result = indexes.insert_edge_version(
            edge_id,
            version_id,
            BiTemporalInterval::new(
                TimeRange::new(500.into(), 600.into()).unwrap(),
                TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
            ),
        );

        assert!(result.is_err(), "Insert beyond limit should fail");
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("Capacity") || err_str.contains("exceeded"),
            "Error should mention capacity: {}",
            err_str
        );
    }

    #[test]
    fn test_dos_protection_different_entities_independent() {
        // Verify that limits are per-entity, not global
        let config = TemporalIndexConfig {
            max_versions_per_entity: 5,
        };
        let indexes = TemporalIndexes::with_config(config);

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();

        // Fill node1 to its limit
        for i in 0..5 {
            indexes
                .insert_node_version(
                    node1,
                    VersionId::new(i).unwrap(),
                    BiTemporalInterval::new(
                        TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into())
                            .unwrap(),
                        TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                    ),
                )
                .unwrap();
        }

        // node2 should still be able to insert (independent limit)
        for i in 0..5 {
            let result = indexes.insert_node_version(
                node2,
                VersionId::new(100 + i).unwrap(),
                BiTemporalInterval::new(
                    TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into())
                        .unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            );
            assert!(
                result.is_ok(),
                "node2 insert {} should succeed (independent limit)",
                i
            );
        }

        // But node1 should still be at its limit
        let result = indexes.insert_node_version(
            node1,
            VersionId::new(10).unwrap(),
            BiTemporalInterval::new(
                TimeRange::new(500.into(), 600.into()).unwrap(),
                TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
            ),
        );
        assert!(result.is_err(), "node1 should still be at limit");
    }

    #[test]
    fn test_dos_protection_batch_insert_respects_limit() {
        let config = TemporalIndexConfig {
            max_versions_per_entity: 10,
        };
        let indexes = TemporalIndexes::with_config(config);
        let node_id = NodeId::new(1).unwrap();

        // Insert 8 versions normally
        for i in 0..8 {
            indexes
                .insert_node_version(
                    node_id,
                    VersionId::new(i).unwrap(),
                    BiTemporalInterval::new(
                        TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into())
                            .unwrap(),
                        TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                    ),
                )
                .unwrap();
        }

        // Try to batch insert 5 more (would exceed limit of 10)
        let batch = vec![
            (
                VersionId::new(8).unwrap(),
                BiTemporalInterval::new(
                    TimeRange::new(800.into(), 900.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            ),
            (
                VersionId::new(9).unwrap(),
                BiTemporalInterval::new(
                    TimeRange::new(900.into(), 1000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            ),
            (
                VersionId::new(10).unwrap(),
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 1100.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            ),
        ];

        let result = indexes.insert_node_versions_batch(node_id, batch);

        // Batch should fail because it would exceed limit
        assert!(result.is_err(), "Batch insert exceeding limit should fail");
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("Capacity") || err_str.contains("exceeded"),
            "Error should mention capacity: {}",
            err_str
        );

        // Verify we still have only 8 versions (batch was rejected atomically)
        let entity_id = EntityId::Node(node_id);
        let timelines = indexes.index.get(&entity_id).unwrap();
        assert_eq!(
            timelines.valid.versions.len(),
            8,
            "Should still have 8 versions after batch rejection"
        );
    }

    #[test]
    fn test_dos_protection_default_limit_reasonable() {
        // Verify the default limit is 1,000,000 (reasonable for production)
        let config = TemporalIndexConfig::default();
        assert_eq!(
            config.max_versions_per_entity, 1_000_000,
            "Default limit should be 1 million"
        );

        // Verify we can create indexes with default config
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();

        // Should be able to insert many versions
        for i in 0..1000 {
            let result = indexes.insert_node_version(
                node_id,
                VersionId::new(i).unwrap(),
                BiTemporalInterval::new(
                    TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into())
                        .unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            );
            assert!(
                result.is_ok(),
                "Insert {} should succeed with default limit",
                i
            );
        }
    }

    // ========================================================================
    // Bi-temporal Point Query Tests (Issue #194 fix)
    // ========================================================================

    #[test]
    fn test_find_node_version_at_point_basic() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();

        // v1: valid [0, 1000), tx [0, MAX)
        indexes
            .insert_node_version(
                node_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), 1000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // v2: valid [1000, 2000), tx [500, MAX)
        indexes
            .insert_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 2000.into()).unwrap(),
                    TimeRange::new(500.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // Query at valid_time=500, tx_time=600 should return v1
        let results = indexes.find_node_version_at_point(node_id, 500.into(), 600.into());
        assert_eq!(results.len(), 1, "Should find exactly one version");
        assert_eq!(results[0], v1, "Should find v1");

        // Query at valid_time=1500, tx_time=600 should return v2
        let results = indexes.find_node_version_at_point(node_id, 1500.into(), 600.into());
        assert_eq!(results.len(), 1, "Should find exactly one version");
        assert_eq!(results[0], v2, "Should find v2");

        // Query at valid_time=1500, tx_time=400 should return nothing (v2 not recorded yet)
        let results = indexes.find_node_version_at_point(node_id, 1500.into(), 400.into());
        assert!(
            results.is_empty(),
            "Should find no versions before v2 was recorded"
        );
    }

    #[test]
    fn test_find_node_version_at_point_empty_index() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();

        let results = indexes.find_node_version_at_point(node_id, 500.into(), 600.into());
        assert!(results.is_empty(), "Empty index should return no results");
    }

    #[test]
    fn test_find_edge_version_at_point_basic() {
        let indexes = TemporalIndexes::new();
        let edge_id = EdgeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();

        indexes
            .insert_edge_version(
                edge_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        let results = indexes.find_edge_version_at_point(edge_id, 500.into(), 600.into());
        assert_eq!(results.len(), 1, "Should find exactly one version");
        assert_eq!(results[0], v1, "Should find v1");
    }

    #[test]
    fn test_find_node_version_at_point_overlapping_intervals() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();

        // v1: valid [0, 2000), tx [0, MAX) - spans the full range
        indexes
            .insert_node_version(
                node_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(0.into(), 2000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // v2: valid [1000, 3000), tx [0, MAX) - overlaps with v1
        indexes
            .insert_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 3000.into()).unwrap(),
                    TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                ),
            )
            .unwrap();

        // Query at valid_time=1500, tx_time=100 should return both v1 and v2
        let results = indexes.find_node_version_at_point(node_id, 1500.into(), 100.into());
        assert_eq!(results.len(), 2, "Should find both overlapping versions");
        assert!(results.contains(&v1), "Should include v1");
        assert!(results.contains(&v2), "Should include v2");

        // Query at valid_time=500, tx_time=100 should return only v1
        let results = indexes.find_node_version_at_point(node_id, 500.into(), 100.into());
        assert_eq!(results.len(), 1, "Should find only v1");
        assert_eq!(results[0], v1, "Should be v1");

        // Query at valid_time=2500, tx_time=100 should return only v2
        let results = indexes.find_node_version_at_point(node_id, 2500.into(), 100.into());
        assert_eq!(results.len(), 1, "Should find only v2");
        assert_eq!(results[0], v2, "Should be v2");
    }

    #[test]
    fn test_find_node_version_at_point_boundary_conditions() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();

        // v1: valid [1000, 2000), tx [500, 1500)
        indexes
            .insert_node_version(
                node_id,
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 2000.into()).unwrap(),
                    TimeRange::new(500.into(), 1500.into()).unwrap(),
                ),
            )
            .unwrap();

        // Just before valid_time start - should not find
        let results = indexes.find_node_version_at_point(node_id, 999.into(), 1000.into());
        assert!(
            results.is_empty(),
            "Should not find version before valid_time start"
        );

        // At valid_time start - should find (inclusive start)
        let results = indexes.find_node_version_at_point(node_id, 1000.into(), 1000.into());
        assert_eq!(results.len(), 1, "Should find at valid_time start");

        // Just before valid_time end - should find
        let results = indexes.find_node_version_at_point(node_id, 1999.into(), 1000.into());
        assert_eq!(results.len(), 1, "Should find just before valid_time end");

        // At valid_time end - should not find (exclusive end)
        let results = indexes.find_node_version_at_point(node_id, 2000.into(), 1000.into());
        assert!(
            results.is_empty(),
            "Should not find at valid_time end (exclusive)"
        );

        // Just before tx_time start - should not find
        let results = indexes.find_node_version_at_point(node_id, 1500.into(), 499.into());
        assert!(results.is_empty(), "Should not find before tx_time start");

        // At tx_time end - should not find (exclusive end)
        let results = indexes.find_node_version_at_point(node_id, 1500.into(), 1500.into());
        assert!(
            results.is_empty(),
            "Should not find at tx_time end (exclusive)"
        );
    }

    // Property-based tests using proptest
    mod proptests {
        use super::*;
        use proptest::prelude::*;

        // Strategy for generating valid timestamps (avoid overflow)
        fn timestamp_strategy() -> impl Strategy<Value = i64> {
            0i64..1_000_000_000i64
        }

        // Strategy for generating time ranges
        fn time_range_strategy() -> impl Strategy<Value = (i64, i64)> {
            timestamp_strategy()
                .prop_flat_map(|start| (Just(start), (start + 1)..=(start + 10_000)))
        }

        // Strategy for generating version entries
        fn version_entry_strategy() -> impl Strategy<Value = (VersionId, BiTemporalInterval)> {
            (0u64..10_000u64, time_range_strategy()).prop_map(|(vid, (start, end))| {
                (
                    VersionId::new(vid).unwrap(),
                    BiTemporalInterval::new(
                        TimeRange::new(start.into(), end.into()).unwrap(),
                        TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                    ),
                )
            })
        }

        proptest! {
            /// Property: Inserting versions in any order should produce the same sorted timeline
            #[test]
            fn prop_insert_order_irrelevant(
                versions in prop::collection::vec(version_entry_strategy(), 1..100)
            ) {
                let indexes = TemporalIndexes::new();
                let node_id = NodeId::new(1).unwrap();

                // Insert in original order
                for (version_id, temporal) in &versions {
                    indexes.insert_node_version(node_id, *version_id, *temporal).unwrap();
                }

                // Get the timeline and resolve version IDs before clearing
                let entity_id = EntityId::Node(node_id);
                let timelines1 = indexes.index.get(&entity_id).unwrap();
                let timeline1_entries: Vec<_> = timelines1.valid.versions.iter().map(|e| {
                    (e.start, e.end, timelines1.resolve_version_id(e.metadata_idx))
                }).collect();
                drop(timelines1);

                // Clear and insert in shuffled order
                indexes.clear();
                let mut shuffled = versions.clone();
                shuffled.sort_by_key(|(vid, _)| *vid); // Sort by version ID (different order)
                for (version_id, temporal) in shuffled {
                    indexes.insert_node_version(node_id, version_id, temporal).unwrap();
                }

                let timelines2 = indexes.index.get(&entity_id).unwrap();
                let timeline2_entries: Vec<_> = timelines2.valid.versions.iter().map(|e| {
                    (e.start, e.end, timelines2.resolve_version_id(e.metadata_idx))
                }).collect();

                // Both timelines should be sorted by start time
                for i in 1..timeline1_entries.len() {
                    prop_assert!(timeline1_entries[i-1].0 <= timeline1_entries[i].0);
                }
                for i in 1..timeline2_entries.len() {
                    prop_assert!(timeline2_entries[i-1].0 <= timeline2_entries[i].0);
                }

                // Both timelines should have the same versions at the same sorted positions
                prop_assert_eq!(timeline1_entries.len(), timeline2_entries.len());
                for i in 0..timeline1_entries.len() {
                    prop_assert_eq!(timeline1_entries[i].0, timeline2_entries[i].0, "Start times should match");
                    prop_assert_eq!(timeline1_entries[i].1, timeline2_entries[i].1, "End times should match");
                    prop_assert_eq!(timeline1_entries[i].2, timeline2_entries[i].2, "Version IDs should match");
                }
            }

            /// Property: Time range queries should return exactly the versions that overlap
            #[test]
            fn prop_range_query_correctness(
                versions in prop::collection::vec(version_entry_strategy(), 1..50),
                query_range in time_range_strategy()
            ) {
                let indexes = TemporalIndexes::new();
                let node_id = NodeId::new(1).unwrap();

                // Insert all versions
                for (version_id, temporal) in &versions {
                    indexes.insert_node_version(node_id, *version_id, *temporal).unwrap();
                }

                // Query the range
                let query_time_range = TimeRange::new(query_range.0.into(), query_range.1.into()).unwrap();
                let results = indexes.find_node_versions_in_valid_time_range(node_id, query_time_range);

                // Manually compute expected results
                let mut expected: Vec<VersionId> = versions
                    .iter()
                    .filter(|(_, temporal)| {
                        let valid = temporal.valid_time();
                        // Check overlap: version.end > query.start && version.start < query.end
                        valid.end() > query_range.0.into() && valid.start() < query_range.1.into()
                    })
                    .map(|(vid, _)| *vid)
                    .collect();

                // Sort both for comparison (query results may not be in insertion order)
                let mut results_sorted = results.clone();
                results_sorted.sort();
                expected.sort();

                prop_assert_eq!(results_sorted, expected, "Query returned incorrect versions");
            }

            /// Property: Batch insert should be equivalent to individual inserts (when no duplicates)
            #[test]
            fn prop_batch_insert_equivalence(
                versions in prop::collection::vec(version_entry_strategy(), 1..50)
            ) {
                let node_id = NodeId::new(1).unwrap();

                // Remove duplicates from input to ensure fair comparison
                let mut unique_versions = versions.clone();
                unique_versions.sort_by_key(|(vid, _)| *vid);
                unique_versions.dedup_by_key(|(vid, _)| *vid);

                // Individual inserts
                let indexes1 = TemporalIndexes::new();
                for (version_id, temporal) in &unique_versions {
                    indexes1.insert_node_version(node_id, *version_id, *temporal).unwrap();
                }

                // Batch insert
                let indexes2 = TemporalIndexes::new();
                indexes2.insert_node_versions_batch(node_id, unique_versions.clone()).unwrap();

                // Both should produce identical timelines
                let entity_id = EntityId::Node(node_id);
                let timelines1 = indexes1.index.get(&entity_id).unwrap();
                let timelines2 = indexes2.index.get(&entity_id).unwrap();

                prop_assert_eq!(timelines1.valid.versions.len(), timelines2.valid.versions.len());
                for i in 0..timelines1.valid.versions.len() {
                    let e1 = &timelines1.valid.versions[i];
                    let e2 = &timelines2.valid.versions[i];
                    prop_assert_eq!(e1.start, e2.start);
                    prop_assert_eq!(e1.end, e2.end);
                    // Compare resolved version IDs
                    let v1 = timelines1.resolve_version_id(e1.metadata_idx);
                    let v2 = timelines2.resolve_version_id(e2.metadata_idx);
                    prop_assert_eq!(v1, v2);
                }
            }

            /// Property: Timeline should remain sorted after random retroactive inserts
            #[test]
            fn prop_retroactive_inserts_maintain_order(
                versions in prop::collection::vec(version_entry_strategy(), 1..100)
            ) {
                let indexes = TemporalIndexes::new();
                let node_id = NodeId::new(1).unwrap();

                // Insert versions in random order (simulates retroactive inserts)
                for (version_id, temporal) in &versions {
                    indexes.insert_node_version(node_id, *version_id, *temporal).unwrap();
                }

                // Verify timeline is sorted
                let entity_id = EntityId::Node(node_id);
                let timeline = indexes.index.get(&entity_id).unwrap().valid.versions.clone();

                for i in 1..timeline.len() {
                    prop_assert!(
                        timeline[i-1].start <= timeline[i].start,
                        "Timeline not sorted: timeline[{}].start={} > timeline[{}].start={}",
                        i-1, timeline[i-1].start, i, timeline[i].start
                    );
                }
            }

            /// Property: Point queries should return subset of range queries
            #[test]
            fn prop_point_query_subset_of_range(
                versions in prop::collection::vec(version_entry_strategy(), 1..50),
                point in timestamp_strategy()
            ) {
                let indexes = TemporalIndexes::new();
                let node_id = NodeId::new(1).unwrap();

                // Insert all versions
                for (version_id, temporal) in &versions {
                    indexes.insert_node_version(node_id, *version_id, *temporal).unwrap();
                }

                // Point query
                let point_results = indexes.find_node_versions_in_valid_time_range(
                    node_id,
                    TimeRange::new(point.into(), (point + 1).into()).unwrap()
                );

                // Range query covering the point
                let range_results = indexes.find_node_versions_in_valid_time_range(
                    node_id,
                    TimeRange::new((point - 1000).into(), (point + 1000).into()).unwrap()
                );

                // Every version from point query should be in range query
                for version_id in point_results {
                    prop_assert!(
                        range_results.contains(&version_id),
                        "Point query returned version not in range query: {:?}",
                        version_id
                    );
                }
            }

            /// Property: Timeline remains sorted after batch insert
            #[test]
            fn prop_batch_maintains_sort_order(
                versions in prop::collection::vec(version_entry_strategy(), 1..50)
            ) {
                let indexes = TemporalIndexes::new();
                let node_id = NodeId::new(1).unwrap();

                // Batch insert
                indexes.insert_node_versions_batch(node_id, versions).unwrap();

                // Get timeline
                let entity_id = EntityId::Node(node_id);
                if let Some(timelines) = indexes.index.get(&entity_id) {
                    let timeline = &timelines.valid.versions;

                    // Verify timeline is sorted by start time
                    for i in 1..timeline.len() {
                        prop_assert!(
                            timeline[i-1].start <= timeline[i].start,
                            "Timeline must be sorted by start time"
                        );
                    }

                    // Verify no consecutive duplicates (same metadata index + start time)
                    for i in 1..timeline.len() {
                        if timeline[i-1].start == timeline[i].start {
                            prop_assert!(
                                timeline[i-1].metadata_idx != timeline[i].metadata_idx,
                                "No consecutive entries with same start time and metadata index"
                            );
                        }
                    }
                }
            }
        }
    }

    // ============================================================
    // Tests for Issue #196: Consolidated Version Metadata Storage
    // ============================================================
    // These tests verify that version metadata is stored in a single
    // authoritative storage structure, eliminating duplication between
    // valid-time and transaction-time indexes.

    mod consolidated_storage_tests {
        use super::*;

        /// Test that version metadata is stored only once per version,
        /// not duplicated across valid and transaction timelines.
        #[test]
        fn test_version_metadata_stored_once() {
            let indexes = TemporalIndexes::new();
            let node_id = NodeId::new(1).unwrap();
            let v1 = VersionId::new(100).unwrap();

            // Insert a version
            indexes
                .insert_node_version(
                    node_id,
                    v1,
                    BiTemporalInterval::new(
                        TimeRange::new(0.into(), 1000.into()).unwrap(),
                        TimeRange::new(500.into(), TIMESTAMP_MAX).unwrap(),
                    ),
                )
                .unwrap();

            // Access the internal structure and verify version metadata count
            let entity_id = EntityId::Node(node_id);
            let timelines = indexes.index.get(&entity_id).unwrap();

            // The version_metadata_count should equal the number of unique versions,
            // NOT the sum of valid + tx timeline entries
            assert_eq!(
                timelines.version_metadata_count(),
                1,
                "Version metadata should be stored exactly once, not duplicated"
            );

            // Both timelines should reference the same metadata via index
            assert_eq!(timelines.valid.versions.len(), 1);
            assert_eq!(timelines.tx.versions.len(), 1);
        }

        /// Test that multiple versions are stored efficiently without duplication.
        #[test]
        fn test_multiple_versions_no_duplication() {
            let indexes = TemporalIndexes::new();
            let node_id = NodeId::new(1).unwrap();

            // Insert 100 versions
            for i in 0..100 {
                let version_id = VersionId::new(i).unwrap();
                let start = (i * 1000) as i64;
                indexes
                    .insert_node_version(
                        node_id,
                        version_id,
                        BiTemporalInterval::new(
                            TimeRange::new(start.into(), (start + 1000).into()).unwrap(),
                            TimeRange::new(start.into(), TIMESTAMP_MAX).unwrap(),
                        ),
                    )
                    .unwrap();
            }

            let entity_id = EntityId::Node(node_id);
            let timelines = indexes.index.get(&entity_id).unwrap();

            // Should have exactly 100 version metadata entries (not 200)
            assert_eq!(
                timelines.version_metadata_count(),
                100,
                "Should store 100 unique versions, not 200 (duplicated)"
            );

            // Both timelines should have 100 entries referencing the metadata
            assert_eq!(timelines.valid.versions.len(), 100);
            assert_eq!(timelines.tx.versions.len(), 100);
        }

        /// Test that queries still return correct VersionIds after consolidation.
        #[test]
        fn test_queries_return_version_ids_correctly() {
            let indexes = TemporalIndexes::new();
            let node_id = NodeId::new(1).unwrap();
            let v1 = VersionId::new(100).unwrap();
            let v2 = VersionId::new(101).unwrap();
            let v3 = VersionId::new(102).unwrap();

            // Insert versions with different valid/tx time ranges
            indexes
                .insert_node_version(
                    node_id,
                    v1,
                    BiTemporalInterval::new(
                        TimeRange::new(0.into(), 1000.into()).unwrap(),
                        TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                    ),
                )
                .unwrap();

            indexes
                .insert_node_version(
                    node_id,
                    v2,
                    BiTemporalInterval::new(
                        TimeRange::new(1000.into(), 2000.into()).unwrap(),
                        TimeRange::new(500.into(), TIMESTAMP_MAX).unwrap(),
                    ),
                )
                .unwrap();

            indexes
                .insert_node_version(
                    node_id,
                    v3,
                    BiTemporalInterval::new(
                        TimeRange::new(2000.into(), 3000.into()).unwrap(),
                        TimeRange::new(1000.into(), TIMESTAMP_MAX).unwrap(),
                    ),
                )
                .unwrap();

            // Valid time range query should return correct VersionIds
            let results = indexes.find_node_versions_in_valid_time_range(
                node_id,
                TimeRange::new(500.into(), 1500.into()).unwrap(),
            );
            assert_eq!(results.len(), 2);
            assert!(results.contains(&v1));
            assert!(results.contains(&v2));

            // Transaction time range query should return correct VersionIds
            let results = indexes.find_node_versions_in_transaction_time_range(
                node_id,
                TimeRange::new(600.into(), 800.into()).unwrap(),
            );
            assert_eq!(results.len(), 2);
            assert!(results.contains(&v1));
            assert!(results.contains(&v2));

            // Bi-temporal point query should return correct VersionId
            let results = indexes.find_node_version_at_point(node_id, 1500.into(), 1500.into());
            assert_eq!(results.len(), 1);
            assert!(results.contains(&v2));
        }

        /// Test batch insertion maintains consolidated storage.
        #[test]
        fn test_batch_insert_consolidated_storage() {
            let indexes = TemporalIndexes::new();
            let node_id = NodeId::new(1).unwrap();

            let versions: Vec<_> = (0..50)
                .map(|i| {
                    let version_id = VersionId::new(i).unwrap();
                    let start = (i * 100) as i64;
                    (
                        version_id,
                        BiTemporalInterval::new(
                            TimeRange::new(start.into(), (start + 100).into()).unwrap(),
                            TimeRange::new(start.into(), TIMESTAMP_MAX).unwrap(),
                        ),
                    )
                })
                .collect();

            indexes
                .insert_node_versions_batch(node_id, versions)
                .unwrap();

            let entity_id = EntityId::Node(node_id);
            let timelines = indexes.index.get(&entity_id).unwrap();

            // Batch insert should also maintain consolidated storage
            assert_eq!(
                timelines.version_metadata_count(),
                50,
                "Batch insert should store 50 unique versions, not 100"
            );
        }

        /// Test that version_count reports correct total across all entities.
        #[test]
        fn test_version_count_reflects_consolidated_storage() {
            let indexes = TemporalIndexes::new();

            // Insert 10 versions for node 1
            let node1 = NodeId::new(1).unwrap();
            for i in 0..10 {
                indexes
                    .insert_node_version(
                        node1,
                        VersionId::new(i).unwrap(),
                        BiTemporalInterval::new(
                            TimeRange::new((i as i64 * 100).into(), ((i as i64 + 1) * 100).into())
                                .unwrap(),
                            TimeRange::from(0.into()),
                        ),
                    )
                    .unwrap();
            }

            // Insert 5 versions for node 2
            let node2 = NodeId::new(2).unwrap();
            for i in 10..15 {
                indexes
                    .insert_node_version(
                        node2,
                        VersionId::new(i).unwrap(),
                        BiTemporalInterval::new(
                            TimeRange::new((i as i64 * 100).into(), ((i as i64 + 1) * 100).into())
                                .unwrap(),
                            TimeRange::from(0.into()),
                        ),
                    )
                    .unwrap();
            }

            // Total version count should be 15, not 30
            assert_eq!(
                indexes.version_count(),
                15,
                "version_count should reflect consolidated storage (15 versions, not 30)"
            );
        }

        /// Test memory efficiency: VersionMetadata storage should be smaller
        /// than storing duplicate data in both timelines.
        #[test]
        fn test_memory_layout_efficiency() {
            // This test verifies the memory layout is correct for the
            // consolidated storage approach.

            // TimelineEntry should store an index (u32 or usize), not VersionId directly
            // VersionMetadata should be stored separately

            // Size of consolidated approach:
            // - TimelineEntry: start (16 bytes) + end (16 bytes) + index (4 bytes) = 36 bytes
            // - VersionMetadata: version_id (8 bytes) = 8 bytes per unique version
            // - Total for N versions: N * 36 * 2 (both timelines) + N * 8 = 80N bytes

            // Size of old approach:
            // - TimelineEntry: start (16) + end (16) + version_id (8) = 40 bytes
            // - Total for N versions: N * 40 * 2 = 80N bytes

            // With additional metadata, consolidated approach saves more:
            // - If we add more fields to VersionMetadata (e.g., entity_id, temporal),
            //   it's stored once vs twice

            // For now, verify the structural change is in place
            let indexes = TemporalIndexes::new();
            let node_id = NodeId::new(1).unwrap();
            let v1 = VersionId::new(42).unwrap();

            indexes
                .insert_node_version(
                    node_id,
                    v1,
                    BiTemporalInterval::new(
                        TimeRange::new(0.into(), 1000.into()).unwrap(),
                        TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
                    ),
                )
                .unwrap();

            // Verify we can retrieve the version metadata
            let entity_id = EntityId::Node(node_id);
            let timelines = indexes.index.get(&entity_id).unwrap();

            // Get version metadata and verify it contains the correct version_id
            let metadata = timelines.get_version_metadata(0);
            assert!(
                metadata.is_some(),
                "Should be able to retrieve version metadata by index"
            );
            assert_eq!(
                metadata.unwrap().version_id(),
                v1,
                "Version metadata should contain the correct version_id"
            );
        }

        /// Test that timeline entries reference metadata via index.
        #[test]
        fn test_timeline_entries_use_metadata_index() {
            let indexes = TemporalIndexes::new();
            let node_id = NodeId::new(1).unwrap();

            // Insert multiple versions
            for i in 0..5 {
                indexes
                    .insert_node_version(
                        node_id,
                        VersionId::new(i * 10).unwrap(),
                        BiTemporalInterval::new(
                            TimeRange::new((i as i64 * 100).into(), ((i as i64 + 1) * 100).into())
                                .unwrap(),
                            TimeRange::from(0.into()),
                        ),
                    )
                    .unwrap();
            }

            let entity_id = EntityId::Node(node_id);
            let timelines = indexes.index.get(&entity_id).unwrap();

            // Verify that valid and tx timelines reference the same metadata indices
            assert_eq!(timelines.valid.versions.len(), 5);
            assert_eq!(timelines.tx.versions.len(), 5);

            // Both timelines should have entries that, when resolved, give the same VersionIds
            for i in 0..5 {
                let valid_entry = &timelines.valid.versions[i];
                let tx_entry = &timelines.tx.versions[i];

                // Get version_id through metadata resolution
                let valid_version = timelines
                    .get_version_metadata(valid_entry.metadata_index())
                    .unwrap()
                    .version_id();
                let tx_version = timelines
                    .get_version_metadata(tx_entry.metadata_index())
                    .unwrap()
                    .version_id();

                // Both should resolve to the same VersionId
                assert_eq!(
                    valid_version, tx_version,
                    "Valid and tx entries for same version should resolve to same VersionId"
                );
            }
        }
    }
}
