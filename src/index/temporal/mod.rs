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

use crate::core::error::{Result, StorageError};
use crate::core::id::{EdgeId, EntityId, NodeId, VersionId};
use crate::core::temporal::{BiTemporalInterval, TimeRange, Timestamp};
use dashmap::DashMap;
use smallvec::SmallVec;

/// Policy for handling duplicate versions during batch insertion.
///
/// Duplicate versions are identified by their `VersionId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeduplicationPolicy {
    /// Keep the first occurrence after sorting by start time (default).
    /// This corresponds to the version with the earliest start time.
    /// Correct for idempotent WAL replay.
    #[default]
    FirstOccurrence,

    /// Keep the last occurrence after sorting by start time.
    /// Use when later data should override earlier data with same version ID.
    LastOccurrence,

    /// Reject duplicates with an error.
    /// Use when duplicates indicate a bug or data corruption.
    Reject,
}

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
/// # Memory Layout (Issue #196)
///
/// Previously, `VersionId` was stored in both valid and tx timelines,
/// causing 8 bytes of duplication per version. With consolidated storage:
/// - Timeline entries store a 4-byte index instead of 8-byte `VersionId`
/// - Version metadata is stored once in a central `Vec<VersionMetadata>`
///
/// # Key Benefit
///
/// While the net memory savings for VersionId alone is minimal, this architecture
/// enables storing additional metadata (BiTemporalInterval, provenance, etc.)
/// without proportional cost increase per timeline entry. Each additional field
/// in `VersionMetadata` is stored once, not twice.
pub type VersionMetadataIndex = u32;

/// Optimization: Use SmallVec for index lists to avoid heap allocations for common small queries.
/// 16 * 4 bytes = 64 bytes, fits well on stack.
pub type IndexVec = SmallVec<[VersionMetadataIndex; 16]>;

/// Consolidated version metadata storage.
///
/// Stores version information in a single authoritative location,
/// eliminating duplication between valid-time and transaction-time indexes.
/// Both timelines reference this metadata via `VersionMetadataIndex`.
///
/// # Size
///
/// Current size: 8 bytes (just `VersionId`).
/// This can grow without affecting `TimelineEntry` size.
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
///
/// # Performance Optimization (Issue #209)
///
/// This structure maintains a HashMap to enable O(1) lookups when updating
/// interval end times. Without this, updating would require O(n) linear search
/// through the versions vector, partially defeating the O(log n) query optimization.
#[derive(Debug, Clone, Default)]
struct EntityTimeline {
    /// Versions sorted by start time.
    versions: Vec<TimelineEntry>,
    /// Fast lookup: metadata_idx -> position in versions vec.
    /// Enables O(1) updates instead of O(n) linear search.
    metadata_to_position: std::collections::HashMap<
        VersionMetadataIndex,
        usize,
        std::hash::BuildHasherDefault<crate::core::hasher::IdentityHasher>,
    >,
}

impl EntityTimeline {
    /// Insert a new version into the timeline, maintaining sorted order.
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

        // Optimization: if this version belongs at the end (common case), just push it.
        // We sort by (start, metadata_idx) to make ordering deterministic for equal starts.
        let new_key = (start, metadata_idx);
        if self
            .versions
            .last()
            .is_none_or(|last| (last.start, last.metadata_idx) <= new_key)
        {
            let position = self.versions.len();
            self.versions.push(entry);
            self.metadata_to_position.insert(metadata_idx, position);
            return;
        }

        let idx = self
            .versions
            .partition_point(|e| (e.start, e.metadata_idx) < new_key);
        self.versions.insert(idx, entry);

