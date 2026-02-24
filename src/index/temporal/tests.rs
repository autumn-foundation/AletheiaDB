use super::*;
use crate::core::id::EntityId;
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::temporal::TimeRange;
use crate::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX};
use crate::index::temporal::metadata::TimelineVersionMetadata;
use crate::index::temporal::timeline::TimelineEntry; // Renamed

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
    let new_metadata_idx = timelines
        .add_version_metadata(TimelineVersionMetadata::new(v3))
        .unwrap();

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
    timelines
        .valid
        .insert_batch(duplicate_entries, DeduplicationPolicy::default())
        .unwrap();

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
fn test_batch_insert_deduplication_non_consecutive_first_occurrence() {
    let indexes = TemporalIndexes::new();
    let node_id = NodeId::new(1).unwrap();
    let v1 = VersionId::new(200).unwrap();
    let v2 = VersionId::new(201).unwrap();

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
                TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
            ),
        )
        .unwrap();

    let mut timelines = indexes.index.get_mut(&EntityId::Node(node_id)).unwrap();
    let v3 = VersionId::new(202).unwrap();
    let new_metadata_idx = timelines
        .add_version_metadata(TimelineVersionMetadata::new(v3))
        .unwrap();

    timelines
        .valid
        .insert_batch(
            vec![
                TimelineEntry {
                    // Duplicate of metadata_idx=0 but with later start, so duplicate entries
                    // are non-consecutive after sorting by start.
                    start: 1500.into(),
                    end: 2500.into(),
                    metadata_idx: 0,
                },
                TimelineEntry {
                    start: 2000.into(),
                    end: 3000.into(),
                    metadata_idx: new_metadata_idx,
                },
            ],
            DeduplicationPolicy::FirstOccurrence,
        )
        .unwrap();

    assert_eq!(
        timelines.valid.versions.len(),
        3,
        "FirstOccurrence must deduplicate non-consecutive duplicate metadata indices"
    );
    let idx0_entries: Vec<_> = timelines
        .valid
        .versions
        .iter()
        .filter(|e| e.metadata_idx == 0)
        .collect();
    assert_eq!(idx0_entries.len(), 1);
    assert_eq!(idx0_entries[0].start, 0.into());
    assert_eq!(idx0_entries[0].end, 1000.into());
}

