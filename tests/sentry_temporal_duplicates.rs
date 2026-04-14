use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX, TimeRange};
use aletheiadb::index::temporal::TemporalIndexes;

#[test]
fn test_insert_duplicate_version_id() {
    let indexes = TemporalIndexes::new();
    let node_id = NodeId::new(1).unwrap();
    let version_id = VersionId::new(100).unwrap();

    // 1. Insert first time
    indexes
        .insert_node_version(
            node_id,
            version_id,
            BiTemporalInterval::new(
                TimeRange::new(0.into(), 1000.into()).unwrap(),
                TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
            ),
        )
        .unwrap();

    // 2. Insert same version again (duplicate)
    // This should fail with DuplicateId
    let result = indexes.insert_node_version(
        node_id,
        version_id,
        BiTemporalInterval::new(
            TimeRange::new(1000.into(), 2000.into()).unwrap(),
            TimeRange::new(0.into(), TIMESTAMP_MAX).unwrap(),
        ),
    );

    assert!(result.is_err(), "Duplicate version insertion should fail");

    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("Duplicate"),
        "Error should mention Duplicate"
    );
    assert!(
        err_str.contains("version"),
        "Error should mention version kind"
    );

    // Check if we still have only 1 version metadata
    let count = indexes.version_count();

    assert_eq!(count, 1, "Should have only 1 unique version metadata");
}
