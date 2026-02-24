use super::metadata::{IndexVec, TimelineVersionMetadata, VersionMetadataIndex};
use super::policy::DeduplicationPolicy;
use crate::core::error::{Result, StorageError};
use crate::core::id::VersionId;
use crate::core::temporal::{TimeRange, Timestamp};

/// Entry in the timeline index.
///
/// Stores temporal bounds and a reference to version metadata.
/// The actual `VersionId` is stored in the consolidated `TimelineVersionMetadata`
/// storage, eliminating duplication between valid and tx timelines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineEntry {
    pub(crate) start: Timestamp,
    pub(crate) end: Timestamp,
    /// Index into the consolidated version metadata storage.
    pub(crate) metadata_idx: VersionMetadataIndex,
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
pub(crate) struct EntityTimeline {
    /// Versions sorted by start time.
    pub(crate) versions: Vec<TimelineEntry>,
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
    pub(crate) fn insert(
        &mut self,
        start: Timestamp,
        end: Timestamp,
        metadata_idx: VersionMetadataIndex,
    ) {
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
    pub(crate) fn update_end_time(
        &mut self,
        metadata_idx: VersionMetadataIndex,
        new_end: Timestamp,
    ) -> bool {
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
    pub(crate) fn insert_batch(
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
    pub(crate) fn find_indices_in_range_iter(
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
    pub(crate) fn find_indices_in_range(&self, range: TimeRange) -> IndexVec {
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
    pub(crate) fn find_indices_at_point_iter(
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
    pub(crate) fn find_indices_at_point(&self, timestamp: Timestamp) -> IndexVec {
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
pub(crate) struct EntityTimelines {
    /// Consolidated version metadata storage.
    /// Both valid and tx timelines reference this via `VersionMetadataIndex`.
    pub(crate) version_metadata: Vec<TimelineVersionMetadata>,
    /// Valid-time timeline index.
    pub(crate) valid: EntityTimeline,
    /// Transaction-time timeline index.
    pub(crate) tx: EntityTimeline,
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
    ) -> Option<&TimelineVersionMetadata> {
        self.version_metadata.get(index as usize)
    }

    /// Add new version metadata and return its index.
    ///
    /// Returns an error if the number of versions exceeds `u32::MAX`.
    /// This is a DoS protection measure aligned with max_versions_per_entity checks.
    #[inline]
    pub(crate) fn add_version_metadata(
        &mut self,
        metadata: TimelineVersionMetadata,
    ) -> Result<VersionMetadataIndex> {
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
    pub(crate) fn resolve_version_id(&self, index: VersionMetadataIndex) -> VersionId {
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
    pub(crate) fn resolve_version_ids(&self, indices: &[VersionMetadataIndex]) -> Vec<VersionId> {
        self.resolve_version_ids_iter(indices).collect()
    }

    /// Find the metadata index for a given VersionId.
    ///
    /// Returns `None` if the VersionId is not found in this entity's metadata.
    #[inline]
    pub(crate) fn find_metadata_index(
        &self,
        version_id: VersionId,
    ) -> Option<VersionMetadataIndex> {
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
    pub(crate) fn update_valid_time_end(
        &mut self,
        version_id: VersionId,
        new_end: Timestamp,
    ) -> bool {
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
    pub(crate) fn update_transaction_time_end(
        &mut self,
        version_id: VersionId,
        new_end: Timestamp,
    ) -> bool {
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
