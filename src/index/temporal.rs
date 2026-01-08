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

use crate::core::id::{EdgeId, EntityId, NodeId, VersionId};
use crate::core::temporal::{BiTemporalInterval, TimeRange, Timestamp};
use dashmap::DashMap;

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
    fn insert_batch(&mut self, mut entries: Vec<TimelineEntry>) {
        if entries.is_empty() {
            return;
        }
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
        // - Point/small range queries (< 1000 ticks) typically return 1-2 versions → cap at 4
        // - Large range queries can return many versions → cap at 16
        let range_size = range.end() - range.start();
        let estimated_capacity = if range_size < 1000 {
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
#[derive(Debug, Default)]
pub struct TemporalIndexes {
    /// Combined index for both valid and transaction timelines.
    index: DashMap<EntityId, EntityTimelines>,
}

impl TemporalIndexes {
    /// Create a new empty temporal index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a node version into the temporal indexes.
    pub fn insert_node_version(
        &self,
        node_id: NodeId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
    ) {
        self.insert_version(EntityId::Node(node_id), version_id, temporal);
    }

    /// Insert an edge version into the temporal indexes.
    pub fn insert_edge_version(
        &self,
        edge_id: EdgeId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
    ) {
        self.insert_version(EntityId::Edge(edge_id), version_id, temporal);
    }

    /// Insert multiple node versions into the temporal indexes efficiently.
    pub fn insert_node_versions_batch(
        &self,
        node_id: NodeId,
        versions: Vec<(VersionId, BiTemporalInterval)>,
    ) {
        self.insert_versions_batch(EntityId::Node(node_id), versions);
    }

    /// Insert multiple edge versions into the temporal indexes efficiently.
    pub fn insert_edge_versions_batch(
        &self,
        edge_id: EdgeId,
        versions: Vec<(VersionId, BiTemporalInterval)>,
    ) {
        self.insert_versions_batch(EntityId::Edge(edge_id), versions);
    }

    /// Insert a version into both temporal indexes.
    fn insert_version(
        &self,
        entity_id: EntityId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
    ) {
        let mut timelines = self.index.entry(entity_id).or_default();

        let valid = temporal.valid_time();
        timelines
            .valid
            .insert(valid.start(), valid.end(), version_id);

        let tx = temporal.transaction_time();
        timelines.tx.insert(tx.start(), tx.end(), version_id);
    }

    /// Helper for batch insertion of versions.
    fn insert_versions_batch(
        &self,
        entity_id: EntityId,
        versions: Vec<(VersionId, BiTemporalInterval)>,
    ) {
        if versions.is_empty() {
            return;
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

        let mut timelines = self.index.entry(entity_id).or_default();
        timelines.valid.insert_batch(valid_entries);
        timelines.tx.insert_batch(tx_entries);
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
    use crate::core::temporal::BiTemporalInterval;

    #[test]
    fn test_insert_and_find_node_versions() {
        let indexes = TemporalIndexes::new();

        let node_id = NodeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();
        let v3 = VersionId::new(102).unwrap();

        // v1: [0, 1000)
        indexes.insert_node_version(
            node_id,
            v1,
            BiTemporalInterval::new(TimeRange::new(0, 1000), TimeRange::new(0, Timestamp::MAX)),
        );

        // v2: [1000, 2000)
        indexes.insert_node_version(
            node_id,
            v2,
            BiTemporalInterval::new(
                TimeRange::new(1000, 2000),
                TimeRange::new(0, Timestamp::MAX),
            ),
        );

        // v3: [2000, 3000)
        indexes.insert_node_version(
            node_id,
            v3,
            BiTemporalInterval::new(
                TimeRange::new(2000, 3000),
                TimeRange::new(0, Timestamp::MAX),
            ),
        );

        // Test overlap logic: Query [500, 1500)
        // v1 overlaps (500 to 1000)
        // v2 overlaps (1000 to 1500)
        let results =
            indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::new(500, 1500));

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
        indexes.insert_node_version(
            node_id,
            v1,
            BiTemporalInterval::new(TimeRange::new(0, 2000), TimeRange::from(0)),
        );
        indexes.insert_node_version(
            node_id,
            v2,
            BiTemporalInterval::new(TimeRange::new(1000, 3000), TimeRange::from(0)),
        );

        // Query point at 1500 (both should match)
        let results = indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::at(1500));
        assert_eq!(results.len(), 2);
        assert!(results.contains(&v1));
        assert!(results.contains(&v2));

        // Query point at 500 (only v1)
        let results = indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::at(500));
        assert_eq!(results.len(), 1);
        assert!(results.contains(&v1));

        // Query point at 2500 (only v2)
        let results = indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::at(2500));
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
        indexes.insert_node_version(
            node_id,
            v1,
            BiTemporalInterval::new(TimeRange::new(0, 1000), TimeRange::from(0)),
        );
        indexes.insert_node_version(
            node_id,
            v2,
            BiTemporalInterval::new(TimeRange::new(1000, 2000), TimeRange::from(0)),
        );

        // Query point at 1000 (only v2 because [start, end) is inclusive-exclusive)
        // Use [1000, 1001) to represent the point 1000
        let results =
            indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::new(1000, 1001));
        assert_eq!(results.len(), 1);
        assert!(results.contains(&v2));

        // Query range [500, 1000) (only v1 because 1000 is exclusive)
        let results =
            indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::new(500, 1000));
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
                BiTemporalInterval::new(TimeRange::new(0, 10), TimeRange::from(0)),
            ),
            (
                VersionId::new(3).unwrap(),
                BiTemporalInterval::new(TimeRange::new(20, 30), TimeRange::from(0)),
            ),
            (
                VersionId::new(2).unwrap(),
                BiTemporalInterval::new(TimeRange::new(10, 20), TimeRange::from(0)),
            ),
        ];

        indexes.insert_node_versions_batch(node_id, versions);

        let results =
            indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::new(5, 25));
        assert_eq!(results.len(), 3);

        // Verify sort order internally (though opaque to API)
        let timelines = indexes.index.get(&EntityId::Node(node_id)).unwrap();
        assert_eq!(timelines.valid.versions[0].start, 0);
        assert_eq!(timelines.valid.versions[1].start, 10);
        assert_eq!(timelines.valid.versions[2].start, 20);
    }

    #[test]
    fn test_transaction_time_range_query() {
        let indexes = TemporalIndexes::new();

        let edge_id = EdgeId::new(1).unwrap();
        let v1 = VersionId::new(100).unwrap();
        let v2 = VersionId::new(101).unwrap();

        // v1: tx [1000, MAX)
        indexes.insert_edge_version(
            edge_id,
            v1,
            BiTemporalInterval::new(
                TimeRange::new(0, Timestamp::MAX),
                TimeRange::new(1000, Timestamp::MAX),
            ),
        );

        // v2: tx [2000, MAX)
        indexes.insert_edge_version(
            edge_id,
            v2,
            BiTemporalInterval::new(
                TimeRange::new(0, Timestamp::MAX),
                TimeRange::new(2000, Timestamp::MAX),
            ),
        );

        // Query: [1500, 2500)
        let results = indexes
            .find_edge_versions_in_transaction_time_range(edge_id, TimeRange::new(1500, 2500));

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

        indexes.insert_node_version(node1, v1, BiTemporalInterval::current(1000));
        indexes.insert_node_version(node2, v2, BiTemporalInterval::current(1000));

        let results =
            indexes.find_node_versions_in_valid_time_range(node1, TimeRange::new(0, 2000));

        assert_eq!(results.len(), 1);
        assert!(results.contains(&v1));
        assert!(!results.contains(&v2));
    }

    #[test]
    fn test_clear() {
        let indexes = TemporalIndexes::new();

        indexes.insert_node_version(
            NodeId::new(1).unwrap(),
            VersionId::new(100).unwrap(),
            BiTemporalInterval::current(1000),
        );

        assert!(indexes.version_count() > 0);

        indexes.clear();

        assert_eq!(indexes.version_count(), 0);
    }

    #[test]
    fn test_empty_timeline_query() {
        let indexes = TemporalIndexes::new();
        let node_id = NodeId::new(1).unwrap();

        // Query an entity with no versions
        let results =
            indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::new(0, 1000));

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
        indexes.insert_node_version(
            node_id,
            v1,
            BiTemporalInterval::new(TimeRange::new(0, 1000), TimeRange::new(0, Timestamp::MAX)),
        );

        // v3 at t=20000 (far future)
        indexes.insert_node_version(
            node_id,
            v3,
            BiTemporalInterval::new(
                TimeRange::new(20000, 21000),
                TimeRange::new(0, Timestamp::MAX),
            ),
        );

        // v2 at t=10000 (retroactive - inserted between v1 and v3)
        indexes.insert_node_version(
            node_id,
            v2,
            BiTemporalInterval::new(
                TimeRange::new(10000, 11000),
                TimeRange::new(0, Timestamp::MAX),
            ),
        );

        // Verify correct sort order is maintained
        let entity_id = EntityId::Node(node_id);
        let timelines = indexes.index.get(&entity_id).unwrap();

        assert_eq!(timelines.valid.versions.len(), 3, "Should have 3 versions");
        assert_eq!(
            timelines.valid.versions[0].start, 0,
            "First version should start at 0"
        );
        assert_eq!(
            timelines.valid.versions[0].version_id, v1,
            "First version should be v1"
        );
        assert_eq!(
            timelines.valid.versions[1].start, 10000,
            "Second version should start at 10000"
        );
        assert_eq!(
            timelines.valid.versions[1].version_id, v2,
            "Second version should be v2"
        );
        assert_eq!(
            timelines.valid.versions[2].start, 20000,
            "Third version should start at 20000"
        );
        assert_eq!(
            timelines.valid.versions[2].version_id, v3,
            "Third version should be v3"
        );

        // Verify queries work correctly with retroactively inserted versions
        let results =
            indexes.find_node_versions_in_valid_time_range(node_id, TimeRange::new(9000, 11000));
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
        indexes.insert_node_version(
            node_id,
            v1,
            BiTemporalInterval::new(TimeRange::new(0, 1000), TimeRange::new(0, Timestamp::MAX)),
        );

        // Insert v2 normally
        indexes.insert_node_version(
            node_id,
            v2,
            BiTemporalInterval::new(
                TimeRange::new(1000, 2000),
                TimeRange::new(0, Timestamp::MAX),
            ),
        );

        // Simulate recovery scenario: batch insert including duplicate v1 (with different timing)
        // This would cause memory leak without deduplication
        let mut timelines = indexes.index.get_mut(&EntityId::Node(node_id)).unwrap();
        let duplicate_entries = vec![
            TimelineEntry {
                start: 0,
                end: 1500, // Different end time (recovery scenario)
                version_id: v1,
            },
            TimelineEntry {
                start: 2000,
                end: 3000,
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
            v1_entry.end, 1000,
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
                                TimeRange::new((v * 1000) as i64, ((v + 1) * 1000) as i64),
                                TimeRange::new(0, Timestamp::MAX),
                            ),
                        );
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
                TimeRange::new(0, (versions_per_thread * 1000) as i64),
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
                                    ((thread_id * versions_per_thread + v) * 100) as i64,
                                    (((thread_id * versions_per_thread + v) + 1) * 100) as i64,
                                ),
                                TimeRange::new(0, Timestamp::MAX),
                            ),
                        );
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
            TimeRange::new(0, ((num_threads * versions_per_thread) * 100) as i64),
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
            indexes.insert_node_version(
                node_id,
                version_id,
                BiTemporalInterval::new(
                    TimeRange::new((i * 100) as i64, ((i + 1) * 100) as i64),
                    TimeRange::new(0, Timestamp::MAX),
                ),
            );
        }

        // Query a small range in the middle - should be fast
        let results = indexes
            .find_node_versions_in_valid_time_range(node_id, TimeRange::new(5_000_000, 5_001_000));

        // Should find ~10 versions in this range
        assert!(
            results.len() >= 10 && results.len() <= 11,
            "Should find ~10 versions in 1000-tick range, found {}",
            results.len()
        );

        // Query the entire timeline - should return all versions
        let all_results = indexes.find_node_versions_in_valid_time_range(
            node_id,
            TimeRange::new(0, (version_count * 100) as i64),
        );

        assert_eq!(
            all_results.len(),
            version_count as usize,
            "Should find all versions in full range query"
        );
    }
}