#[test]
fn test_batch_insert_deduplication_non_consecutive_last_occurrence() {
    let indexes = TemporalIndexes::new();
    let node_id = NodeId::new(2).unwrap();
    let v1 = VersionId::new(300).unwrap();
    let v2 = VersionId::new(301).unwrap();

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
                TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
            ),
        )
        .unwrap();

    let mut timelines = indexes.index.get_mut(&EntityId::Node(node_id)).unwrap();
    timelines
        .valid
        .insert_batch(
            vec![TimelineEntry {
                // Duplicate of metadata_idx=0 appears after metadata_idx=1 in sorted order.
                start: 1500.into(),
                end: 2500.into(),
                metadata_idx: 0,
            }],
            DeduplicationPolicy::LastOccurrence,
        )
        .unwrap();

    assert_eq!(
        timelines.valid.versions.len(),
        2,
        "LastOccurrence must deduplicate non-consecutive duplicate metadata indices"
    );
    let idx0_entries: Vec<_> = timelines
        .valid
        .versions
        .iter()
        .filter(|e| e.metadata_idx == 0)
        .collect();
    assert_eq!(idx0_entries.len(), 1);
    assert_eq!(idx0_entries[0].start, 1500.into());
    assert_eq!(idx0_entries[0].end, 2500.into());
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
                    let version_id = VersionId::new(thread_id * versions_per_thread + v).unwrap();
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
                    let version_id = VersionId::new(thread_id * versions_per_thread + v).unwrap();
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
fn test_batch_insert_equivalence_with_same_start_retroactive_pattern() {
    let node_id = NodeId::new(1).unwrap();
    let versions = vec![
        (
            VersionId::new(1).unwrap(),
            BiTemporalInterval::new(
                TimeRange::new(10.into(), 15.into()).unwrap(),
                TimeRange::from(0.into()),
            ),
        ),
        (
            VersionId::new(2).unwrap(),
            BiTemporalInterval::new(
                TimeRange::new(20.into(), 25.into()).unwrap(),
                TimeRange::from(0.into()),
            ),
        ),
        (
            VersionId::new(3).unwrap(),
            BiTemporalInterval::new(
                TimeRange::new(10.into(), 12.into()).unwrap(),
                TimeRange::from(0.into()),
            ),
        ),
    ];

    let indexes_individual = TemporalIndexes::new();
    for (version_id, temporal) in &versions {
        indexes_individual
            .insert_node_version(node_id, *version_id, *temporal)
            .unwrap();
    }

    let indexes_batch = TemporalIndexes::new();
    indexes_batch
        .insert_node_versions_batch(node_id, versions)
        .unwrap();

    let entity_id = EntityId::Node(node_id);
    let timelines_individual = indexes_individual.index.get(&entity_id).unwrap();
    let timelines_batch = indexes_batch.index.get(&entity_id).unwrap();

    let individual_entries: Vec<_> = timelines_individual
        .valid
        .versions
        .iter()
        .map(|entry| {
            (
                entry.start,
                entry.end,
                timelines_individual.resolve_version_id(entry.metadata_idx),
            )
        })
        .collect();
    let batch_entries: Vec<_> = timelines_batch
        .valid
        .versions
        .iter()
        .map(|entry| {
            (
                entry.start,
                entry.end,
                timelines_batch.resolve_version_id(entry.metadata_idx),
            )
        })
        .collect();

    assert_eq!(individual_entries, batch_entries);
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
        timestamp_strategy().prop_flat_map(|start| (Just(start), (start + 1)..=(start + 10_000)))
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

            // Canonicalize order for comparison:
            // Internal order of elements with equal start time depends on insertion order (metadata_idx),
            // which changes when we shuffle the input. To compare sets, we must sort deterministically.
            let mut t1_sorted = timeline1_entries.clone();
            t1_sorted.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

            let mut t2_sorted = timeline2_entries.clone();
            t2_sorted.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

            // Both timelines should have the same versions
            prop_assert_eq!(t1_sorted, t2_sorted);
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

/// Test for iterator-based find_indices_in_range (TDD - Issue #197)
///
/// This test demonstrates the performance benefits of iterator-based access:
/// 1. Count results without allocation
/// 2. Get first result without collecting all
/// 3. Lazy evaluation for partial result processing
#[test]
fn test_find_indices_in_range_iterator() {
    let mut timeline = crate::index::temporal::timeline::EntityTimeline::default();

    // Create 10 versions with non-overlapping intervals
    for i in 0..10 {
        timeline.insert((i * 100).into(), ((i + 1) * 100).into(), i as u32);
    }

    // Test 1: Count without allocation
    // Query range [250, 550) should overlap with versions 2, 3, 4, 5
    let range = TimeRange::new(250.into(), 550.into()).unwrap();
    let count = timeline.find_indices_in_range_iter(range).count();
    assert_eq!(count, 4, "Should find 4 overlapping versions");

    // Test 2: Get first result without collecting all
    let first = timeline.find_indices_in_range_iter(range).next();
    assert_eq!(first, Some(2), "First result should be version 2");

    // Test 3: Collect to vec when needed (same behavior as old API)
    let collected: Vec<_> = timeline.find_indices_in_range_iter(range).collect();
    assert_eq!(collected.len(), 4);
    assert_eq!(collected[0], 2);
    assert_eq!(collected[1], 3);
    assert_eq!(collected[2], 4);
    assert_eq!(collected[3], 5);

    // Test 4: Iterator can be filtered/mapped without extra allocations
    let sum: u32 = timeline.find_indices_in_range_iter(range).sum();
    assert_eq!(sum, 2 + 3 + 4 + 5, "Sum of indices should be 14");

    // Test 5: Point query (small range)
    let point_range = TimeRange::at(250.into());
    let point_results: Vec<_> = timeline.find_indices_in_range_iter(point_range).collect();
    assert_eq!(point_results.len(), 1);
    assert_eq!(point_results[0], 2);
}

/// Test for iterator-based find_indices_at_point (TDD - Issue #197)
#[test]
fn test_find_indices_at_point_iterator() {
    let mut timeline = crate::index::temporal::timeline::EntityTimeline::default();

    // Create overlapping versions at timestamp 1000
    timeline.insert(500.into(), 1500.into(), 0);
    timeline.insert(800.into(), 1200.into(), 1);
    timeline.insert(1100.into(), 2000.into(), 2);

    // Test 1: Count overlapping versions
    let count = timeline.find_indices_at_point_iter(1000.into()).count();
    assert_eq!(count, 2, "Should find 2 versions at timestamp 1000");

    // Test 2: Get first result
    let first = timeline.find_indices_at_point_iter(1000.into()).next();
    assert_eq!(first, Some(0), "First result should be version 0");

    // Test 3: Collect all results
    let collected: Vec<_> = timeline.find_indices_at_point_iter(1000.into()).collect();
    assert_eq!(collected.len(), 2);
    assert!(collected.contains(&0));
    assert!(collected.contains(&1));
    assert!(!collected.contains(&2));
}

/// Test for public iterator-based find_node_version_at_point_iter (Issue #197)
///
/// This tests the zero-allocation iterator API for bi-temporal point queries,
/// demonstrating the performance benefits for common use cases.
#[test]
fn test_find_node_version_at_point_iterator() {
    let indexes = TemporalIndexes::new();
    let node_id = NodeId::new(1).unwrap();

    // Create 3 overlapping versions at different times
    let v1 = VersionId::new(100).unwrap();
    let v2 = VersionId::new(101).unwrap();
    let v3 = VersionId::new(102).unwrap();

    // v1: valid [0, 2000), tx [0, MAX)
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

    // v2: valid [1000, 3000), tx [500, MAX)
    indexes
        .insert_node_version(
            node_id,
            v2,
            BiTemporalInterval::new(
                TimeRange::new(1000.into(), 3000.into()).unwrap(),
                TimeRange::from(500.into()),
            ),
        )
        .unwrap();

    // v3: valid [1500, 4000), tx [1000, MAX)
    indexes
        .insert_node_version(
            node_id,
            v3,
            BiTemporalInterval::new(
                TimeRange::new(1500.into(), 4000.into()).unwrap(),
                TimeRange::from(1000.into()),
            ),
        )
        .unwrap();

    // Test 1: Get first result only
    // Query at valid_time=1600, tx_time=1200
    // v1: valid [0, 2000) ✓, tx [0, MAX) ✓ → MATCH
    // v2: valid [1000, 3000) ✓, tx [500, MAX) ✓ → MATCH
    // v3: valid [1500, 4000) ✓, tx [1000, MAX) ✓ → MATCH
    // All 3 versions are visible at this point
    let first = indexes
        .find_node_version_at_point_iter(node_id, 1600.into(), 1200.into())
        .next();
    assert!(first.is_some(), "Should find at least one version");
    // Any of the three versions could be first
    assert!(
        first == Some(v1) || first == Some(v2) || first == Some(v3),
        "First result should be one of the matching versions"
    );

    // Test 2: Count results
    let count = indexes
        .find_node_version_at_point_iter(node_id, 1600.into(), 1200.into())
        .count();
    assert_eq!(count, 3, "Should find 3 versions at this point");

    // Test 3: Collect to verify behavior matches Vec-based API
    let iter_results: Vec<_> = indexes
        .find_node_version_at_point_iter(node_id, 1600.into(), 1200.into())
        .collect();
    let vec_results = indexes.find_node_version_at_point(node_id, 1600.into(), 1200.into());

    assert_eq!(iter_results.len(), vec_results.len());
    for version_id in &iter_results {
        assert!(
            vec_results.contains(version_id),
            "Iterator results should match Vec results"
        );
    }

    // Test 4: Query at point with no results
    let empty_count = indexes
        .find_node_version_at_point_iter(node_id, 5000.into(), 5000.into())
        .count();
    assert_eq!(empty_count, 0, "Should find no versions outside time range");

    // Test 5: Query for non-existent node
    let missing_node = NodeId::new(999).unwrap();
    let missing_count = indexes
        .find_node_version_at_point_iter(missing_node, 1000.into(), 1000.into())
        .count();
    assert_eq!(missing_count, 0, "Should find no versions for missing node");
}

/// Test for public iterator-based find_edge_version_at_point_iter (Issue #197)
#[test]
fn test_find_edge_version_at_point_iterator() {
    let indexes = TemporalIndexes::new();
    let edge_id = EdgeId::new(1).unwrap();

    let v1 = VersionId::new(100).unwrap();
    let v2 = VersionId::new(101).unwrap();

    // v1: valid [0, 1500), tx [0, MAX)
    indexes
        .insert_edge_version(
            edge_id,
            v1,
            BiTemporalInterval::new(
                TimeRange::new(0.into(), 1500.into()).unwrap(),
                TimeRange::from(0.into()),
            ),
        )
        .unwrap();

    // v2: valid [1000, 2000), tx [500, MAX)
    indexes
        .insert_edge_version(
            edge_id,
            v2,
            BiTemporalInterval::new(
                TimeRange::new(1000.into(), 2000.into()).unwrap(),
                TimeRange::from(500.into()),
            ),
        )
        .unwrap();

    // Test 1: Get first result
    let first = indexes
        .find_edge_version_at_point_iter(edge_id, 1200.into(), 600.into())
        .next();
    assert!(first.is_some());

    // Test 2: Verify consistency with Vec-based API
    let iter_results: Vec<_> = indexes
        .find_edge_version_at_point_iter(edge_id, 1200.into(), 600.into())
        .collect();
    let vec_results = indexes.find_edge_version_at_point(edge_id, 1200.into(), 600.into());

    assert_eq!(iter_results.len(), vec_results.len());
    for version_id in &iter_results {
        assert!(vec_results.contains(version_id));
    }
}

/// Test update_node_valid_time_end - Issue #209
#[test]
fn test_update_node_valid_time_end() {
    let indexes = TemporalIndexes::new();
    let node_id = NodeId::new(1).unwrap();
    let v1 = VersionId::new(100).unwrap();

    // Insert version with open-ended valid time
    indexes
        .insert_node_version(
            node_id,
            v1,
            BiTemporalInterval::new(
                TimeRange::from(1000.into()), // open-ended
                TimeRange::from(0.into()),
            ),
        )
        .unwrap();

    // Should find version at time 2000
    let results = indexes.find_node_version_at_point(node_id, 2000.into(), 100.into());
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], v1);

    // Close the valid time interval
    indexes.update_node_valid_time_end(node_id, v1, 1500.into());

    // Should still find at time 1200 (within closed interval)
    let results = indexes.find_node_version_at_point(node_id, 1200.into(), 100.into());
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], v1);

    // Should NOT find at time 2000 (after closed interval)
    let results = indexes.find_node_version_at_point(node_id, 2000.into(), 100.into());
    assert_eq!(results.len(), 0);
}

