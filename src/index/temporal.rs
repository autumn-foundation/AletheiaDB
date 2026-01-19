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

/// Entry in the timeline index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimelineEntry {
    start: Timestamp,
    end: Timestamp,
    version_id: VersionId,
}

/// Timeline for a specific entity.
#[derive(Debug, Clone, Default)]
struct EntityTimeline {
    /// Versions sorted by start time.
    versions: Vec<TimelineEntry>,
}

impl EntityTimeline {
    /// Insert a new version into the timeline, maintaining sorted order by start time.
    fn insert(&mut self, start: Timestamp, end: Timestamp, version_id: VersionId) {
        let entry = TimelineEntry {
            start,
            end,
            version_id,
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
    /// - Deduplication prevents memory leaks from duplicate version IDs
    /// - Pre-allocates capacity to avoid multiple reallocations during append
    ///
    /// # Deduplication Policy for Recovery
    ///
    /// After merging and sorting by `start` time, consecutive entries with duplicate
    /// `version_id` are removed. The **first occurrence in the sorted vector** is kept,
    /// which corresponds to the version with the earliest `start` time.
    ///
    /// **Rationale**: This is correct for idempotent WAL replay. If a version is
    /// replayed multiple times, all replayed entries have identical start times, so
    /// keeping the first occurrence (arbitrary among identical entries) is safe.
    ///
    /// **Important**: This method assumes duplicate `version_id` values represent
    /// the same logical version being inserted multiple times. If duplicates represent
    /// different versions (corrections), callers MUST use unique version IDs or
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
        // Deduplicate by version_id to prevent memory leaks during recovery or bulk updates.
        // Keeps the first occurrence when duplicates exist.
        self.versions.dedup_by_key(|e| e.version_id);
    }

    /// Find all versions in this timeline that overlap with the given time range.
    fn find_in_range(&self, range: TimeRange) -> Vec<VersionId> {
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
                results.push(entry.version_id);
            }
        }

        results
    }
}

/// Grouped timelines for valid and transaction dimensions.
#[derive(Debug, Clone, Default)]
struct EntityTimelines {
    valid: EntityTimeline,
    tx: EntityTimeline,
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
    fn insert_version(
        &self,
        entity_id: EntityId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
    ) -> Result<()> {
        let mut timelines = self.index.entry(entity_id).or_default();

        // Check version limit before inserting (DoS protection)
        let current_count = timelines.valid.versions.len();
        if current_count >= self.config.max_versions_per_entity {
            return Err(StorageError::CapacityExceeded {
                resource: format!("versions for entity {:?}", entity_id),
                current: current_count,
                limit: self.config.max_versions_per_entity,
            }
            .into());
        }

        let valid = temporal.valid_time();
        timelines
            .valid
            .insert(valid.start(), valid.end(), version_id);

        let tx = temporal.transaction_time();
        timelines.tx.insert(tx.start(), tx.end(), version_id);

        Ok(())
    }

    /// Helper for batch insertion of versions.
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
        let current_count = timelines.valid.versions.len();
        let new_count = current_count + versions.len();
        if new_count > self.config.max_versions_per_entity {
            return Err(StorageError::CapacityExceeded {
                resource: format!("versions for entity {:?}", entity_id),
                current: new_count,
                limit: self.config.max_versions_per_entity,
            }
            .into());
        }

        let mut valid_entries = Vec::with_capacity(versions.len());
        let mut tx_entries = Vec::with_capacity(versions.len());

        for (v_id, temporal) in versions {
            let valid = temporal.valid_time();
            let tx = temporal.transaction_time();

            valid_entries.push(TimelineEntry {
                start: valid.start(),
                end: valid.end(),
                version_id: v_id,
            });
            tx_entries.push(TimelineEntry {
                start: tx.start(),
                end: tx.end(),
                version_id: v_id,
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
            .map(|t| t.valid.find_in_range(time_range))
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
            .map(|t| t.valid.find_in_range(time_range))
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
            .map(|t| t.tx.find_in_range(time_range))
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
            .map(|t| t.tx.find_in_range(time_range))
            .unwrap_or_default()
    }

    /// Get the total number of indexed version entries.
    pub fn version_count(&self) -> usize {
        self.index
            .iter()
            .map(|entry| entry.value().valid.versions.len())
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
        let results = indexes
            .find_node_versions_in_valid_time_range(node_id, TimeRange::new(500.into(), 1500.into()).unwrap());

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
                BiTemporalInterval::new(TimeRange::new(0.into(), 2000.into()).unwrap(), TimeRange::from(0.into())),
            )
            .unwrap();
        indexes
            .insert_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(TimeRange::new(1000.into(), 3000.into()).unwrap(), TimeRange::from(0.into())),
            )
            .unwrap();

        // Query point at 1500 (both should match)
        let results = indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::at(1500.into()));
        assert_eq!(results.len(), 2);
        assert!(results.contains(&v1));
        assert!(results.contains(&v2));

