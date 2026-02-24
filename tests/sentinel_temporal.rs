use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};
use aletheiadb::index::temporal::{DeduplicationPolicy, TemporalIndexes};

// Test to verify that intersection of large sets (> 16 items) works correctly.
// This targets the HashSet optimization branch in intersect_metadata_indices.
#[test]
fn test_large_intersection_correctness() {
    let indexes = TemporalIndexes::new();
    let node_id = NodeId::new(1).unwrap();

    // Create 20 overlapping versions at (valid=1000, tx=1000)
    // The threshold for switching to HashSet in intersect_metadata_indices is 16.
    let num_versions = 20;

    for i in 0..num_versions {
        let version_id = VersionId::new(i as u64).unwrap();
        // All versions span valid [0, 2000) and tx [0, MAX)
        // They all overlap at valid=1000, tx=1000
        let interval = BiTemporalInterval::new(
            TimeRange::new(0.into(), 2000.into()).unwrap(),
            TimeRange::from(0.into()),
        );

        indexes
            .insert_node_version(node_id, version_id, interval)
            .unwrap();
    }

    // Query at the overlapping point
    let results = indexes.find_node_version_at_point(node_id, 1000.into(), 1000.into());

    assert_eq!(
        results.len(),
        num_versions,
        "Should find all {} overlapping versions",
        num_versions
    );

    // Verify all version IDs are present
    for i in 0..num_versions {
        let version_id = VersionId::new(i as u64).unwrap();
        assert!(
            results.contains(&version_id),
            "Result should contain version {:?}",
            version_id
        );
    }
}

// Test to verify that batch insertion with DeduplicationPolicy::Reject correctly
// detects duplicates against *existing* versions in the timeline, not just within the batch.
#[test]
fn test_batch_insert_reject_existing() {
    let indexes = TemporalIndexes::new();
    let node_id = NodeId::new(1).unwrap();
    let v1 = VersionId::new(100).unwrap();

    // Insert v1 initially
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

    // Try to insert v1 again via batch with Reject policy
    // The batch itself has no duplicates, but it duplicates an existing version.
    let batch = vec![(
        v1,
        BiTemporalInterval::new(
            TimeRange::new(1000.into(), 2000.into()).unwrap(),
            TimeRange::from(0.into()),
        ),
    )];

    let result =
        indexes.insert_node_versions_batch_with_policy(node_id, batch, DeduplicationPolicy::Reject);

    assert!(
        result.is_err(),
        "Batch insert with Reject policy should fail if version already exists in timeline"
    );

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("already exists"),
        "Error should indicate version already exists, got: {}",
        err
    );
}