/// Test update_edge_transaction_time_end - Issue #209
#[test]
fn test_update_edge_transaction_time_end() {
    let indexes = TemporalIndexes::new();
    let edge_id = EdgeId::new(1).unwrap();
    let v1 = VersionId::new(100).unwrap();

    // Insert version with open-ended transaction time
    indexes
        .insert_edge_version(
            edge_id,
            v1,
            BiTemporalInterval::new(
                TimeRange::new(1000.into(), 2000.into()).unwrap(),
                TimeRange::from(500.into()), // open-ended
            ),
        )
        .unwrap();

    // Should find version at tx_time 1000
    let results = indexes.find_edge_version_at_point(edge_id, 1500.into(), 1000.into());
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], v1);

    // Close the transaction time interval
    indexes.update_edge_transaction_time_end(edge_id, v1, 800.into());

    // Should still find at tx_time 600 (within closed interval)
    let results = indexes.find_edge_version_at_point(edge_id, 1500.into(), 600.into());
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], v1);

    // Should NOT find at tx_time 1000 (after closed interval)
    let results = indexes.find_edge_version_at_point(edge_id, 1500.into(), 1000.into());
    assert_eq!(results.len(), 0);
}

/// Test update with multiple versions - Issue #209
#[test]
fn test_update_multiple_versions() {
    let indexes = TemporalIndexes::new();
    let node_id = NodeId::new(1).unwrap();
    let v1 = VersionId::new(100).unwrap();
    let v2 = VersionId::new(101).unwrap();
    let v3 = VersionId::new(102).unwrap();

    // Insert three versions with overlapping intervals
    indexes
        .insert_node_version(
            node_id,
            v1,
            BiTemporalInterval::new(TimeRange::from(1000.into()), TimeRange::from(0.into())),
        )
        .unwrap();

    indexes
        .insert_node_version(
            node_id,
            v2,
            BiTemporalInterval::new(TimeRange::from(2000.into()), TimeRange::from(0.into())),
        )
        .unwrap();

    indexes
        .insert_node_version(
            node_id,
            v3,
            BiTemporalInterval::new(TimeRange::from(3000.into()), TimeRange::from(0.into())),
        )
        .unwrap();

    // Close v1's interval
    indexes.update_node_valid_time_end(node_id, v1, 2000.into());

    // Close v2's interval
    indexes.update_node_valid_time_end(node_id, v2, 3000.into());

    // v1 should be found only before 2000
    let results = indexes.find_node_version_at_point(node_id, 1500.into(), 100.into());
    assert_eq!(results, vec![v1]);

    // v2 should be found between 2000 and 3000
    let results = indexes.find_node_version_at_point(node_id, 2500.into(), 100.into());
    assert_eq!(results, vec![v2]);

    // v3 should be found after 3000
    let results = indexes.find_node_version_at_point(node_id, 3500.into(), 100.into());
    assert_eq!(results, vec![v3]);
}