        // Query point at 500 (only v1)
        let results = indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::at(500.into()));
        assert_eq!(results.len(), 1);
        assert!(results.contains(&v1));

        // Query point at 2500 (only v2)
        let results = indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::at(2500.into()));
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
                BiTemporalInterval::new(TimeRange::new(0.into(), 1000.into()).unwrap(), TimeRange::from(0.into())),
            )
            .unwrap();
        indexes
            .insert_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(TimeRange::new(1000.into(), 2000.into()).unwrap(), TimeRange::from(0.into())),
            )
            .unwrap();

        // Query point at 1000 (only v2 because [start, end) is inclusive-exclusive)
        // Use [1000, 1001) to represent the point 1000
        let results = indexes
            .find_node_versions_in_valid_time_range(node_id, TimeRange::new(1000.into(), 1001.into()).unwrap());
        assert_eq!(results.len(), 1);
        assert!(results.contains(&v2));

        // Query range [500, 1000) (only v1 because 1000 is exclusive)
        let results = indexes
            .find_node_versions_in_valid_time_range(node_id, TimeRange::new(500.into(), 1000.into()).unwrap());
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
                BiTemporalInterval::new(TimeRange::new(0.into(), 10.into()).unwrap(), TimeRange::from(0.into())),
            ),
            (
                VersionId::new(3).unwrap(),
                BiTemporalInterval::new(TimeRange::new(20.into(), 30.into()).unwrap(), TimeRange::from(0.into())),
            ),
            (
                VersionId::new(2).unwrap(),
                BiTemporalInterval::new(TimeRange::new(10.into(), 20.into()).unwrap(), TimeRange::from(0.into())),
            ),
        ];

        indexes
            .insert_node_versions_batch(node_id, versions)
            .unwrap();

        let results =
            indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::new(5.into(), 25.into()).unwrap());
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

        let results =
            indexes.find_node_versions_in_valid_time_range(node1, TimeRange::new(0.into(), 2000.into()).unwrap());

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
        let results = indexes
            .find_node_versions_in_valid_time_range(node_id, TimeRange::new(0.into(), 1000.into()).unwrap());

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
            timelines.valid.versions[0].start, 0.into(),
            "First version should start at 0"
        );
        assert_eq!(
            timelines.valid.versions[0].version_id, v1,
            "First version should be v1"
        );
        assert_eq!(
            timelines.valid.versions[1].start, 10000.into(),
            "Second version should start at 10000"
        );
        assert_eq!(
            timelines.valid.versions[1].version_id, v2,
            "Second version should be v2"
        );
        assert_eq!(
            timelines.valid.versions[2].start, 20000.into(),
            "Third version should start at 20000"
        );
        assert_eq!(
            timelines.valid.versions[2].version_id, v3,
            "Third version should be v3"
        );

        // Verify queries work correctly with retroactively inserted versions
        let results = indexes
            .find_node_versions_in_valid_time_range(node_id, TimeRange::new(9000.into(), 11000.into()).unwrap());
        assert_eq!(results.len(), 1, "Should find 1 version in range");
        assert_eq!(results[0], v2, "Should find v2 in the middle");
    }

    #[test]
    fn test_batch_insert_deduplication() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();

        // Insert v1 normally
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

        // Insert v2 normally
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

        // Simulate recovery scenario: batch insert including duplicate v1 (with different timing)
        // This would cause memory leak without deduplication
        let mut timelines = indexes.index.get_mut(&EntityId::Node(node_id)).unwrap();
        let duplicate_entries = vec![
            TimelineEntry {
                start: 0.into(),
                end: 1500.into(), // Different end time (recovery scenario)
                version_id: v1,
            },
            TimelineEntry {
                start: 2000.into(),
                end: 3000.into(),
                version_id: VersionId::new(102).unwrap(),
            },
        ];
        timelines.valid.insert_batch(duplicate_entries);

        // Verify deduplication worked - should have 3 unique versions, not 4
        assert_eq!(
            timelines.valid.versions.len(),
            3,
            "Deduplication should remove duplicate v1"
        );

        // Verify v1 appears only once (last occurrence kept)
        let v1_count = timelines
            .valid
            .versions
            .iter()
            .filter(|e| e.version_id == v1)
            .count();
        assert_eq!(v1_count, 1, "v1 should appear exactly once after dedup");

        // Verify the kept v1 has the first data (end=1000, not end=1500)
        // dedup_by_key keeps the first occurrence
        let v1_entry = timelines
            .valid
            .versions
            .iter()
            .find(|e| e.version_id == v1)
            .unwrap();
        assert_eq!(
            v1_entry.end, 1000.into(),
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
                                TimeRange::new(((v * 1000) as i64).into(), (((v + 1) * 1000) as i64).into()).unwrap(),
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
                                    ((((thread_id * versions_per_thread + v) + 1) * 100) as i64).into(),
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
            TimeRange::new(0.into(), (((num_threads * versions_per_thread) * 100) as i64).into()).unwrap(),
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
                        TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into()).unwrap(),
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
                BiTemporalInterval::new(TimeRange::new(1000.into(), 2000.into()).unwrap(), TimeRange::from(0.into())),
            )
            .unwrap();
        indexes
            .insert_node_version(
                node_id,
                v2,
                BiTemporalInterval::new(TimeRange::new(1000.into(), 3000.into()).unwrap(), TimeRange::from(0.into())),
            )
            .unwrap();

        // Query at time 1500 should return both versions
        let results = indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::at(1500.into()));
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
                    TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into()).unwrap(),
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
                    TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into()).unwrap(),
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
                        TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into()).unwrap(),
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
                    TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into()).unwrap(),
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
                        TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into()).unwrap(),
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
                    TimeRange::new(((i * 100) as i64).into(), (((i + 1) * 100) as i64).into()).unwrap(),
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

                // Get the timeline
                let entity_id = EntityId::Node(node_id);
                let timeline1 = indexes.index.get(&entity_id).unwrap().valid.versions.clone();

                // Clear and insert in shuffled order
                indexes.clear();
                let mut shuffled = versions.clone();
                shuffled.sort_by_key(|(vid, _)| *vid); // Sort by version ID (different order)
                for (version_id, temporal) in shuffled {
                    indexes.insert_node_version(node_id, version_id, temporal).unwrap();
                }

                let timeline2 = indexes.index.get(&entity_id).unwrap().valid.versions.clone();

                // Both timelines should be sorted by start time
                for i in 1..timeline1.len() {
                    prop_assert!(timeline1[i-1].start <= timeline1[i].start);
                }
                for i in 1..timeline2.len() {
                    prop_assert!(timeline2[i-1].start <= timeline2[i].start);
                }

                // Both timelines should have the same versions at the same sorted positions
                prop_assert_eq!(timeline1.len(), timeline2.len());
                for i in 0..timeline1.len() {
                    prop_assert_eq!(timeline1[i].start, timeline2[i].start);
                    prop_assert_eq!(timeline1[i].end, timeline2[i].end);
                    prop_assert_eq!(timeline1[i].version_id, timeline2[i].version_id);
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
                let timeline1 = indexes1.index.get(&entity_id).unwrap().valid.versions.clone();
                let timeline2 = indexes2.index.get(&entity_id).unwrap().valid.versions.clone();

                prop_assert_eq!(timeline1.len(), timeline2.len());
                for i in 0..timeline1.len() {
                    prop_assert_eq!(timeline1[i].start, timeline2[i].start);
                    prop_assert_eq!(timeline1[i].end, timeline2[i].end);
                    prop_assert_eq!(timeline1[i].version_id, timeline2[i].version_id);
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

                    // Verify no consecutive duplicates (same version ID + start time)
                    for i in 1..timeline.len() {
                        if timeline[i-1].start == timeline[i].start {
                            prop_assert!(
                                timeline[i-1].version_id != timeline[i].version_id,
                                "No consecutive entries with same start time and version ID"
                            );
                        }
                    }
                }
            }
        }
    }
}
