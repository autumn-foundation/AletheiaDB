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

/// Configuration for temporal indexes.
pub mod config;
/// Metadata storage for versions.
pub mod metadata;
/// Deduplication policies for batch operations.
pub mod policy;
#[cfg(test)]
mod tests;
/// Timeline implementation.
pub mod timeline;

pub use config::TemporalIndexConfig;
use metadata::{IndexVec, TimelineVersionMetadata, VersionMetadataIndex};
pub use policy::DeduplicationPolicy;
use timeline::{EntityTimelines, TimelineEntry};

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

        // Store version metadata once in consolidated storage
        let metadata = TimelineVersionMetadata::new(version_id);
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
                let metadata = TimelineVersionMetadata::new(v_id);
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