        // Rebuild position map after insertion (positions shifted)
        self.rebuild_position_map();
    }

    /// Rebuild the metadata_to_position HashMap after operations that shift positions.
    ///
    /// This is called after insertions that aren't at the end, which are rare
    /// (most inserts are chronological and append to the end).
    fn rebuild_position_map(&mut self) {
        self.metadata_to_position.clear();
        for (pos, entry) in self.versions.iter().enumerate() {
            self.metadata_to_position.insert(entry.metadata_idx, pos);
        }
    }

    /// Update the end time of an existing timeline entry.
    ///
    /// This is used when a version's temporal interval is closed (e.g., when a new
    /// version supersedes it). Uses O(1) HashMap lookup to find the entry position.
    ///
    /// # Arguments
    ///
    /// * `metadata_idx` - Index of the version to update
    /// * `new_end` - New end timestamp (exclusive)
    ///
    /// # Returns
    ///
    /// Returns `true` if the entry was found and updated, `false` otherwise.
    ///
    /// # Performance (Issue #209)
    ///
    /// This method is O(1) thanks to the metadata_to_position HashMap. Without it,
    /// we'd need O(n) linear search, partially defeating the query optimization.
    fn update_end_time(&mut self, metadata_idx: VersionMetadataIndex, new_end: Timestamp) -> bool {
        // O(1) lookup via HashMap
        if let Some(&position) = self.metadata_to_position.get(&metadata_idx)
            && let Some(entry) = self.versions.get_mut(position)
        {
            entry.end = new_end;
            return true;
        }
        false
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
    /// # Deduplication Policy
    ///
    /// After merging and sorting by `start` time, entries with duplicate `metadata_idx`
    /// are deduplicated globally according to the provided `policy`.
    ///
    /// | Policy | Behavior | Use Case |
    /// |--------|----------|----------|
    /// | `FirstOccurrence` | Keeps the earliest `start` time | WAL replay, idempotent recovery |
    /// | `LastOccurrence` | Keeps the latest `start` time | Corrections, "latest wins" updates |
    /// | `Reject` | Returns an error if duplicates found | Data integrity validation |
    ///
    /// **Default Behavior**: `FirstOccurrence` is the default policy, which is correct
    /// for idempotent WAL replay.
    ///
    /// **Important**: This method assumes duplicate `metadata_idx` values represent
    /// the same logical version being inserted multiple times. If duplicates represent
    /// different versions (corrections), callers MUST use unique metadata indices or
    /// deduplicate before calling this method.
    fn insert_batch(
        &mut self,
        mut entries: Vec<TimelineEntry>,
        policy: DeduplicationPolicy,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        if policy == DeduplicationPolicy::Reject {
            // Check for duplicates within the incoming batch (atomic check before mutation)
            let mut seen_in_batch = std::collections::HashSet::with_capacity(entries.len());
            for entry in &entries {
                if !seen_in_batch.insert(entry.metadata_idx) {
                    return Err(StorageError::DuplicateId {
                        id: format!("metadata_idx:{}", entry.metadata_idx),
                        kind: "version (duplicate in batch)".to_string(),
                    }
                    .into());
                }
            }

            // Check for duplicates against existing versions (atomic check before mutation)
            for entry in &entries {
                if self.metadata_to_position.contains_key(&entry.metadata_idx) {
                    return Err(StorageError::DuplicateId {
                        id: format!("metadata_idx:{}", entry.metadata_idx),
                        kind: "version (already exists in timeline)".to_string(),
                    }
                    .into());
                }
            }
        }

        // Pre-allocate capacity for single reallocation during append.
        // Critical for bulk recovery/migration (10K+ versions per entity).
        self.versions.reserve(entries.len());
        self.versions.append(&mut entries);
        // Sort by start time with metadata_idx tie-breaker for deterministic ordering.
        // Timsort exploits existing order in the timeline.
        self.versions.sort_by_key(|e| (e.start, e.metadata_idx));

        match policy {
            DeduplicationPolicy::FirstOccurrence => {
                // Keep the earliest occurrence for each metadata_idx.
                // Uses global deduplication (not just consecutive runs) to preserve idempotence
                // even when duplicates are separated by other entries after sorting.
                let mut seen = std::collections::HashSet::with_capacity(self.versions.len());
                self.versions
                    .retain(|entry| seen.insert(entry.metadata_idx));
            }
            DeduplicationPolicy::LastOccurrence => {
                // Keep the latest occurrence for each metadata_idx by scanning in reverse.
                // Reverse scan avoids O(n^2) lookups and preserves sorted order after reverse().
                let mut seen = std::collections::HashSet::with_capacity(self.versions.len());
                let mut deduped = Vec::with_capacity(self.versions.len());
                for entry in self.versions.iter().rev() {
                    if seen.insert(entry.metadata_idx) {
                        deduped.push(*entry);
                    }
                }
                deduped.reverse();
                self.versions = deduped;
            }
            DeduplicationPolicy::Reject => {
                // Already checked above, no further deduplication needed.
            }
        }

        // Rebuild position map after bulk operations (Issue #209)
        self.rebuild_position_map();

        Ok(())
    }

    /// Find all metadata indices in this timeline that overlap with the given time range.
    ///
    /// Returns an iterator over indices into the consolidated version metadata storage.
    /// Callers must resolve these indices to `VersionId` using `EntityTimelines::get_version_metadata`.
    ///
    /// # Performance Benefits (Issue #197)
    ///
    /// This iterator-based version provides zero-allocation access:
    /// - Count results: `find_indices_in_range_iter(range).count()` - no allocation
    /// - First result: `find_indices_in_range_iter(range).next()` - no allocation
    /// - Lazy evaluation: Caller controls allocation strategy via `collect()`, `take()`, etc.
    ///
    /// # Implementation
    ///
    /// Uses binary search (`partition_point`) to find the cutoff, then filters the slice
    /// for overlapping entries. The iterator is lazy and only computes results as needed.
    fn find_indices_in_range_iter(
        &self,
        range: TimeRange,
    ) -> impl Iterator<Item = VersionMetadataIndex> + '_ {
        // Find versions starting before the query range ends.
        let cutoff = self.versions.partition_point(|e| e.start < range.end());
        let range_start = range.start();

        // Return iterator over filtered entries.
        // The filter closure captures range.start() to check for overlap.
        self.versions[..cutoff]
            .iter()
            .filter(move |entry| entry.end > range_start)
            .map(|entry| entry.metadata_idx)
    }

    /// Find all metadata indices in this timeline that overlap with the given time range.
    ///
    /// Returns indices into the consolidated version metadata storage.
    /// Callers must resolve these indices to `VersionId` using `EntityTimelines::get_version_metadata`.
    ///
    /// # Performance
    ///
    /// This is a convenience method that collects results into a `Vec`. For better performance
    /// when you only need a count, first element, or want to process results lazily, use
    /// `find_indices_in_range_iter()` instead.
    fn find_indices_in_range(&self, range: TimeRange) -> IndexVec {
        // Use iterator version and collect.
        // SmallVec stays on stack for up to 16 items (inline capacity).
        let mut results = IndexVec::new();
        results.extend(self.find_indices_in_range_iter(range));
        results
    }

    /// Find all metadata indices that contain a specific point in time.
    ///
    /// Returns an iterator over indices into the consolidated version metadata storage.
    /// A version [start, end) contains timestamp T if: start <= T < end
    ///
    /// # Performance Benefits (Issue #197)
    ///
    /// This iterator-based version provides zero-allocation access:
    /// - Count results: `find_indices_at_point_iter(t).count()` - no allocation
    /// - First result: `find_indices_at_point_iter(t).next()` - no allocation
    /// - Lazy evaluation: Caller controls allocation strategy
    ///
    /// # Complexity
    ///
    /// - Time complexity: O(log N + K) where N = versions, K = overlapping versions
    /// - For typical bi-temporal databases with non-overlapping intervals, K = 1
    fn find_indices_at_point_iter(
        &self,
        timestamp: Timestamp,
    ) -> impl Iterator<Item = VersionMetadataIndex> + '_ {
        // Find all entries where start <= timestamp (these could potentially contain T)
        let cutoff = self.versions.partition_point(|e| e.start <= timestamp);

        // Filter to entries where end > timestamp (completing the containment check)
        self.versions[..cutoff]
            .iter()
            .filter(move |entry| entry.end > timestamp)
            .map(|entry| entry.metadata_idx)
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
    ///
    /// This is a convenience method that collects results into a `Vec`. For better performance
    /// when you only need a count, first element, or want to process results lazily, use
    /// `find_indices_at_point_iter()` instead.
    fn find_indices_at_point(&self, timestamp: Timestamp) -> IndexVec {
        let mut results = IndexVec::new();
        results.extend(self.find_indices_at_point_iter(timestamp));
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
    pub(crate) fn get_version_metadata(
        &self,
        index: VersionMetadataIndex,
    ) -> Option<&VersionMetadata> {
        self.version_metadata.get(index as usize)
    }

    /// Add new version metadata and return its index.
    ///
    /// Returns an error if the number of versions exceeds `u32::MAX`.
    /// This is a DoS protection measure aligned with max_versions_per_entity checks.
    #[inline]
    fn add_version_metadata(&mut self, metadata: VersionMetadata) -> Result<VersionMetadataIndex> {
        let index = self.version_metadata.len();
        if index > u32::MAX as usize {
            return Err(StorageError::CapacityExceeded {
                resource: "version metadata indices".to_string(),
                current: index,
                limit: u32::MAX as usize,
            }
            .into());
        }
        self.version_metadata.push(metadata);
        Ok(index as VersionMetadataIndex)
    }

    /// Resolve a metadata index to a `VersionId`.
    ///
    /// Uses safe indexing to prevent panics in production.
    /// An invalid index indicates internal inconsistency.
    #[inline]
    fn resolve_version_id(&self, index: VersionMetadataIndex) -> VersionId {
        // SAFETY: Indices are generated by add_version_metadata and stored in TimelineEntry.
        // An invalid index would indicate a bug in our own code (internal invariant).
        // Using expect() provides a clear error message if this ever happens.
        self.version_metadata
            .get(index as usize)
            .expect(
                "internal error: invalid metadata index - this indicates a bug in temporal index",
            )
            .version_id()
    }

    /// Resolve multiple metadata indices to `VersionId`s.
    ///
    /// Takes a slice to avoid ownership transfer and returns an iterator
    /// to allow the caller to decide on allocation strategy for hot paths.
    #[inline]
    fn resolve_version_ids_iter<'a>(
        &'a self,
        indices: &'a [VersionMetadataIndex],
    ) -> impl Iterator<Item = VersionId> + 'a {
        indices.iter().map(|&idx| self.resolve_version_id(idx))
    }

    /// Resolve multiple metadata indices to `VersionId`s, returning a Vec.
    ///
    /// Convenience method that collects the iterator results.
    #[inline]
    fn resolve_version_ids(&self, indices: &[VersionMetadataIndex]) -> Vec<VersionId> {
        self.resolve_version_ids_iter(indices).collect()
    }

    /// Find the metadata index for a given VersionId.
    ///
    /// Returns `None` if the VersionId is not found in this entity's metadata.
    #[inline]
    fn find_metadata_index(&self, version_id: VersionId) -> Option<VersionMetadataIndex> {
        self.version_metadata
            .iter()
            .position(|m| m.version_id() == version_id)
            .map(|idx| idx as VersionMetadataIndex)
    }

    /// Update the valid time end timestamp for a version.
    ///
    /// This is used when closing a version's valid time interval (e.g., when a new
    /// version supersedes it). Finds the version by its VersionId and updates the
    /// end time in the valid timeline.
    ///
    /// # Arguments
    ///
    /// * `version_id` - The version to update
    /// * `new_end` - New valid time end timestamp (exclusive)
    ///
    /// # Returns
    ///
    /// Returns `true` if the version was found and updated, `false` otherwise.
    ///
    /// # Invariants
    ///
    /// In correct operation, this should always succeed (version exists in temporal index
    /// if it exists in storage). Debug builds assert this invariant.
    fn update_valid_time_end(&mut self, version_id: VersionId, new_end: Timestamp) -> bool {
        if let Some(metadata_idx) = self.find_metadata_index(version_id) {
            let result = self.valid.update_end_time(metadata_idx, new_end);
            debug_assert!(
                result,
                "Temporal index inconsistency: version {:?} exists in metadata but not in valid timeline",
                version_id
            );
            result
        } else {
            debug_assert!(
                false,
                "Temporal index inconsistency: version {:?} not found in metadata",
                version_id
            );
            false
        }
    }

    /// Update the transaction time end timestamp for a version.
    ///
    /// This is used when closing a version's transaction time interval. Finds the
    /// version by its VersionId and updates the end time in the transaction timeline.
    ///
    /// # Arguments
    ///
    /// * `version_id` - The version to update
    /// * `new_end` - New transaction time end timestamp (exclusive)
    ///
    /// # Returns
    ///
    /// Returns `true` if the version was found and updated, `false` otherwise.
    ///
    /// # Invariants
    ///
    /// In correct operation, this should always succeed (version exists in temporal index
    /// if it exists in storage). Debug builds assert this invariant.
    fn update_transaction_time_end(&mut self, version_id: VersionId, new_end: Timestamp) -> bool {
        if let Some(metadata_idx) = self.find_metadata_index(version_id) {
            let result = self.tx.update_end_time(metadata_idx, new_end);
            debug_assert!(
                result,
                "Temporal index inconsistency: version {:?} exists in metadata but not in tx timeline",
                version_id
            );
            result
        } else {
            debug_assert!(
                false,
                "Temporal index inconsistency: version {:?} not found in metadata",
                version_id
            );
            false
        }
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
    /// Uses the default `DeduplicationPolicy::FirstOccurrence`.
    pub fn insert_node_versions_batch(
        &self,
        node_id: NodeId,
        versions: Vec<(VersionId, BiTemporalInterval)>,
    ) -> Result<()> {
        self.insert_node_versions_batch_with_policy(
            node_id,
            versions,
            DeduplicationPolicy::default(),
        )
    }

    /// Insert multiple node versions into the temporal indexes with a specific deduplication policy.
    pub fn insert_node_versions_batch_with_policy(
        &self,
        node_id: NodeId,
        versions: Vec<(VersionId, BiTemporalInterval)>,
        policy: DeduplicationPolicy,
    ) -> Result<()> {
        self.insert_versions_batch(EntityId::Node(node_id), versions, policy)
    }

    /// Insert multiple edge versions into the temporal indexes efficiently.
    ///
    /// Returns an error if the entity exceeds the configured version limit.
    /// Uses the default `DeduplicationPolicy::FirstOccurrence`.
    pub fn insert_edge_versions_batch(
        &self,
        edge_id: EdgeId,
        versions: Vec<(VersionId, BiTemporalInterval)>,
    ) -> Result<()> {
        self.insert_edge_versions_batch_with_policy(
            edge_id,
            versions,
            DeduplicationPolicy::default(),
        )
    }

    /// Insert multiple edge versions into the temporal indexes with a specific deduplication policy.
    pub fn insert_edge_versions_batch_with_policy(
        &self,
        edge_id: EdgeId,
        versions: Vec<(VersionId, BiTemporalInterval)>,
        policy: DeduplicationPolicy,
    ) -> Result<()> {
        self.insert_versions_batch(EntityId::Edge(edge_id), versions, policy)
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

        // Check for duplicates before inserting (Issue #196)
        if timelines.find_metadata_index(version_id).is_some() {
            return Err(StorageError::DuplicateId {
                id: format!("{}", version_id),
                kind: "version".to_string(),
            }
            .into());
        }

        // Store version metadata once in consolidated storage
        let metadata = VersionMetadata::new(version_id);
        let metadata_idx = timelines.add_version_metadata(metadata)?;

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
    ///
    /// This method ensures that if multiple entries in the batch (or existing entries
    /// in the index) have the same `VersionId`, they share the same metadata index
    /// and are deduplicated according to the specified `policy`.
    fn insert_versions_batch(
        &self,
        entity_id: EntityId,
        versions: Vec<(VersionId, BiTemporalInterval)>,
        policy: DeduplicationPolicy,
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

        // Temporary map to reuse metadata indices within the same batch.
        // This ensures that deduplication in EntityTimeline works for the same VersionId.
        let mut v_id_to_idx = std::collections::HashMap::with_capacity(versions.len());

        for (v_id, temporal) in versions {
            // Store version metadata once in consolidated storage or reuse existing one
            let metadata_idx = if let Some(&idx) = v_id_to_idx.get(&v_id) {
                idx
            } else if let Some(idx) = timelines.find_metadata_index(v_id) {
                v_id_to_idx.insert(v_id, idx);
                idx
            } else {
                let metadata = VersionMetadata::new(v_id);
                let idx = timelines.add_version_metadata(metadata)?;
                v_id_to_idx.insert(v_id, idx);
                idx
            };

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

        timelines.valid.insert_batch(valid_entries, policy)?;
        timelines.tx.insert_batch(tx_entries, policy)?;

        Ok(())
    }

    /// Update the valid time end for a node version (Issue #209).
    ///
    /// This is called when a new version is created and the previous version's
    /// valid time interval needs to be closed. Updates the temporal index to
    /// reflect the new end time.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node whose version is being updated
    /// * `version_id` - The version to update
    /// * `new_end` - New valid time end timestamp (exclusive)
    pub fn update_node_valid_time_end(
        &self,
        node_id: NodeId,
        version_id: VersionId,
        new_end: Timestamp,
    ) {
        if let Some(mut timelines) = self.index.get_mut(&EntityId::Node(node_id)) {
            timelines.update_valid_time_end(version_id, new_end);
        }
    }

    /// Update the transaction time end for a node version (Issue #209).
    ///
    /// This is called when a version's transaction time interval needs to be closed.
    /// Updates the temporal index to reflect the new end time.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node whose version is being updated
    /// * `version_id` - The version to update
    /// * `new_end` - New transaction time end timestamp (exclusive)
    pub fn update_node_transaction_time_end(
        &self,
        node_id: NodeId,
        version_id: VersionId,
        new_end: Timestamp,
    ) {
        if let Some(mut timelines) = self.index.get_mut(&EntityId::Node(node_id)) {
            timelines.update_transaction_time_end(version_id, new_end);
        }
    }

    /// Update the valid time end for an edge version (Issue #209).
    ///
    /// This is called when a new version is created and the previous version's
    /// valid time interval needs to be closed. Updates the temporal index to
    /// reflect the new end time.
    ///
    /// # Arguments
    ///
    /// * `edge_id` - The edge whose version is being updated
    /// * `version_id` - The version to update
    /// * `new_end` - New valid time end timestamp (exclusive)
    pub fn update_edge_valid_time_end(
        &self,
        edge_id: EdgeId,
        version_id: VersionId,
        new_end: Timestamp,
    ) {
        if let Some(mut timelines) = self.index.get_mut(&EntityId::Edge(edge_id)) {
            timelines.update_valid_time_end(version_id, new_end);
        }
    }

    /// Update the transaction time end for an edge version (Issue #209).
    ///
    /// This is called when a version's transaction time interval needs to be closed.
    /// Updates the temporal index to reflect the new end time.
    ///
    /// # Arguments
    ///
    /// * `edge_id` - The edge whose version is being updated
    /// * `version_id` - The version to update
    /// * `new_end` - New transaction time end timestamp (exclusive)
    pub fn update_edge_transaction_time_end(
        &self,
        edge_id: EdgeId,
        version_id: VersionId,
        new_end: Timestamp,
    ) {
        if let Some(mut timelines) = self.index.get_mut(&EntityId::Edge(edge_id)) {
            timelines.update_transaction_time_end(version_id, new_end);
        }
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
                t.resolve_version_ids(&indices)
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
                t.resolve_version_ids(&indices)
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
                t.resolve_version_ids(&indices)
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
                t.resolve_version_ids(&indices)
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

    /// Find node versions visible at a specific bi-temporal point (iterator version).
    ///
    /// Returns an iterator over VersionIds, allowing zero-allocation access for
    /// use cases like `.find()`, `.next()`, or `.take(1)`.
    ///
    /// # Performance Benefits (Issue #197)
    ///
    /// - First result only: `find_node_version_at_point_iter(...).next()` - minimal allocation
    /// - Count: `find_node_version_at_point_iter(...).count()` - no VersionId vec allocation
    /// - Lazy evaluation: Only processes what caller needs
    ///
    /// For typical bi-temporal databases where K=1-2, this significantly reduces allocation
    /// overhead compared to the Vec-based API, especially when only the first result is needed.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node to query
    /// * `valid_time` - The valid time coordinate (when the fact was true in reality)
    /// * `transaction_time` - The transaction time coordinate (when the fact was recorded)
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Get first matching version without allocating Vec
    /// let version = temporal_indexes
    ///     .find_node_version_at_point_iter(node_id, valid_time, tx_time)
    ///     .next();
    /// ```
    pub fn find_node_version_at_point_iter(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> impl Iterator<Item = VersionId> + '_ {
        self.find_version_at_point_iter_impl(EntityId::Node(node_id), valid_time, transaction_time)
    }

    /// Find edge versions visible at a specific bi-temporal point (iterator version).
    ///
    /// Returns an iterator over VersionIds, allowing zero-allocation access for
    /// use cases like `.find()`, `.next()`, or `.take(1)`.
    ///
    /// # Performance Benefits (Issue #197)
    ///
    /// - First result only: `find_edge_version_at_point_iter(...).next()` - minimal allocation
    /// - Count: `find_edge_version_at_point_iter(...).count()` - no VersionId vec allocation
    /// - Lazy evaluation: Only processes what caller needs
    ///
    /// For typical bi-temporal databases where K=1-2, this significantly reduces allocation
    /// overhead compared to the Vec-based API, especially when only the first result is needed.
    ///
    /// # Arguments
    ///
    /// * `edge_id` - The edge to query
    /// * `valid_time` - The valid time coordinate (when the fact was true in reality)
    /// * `transaction_time` - The transaction time coordinate (when the fact was recorded)
    pub fn find_edge_version_at_point_iter(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> impl Iterator<Item = VersionId> + '_ {
        self.find_version_at_point_iter_impl(EntityId::Edge(edge_id), valid_time, transaction_time)
    }

    /// Internal iterator implementation for bi-temporal point queries.
    ///
    /// Shared logic for both node and edge lookups to avoid code duplication.
    ///
    /// # Implementation Strategy
    ///
    /// Due to Rust's borrowing rules with DashMap guards, this method collects
    /// results eagerly and returns an owned iterator. While not zero-allocation,
    /// it provides a consistent iterator-based API and allows future optimizations.
    ///
    /// The Vec-based `find_version_at_point` can be implemented in terms of this
    /// method as `.collect()`, maintaining DRY principles.
    ///
    /// # Current Limitations
    ///
    /// This implementation still allocates a `Vec<VersionId>` internally due to
    /// DashMap's guard-based access patterns. Future optimizations might use
    /// different concurrent data structures to enable true lazy iteration.
    fn find_version_at_point_iter_impl(
        &self,
        entity_id: EntityId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> impl Iterator<Item = VersionId> {
        // Delegate to Vec-based implementation and return owned iterator
        // This is semantically equivalent but provides iterator-based API
        self.find_version_at_point_impl(entity_id, valid_time, transaction_time)
            .into_iter()
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
        let intersected_indices = Self::intersect_metadata_indices(&valid_indices, &tx_indices);

        // Resolve intersected indices to VersionIds
        timelines.resolve_version_ids(&intersected_indices)
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
        a: &[VersionMetadataIndex],
        b: &[VersionMetadataIndex],
    ) -> IndexVec {
        // Threshold for switching to HashSet-based intersection.
        // Below this, linear scan is faster due to cache locality and no allocation.
        const HASH_THRESHOLD: usize = 16;

        let max_len = a.len().max(b.len());

        if max_len < HASH_THRESHOLD {
            // Small sets: linear intersection is efficient due to cache locality
            if a.len() <= b.len() {
                a.iter().copied().filter(|v| b.contains(v)).collect()
            } else {
                b.iter().copied().filter(|v| a.contains(v)).collect()
            }
        } else {
            // Large sets: use HashSet for O(K) intersection instead of O(K²)
            use std::collections::HashSet;
            let b_set: HashSet<_> = b.iter().copied().collect();
            a.iter().copied().filter(|v| b_set.contains(v)).collect()
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

    /// Iterate over all entity IDs currently indexed.
    ///
    /// This allows processing entities without collecting all IDs into memory,
    /// avoiding O(N) memory allocation.
    pub fn entity_ids(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.index.iter().map(|entry| *entry.key())
    }

    /// Clear all indexes.
    pub fn clear(&self) {
        self.index.clear();
    }
}

#[cfg(test)]
mod tests;