/// Test concurrent updates (Issue #209 - thread safety)
#[test]
fn test_concurrent_updates() {
    use std::sync::Arc;
    use std::thread;

    let indexes = Arc::new(TemporalIndexes::new());
    let node_id = NodeId::new(1).unwrap();

    // Insert 100 versions from main thread
    for i in 0..100 {
        let v_id = VersionId::new(i).unwrap();
        indexes
            .insert_node_version(
                node_id,
                v_id,
                BiTemporalInterval::new(
                    TimeRange::from(((i * 1000) as i64).into()),
                    TimeRange::from(0.into()),
                ),
            )
            .unwrap();
    }

    // Spawn threads to close intervals concurrently
    let mut handles = vec![];
    for i in 0..10 {
        let indexes_clone = Arc::clone(&indexes);
        let handle = thread::spawn(move || {
            for j in 0..10 {
                let idx = i * 10 + j;
                let v_id = VersionId::new(idx).unwrap();
                indexes_clone.update_node_valid_time_end(
                    node_id,
                    v_id,
                    (((idx + 1) * 1000) as i64).into(),
                );
            }
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    // Verify all intervals were closed correctly
    for i in 0..100 {
        let v_id = VersionId::new(i).unwrap();
        let query_time = (((i * 1000) + 500) as i64).into();

        let results = indexes.find_node_version_at_point(node_id, query_time, 100.into());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], v_id);

        // Should NOT find after closed interval
        let query_time = (((i + 1) * 1000 + 500) as i64).into();
        let results = indexes.find_node_version_at_point(node_id, query_time, 100.into());
        // Should find the next version (if it exists) or nothing
        if i < 99 {
            assert_eq!(results.len(), 1);
            assert_eq!(results[0], VersionId::new(i + 1).unwrap());
        }
    }
}

#[test]
fn test_deduplication_policy_variants() {
    let indexes = TemporalIndexes::new();
    let node_id = NodeId::new(1).unwrap();
    let v1 = VersionId::new(100).unwrap();

    // 1. Test FirstOccurrence (Default)
    let batch_first = vec![
        (
            v1,
            BiTemporalInterval::new(
                TimeRange::new(1000.into(), 2000.into()).unwrap(), // Later start
                TimeRange::from(0.into()),
            ),
        ),
        (
            v1,
            BiTemporalInterval::new(
                TimeRange::new(500.into(), 1500.into()).unwrap(), // Earlier start
                TimeRange::from(0.into()),
            ),
        ),
    ];
    indexes
        .insert_node_versions_batch_with_policy(
            node_id,
            batch_first,
            DeduplicationPolicy::FirstOccurrence,
        )
        .unwrap();

    let results = indexes.find_node_versions_in_valid_time_range(
        node_id,
        TimeRange::new(0.into(), 5000.into()).unwrap(),
    );
    assert_eq!(results.len(), 1);
    // Verify earliest-start wins: point query at 600 should match
    assert_eq!(
        indexes
            .find_node_versions_in_valid_time_range(node_id, TimeRange::at(600.into()))
            .len(),
        1
    );
    // point query at 1800 should NOT match
    assert_eq!(
        indexes
            .find_node_versions_in_valid_time_range(node_id, TimeRange::at(1800.into()))
            .len(),
        0
    );

    // 2. Test LastOccurrence
    indexes.clear();
    let batch_last = vec![
        (
            v1,
            BiTemporalInterval::new(
                TimeRange::new(1000.into(), 2000.into()).unwrap(), // Later start
                TimeRange::from(0.into()),
            ),
        ),
        (
            v1,
            BiTemporalInterval::new(
                TimeRange::new(500.into(), 1500.into()).unwrap(), // Earlier start
                TimeRange::from(0.into()),
            ),
        ),
    ];
    indexes
        .insert_node_versions_batch_with_policy(
            node_id,
            batch_last,
            DeduplicationPolicy::LastOccurrence,
        )
        .unwrap();

    let results = indexes.find_node_versions_in_valid_time_range(
        node_id,
        TimeRange::new(0.into(), 5000.into()).unwrap(),
    );
    assert_eq!(results.len(), 1);
    // Verify latest-start wins: point query at 1800 should match
    assert_eq!(
        indexes
            .find_node_versions_in_valid_time_range(node_id, TimeRange::at(1800.into()))
            .len(),
        1
    );
    // point query at 600 should NOT match
    assert_eq!(
        indexes
            .find_node_versions_in_valid_time_range(node_id, TimeRange::at(600.into()))
            .len(),
        0
    );

    // 3. Test Reject
    indexes.clear();
    let batch_reject = vec![
        (
            v1,
            BiTemporalInterval::new(
                TimeRange::new(1000.into(), 2000.into()).unwrap(),
                TimeRange::from(0.into()),
            ),
        ),
        (
            v1,
            BiTemporalInterval::new(
                TimeRange::new(500.into(), 1500.into()).unwrap(),
                TimeRange::from(0.into()),
            ),
        ),
    ];
    let result = indexes.insert_node_versions_batch_with_policy(
        node_id,
        batch_reject,
        DeduplicationPolicy::Reject,
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Duplicate"));
}

#[test]
fn test_metadata_index_reuse_across_batches() {
    let indexes = TemporalIndexes::new();
    let node_id = NodeId::new(1).unwrap();
    let v1 = VersionId::new(100).unwrap();

    // First batch: insert v1
    indexes
        .insert_node_versions_batch(
            node_id,
            vec![(
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(1000.into(), 2000.into()).unwrap(),
                    TimeRange::from(0.into()),
                ),
            )],
        )
        .unwrap();

    // Second batch: insert v1 again with different time
    // It should reuse the metadata index and then deduplicate
    indexes
        .insert_node_versions_batch(
            node_id,
            vec![(
                v1,
                BiTemporalInterval::new(
                    TimeRange::new(500.into(), 1500.into()).unwrap(),
                    TimeRange::from(0.into()),
                ),
            )],
        )
        .unwrap();

    let timelines = indexes.index.get(&EntityId::Node(node_id)).unwrap();
    assert_eq!(
        timelines.version_metadata_count(),
        1,
        "Metadata should be reused"
    );
    assert_eq!(
        timelines.valid.versions.len(),
        1,
        "Duplicate should be removed by insert_batch"
    );
}

#[test]
fn test_get_all_entity_ids() {
    let indexes = TemporalIndexes::new();
    let node_id = NodeId::new(1).unwrap();
    let edge_id = EdgeId::new(2).unwrap();

    indexes
        .insert_node_version(
            node_id,
            VersionId::new(1).unwrap(),
            BiTemporalInterval::current(1000.into()),
        )
        .unwrap();

    indexes
        .insert_edge_version(
            edge_id,
            VersionId::new(2).unwrap(),
            BiTemporalInterval::current(1000.into()),
        )
        .unwrap();

    let ids: Vec<_> = indexes.entity_ids().collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&EntityId::Node(node_id)));
    assert!(ids.contains(&EntityId::Edge(edge_id)));
}
