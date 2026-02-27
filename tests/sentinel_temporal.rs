use aletheiadb::core::error::{StorageError, TemporalError};
use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::core::temporal::{
    BiTemporalInterval, MAX_VALID_TIMESTAMP, TIMESTAMP_MAX, TimeRange,
};
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

// 🛡️ Sentinel Tests added by Sentinel agent

#[test]
fn test_sentinel_bitemporal_visible_at_boundaries() {
    // 🛡️ Sentinel Test: Verify BiTemporalInterval::is_visible_at logic.
    // Specifically targets:
    // - Off-by-one errors at boundaries
    // - Logic errors (e.g. using || instead of &&)

    let valid_start = 100.into();
    let valid_end = 200.into();
    let tx_start = 300.into();
    let tx_end = 400.into();

    let valid_range = TimeRange::new(valid_start, valid_end).unwrap();
    let tx_range = TimeRange::new(tx_start, tx_end).unwrap();
    let interval = BiTemporalInterval::new(valid_range, tx_range);

    // Case 1: Both inside (middle) -> Visible
    assert!(interval.is_visible_at(150.into(), 350.into()));

    // Case 2: Valid inside, Tx inside (exact start boundary) -> Visible
    assert!(interval.is_visible_at(100.into(), 300.into()));

    // Case 3: Valid inside, Tx inside (just before end) -> Visible
    assert!(interval.is_visible_at(199.into(), 399.into()));

    // Case 4: Valid OUT (before), Tx inside -> Not Visible
    assert!(!interval.is_visible_at(99.into(), 350.into()));

    // Case 5: Valid OUT (at end), Tx inside -> Not Visible (end is exclusive)
    assert!(!interval.is_visible_at(200.into(), 350.into()));

    // Case 6: Valid inside, Tx OUT (before) -> Not Visible
    assert!(!interval.is_visible_at(150.into(), 299.into()));

    // Case 7: Valid inside, Tx OUT (at end) -> Not Visible (end is exclusive)
    assert!(!interval.is_visible_at(150.into(), 400.into()));

    // Case 8: Both OUT -> Not Visible
    assert!(!interval.is_visible_at(99.into(), 299.into()));
}

#[test]
fn test_sentinel_timerange_overlaps_touching() {
    // 🛡️ Sentinel Test: Verify strict inequality in overlaps().
    // Touching ranges [100, 200) and [200, 300) must NOT overlap.

    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();
    let r2 = TimeRange::new(200.into(), 300.into()).unwrap();

    // r1 overlaps r2? (100 < 300 && 200 < 200) -> False
    assert!(!r1.overlaps(&r2));

    // r2 overlaps r1? (200 < 200 && 100 < 300) -> False
    assert!(!r2.overlaps(&r1));
}

#[test]
fn test_sentinel_timerange_contains_range_strict() {
    // 🛡️ Sentinel Test: Verify contains_range logic.
    // Targets: self.start <= other.start && other.end <= self.end

    let outer = TimeRange::new(100.into(), 300.into()).unwrap();

    // Exact match -> Contained
    let inner_exact = TimeRange::new(100.into(), 300.into()).unwrap();
    assert!(outer.contains_range(&inner_exact));

    // Ends exactly at boundary -> Contained
    let inner_end_boundary = TimeRange::new(150.into(), 300.into()).unwrap();
    assert!(outer.contains_range(&inner_end_boundary));

    // Starts exactly at boundary -> Contained
    let inner_start_boundary = TimeRange::new(100.into(), 250.into()).unwrap();
    assert!(outer.contains_range(&inner_start_boundary));

    // Extends beyond start -> Not contained
    let inner_out_start = TimeRange::new(99.into(), 200.into()).unwrap();
    assert!(!outer.contains_range(&inner_out_start));

    // Extends beyond end -> Not contained
    let inner_out_end = TimeRange::new(200.into(), 301.into()).unwrap();
    assert!(!outer.contains_range(&inner_out_end));
}

#[test]
fn test_sentinel_timerange_new_validation() {
    // 🛡️ Sentinel Test: Verify constructor validation logic.

    let t100 = 100.into();
    let t200 = 200.into();

    // Valid: start < end
    assert!(TimeRange::new(t100, t200).is_ok());

    // Valid: start == end (empty range)
    // This is crucial: mutating > to >= would break this
    assert!(TimeRange::new(t100, t100).is_ok());

    // Invalid: start > end
    // This targets mutating > to >= (which would make equal invalid, caught above)
    // or removing the check entirely
    let err = TimeRange::new(t200, t100);
    assert!(err.is_err());
    assert!(matches!(
        err.unwrap_err(),
        TemporalError::InvalidTimeRange { .. }
    ));
}

#[test]
fn test_sentinel_timerange_duration_overflow_check() {
    // 🛡️ Sentinel Test: Verify duration calculation handles overflow safely.
    // If end - start overflows i64, it should saturate, not panic or wrap.

    // Create timestamps far apart
    let start = 0.into();
    // i64::MAX is valid as a wallclock value in HybridTimestamp (but check constraints)
    // MAX_VALID_TIMESTAMP is i64::MAX - 1000.

    let max_valid = MAX_VALID_TIMESTAMP.into();
    let range = TimeRange::new(start, max_valid).unwrap();

    // Duration should be MAX_VALID_TIMESTAMP
    assert_eq!(range.duration_micros(), Some(MAX_VALID_TIMESTAMP));

    // Use start = i64::MIN.
    // TimeRange::new checks start > MAX_VALID_TIMESTAMP. i64::MIN is negative, so < MAX.

    let min_start = i64::MIN.into();
    let max_end = MAX_VALID_TIMESTAMP.into();

    let extreme_range = TimeRange::new(min_start, max_end).unwrap();

    // Duration: (MAX - 1000) - MIN = MAX - 1000 - (-MAX - 1) = 2*MAX - 999.
    // This definitely overflows i64.
    // The implementation uses checked_sub.
    // If it overflows, it returns None, then `or(Some(i64::MAX))`.
    // So it should return i64::MAX.

    assert_eq!(extreme_range.duration_micros(), Some(i64::MAX));
}

#[test]
fn test_timerange_close_at_boundary_checks() {
    // 🛡️ Sentinel Test: Verify close_at handles boundary conditions correctly.
    // This targets mutants that might weaken the validation logic in close_at.

    let start = 100.into();
    let range = TimeRange::from(start);

    // Case 1: Close exactly at start time (valid, empty range)
    let closed_at_start = range.close_at(start).unwrap();
    assert_eq!(closed_at_start.end(), start);
    assert!(closed_at_start.is_empty());

    // Case 2: Close before start time (invalid)
    let before_start = 99.into();
    let err = range.close_at(before_start);
    assert!(err.is_err());
    assert!(matches!(
        err.unwrap_err(),
        TemporalError::InvalidTimeRange { .. }
    ));

    // Case 3: Close at TIMESTAMP_MAX (valid, effectively redundant but allowed by logic)
    // The logic allows end <= TIMESTAMP_MAX implicitly if checked against MAX_VALID_TIMESTAMP
    // but the implementation specifically handles TIMESTAMP_MAX as a sentinel or checks < MAX_VALID.
    // The code says: if end.wallclock() > MAX_VALID_TIMESTAMP && end != TIMESTAMP_MAX { Error }
    // So TIMESTAMP_MAX is explicitly allowed.
    let closed_at_max = range.close_at(TIMESTAMP_MAX).unwrap();
    assert_eq!(closed_at_max.end(), TIMESTAMP_MAX);
    assert!(closed_at_max.is_current());
}

#[test]
fn test_timerange_deserialize_malformed_inputs() {
    // 🛡️ Sentinel Test: Verify deserialize handles malformed inputs robustly.
    // This targets buffer length checks and value validation.

    // Case 1: Buffer too short
    let short_bytes = vec![0u8; 23];
    let err = TimeRange::deserialize(&short_bytes);
    assert!(err.is_err());
    assert!(matches!(err.unwrap_err(), StorageError::CorruptedData(_)));

    // Case 2: Inverted range (start > end)
    // Start = 200, End = 100
    let mut inverted_bytes = Vec::new();
    inverted_bytes.extend_from_slice(&200i64.to_le_bytes()); // start.wallclock
    inverted_bytes.extend_from_slice(&0u32.to_le_bytes()); // start.logical
    inverted_bytes.extend_from_slice(&100i64.to_le_bytes()); // end.wallclock
    inverted_bytes.extend_from_slice(&0u32.to_le_bytes()); // end.logical

    let err_inverted = TimeRange::deserialize(&inverted_bytes);
    assert!(err_inverted.is_err());
    // Error message should indicate invalid range
    if let Err(e) = err_inverted {
        // HybridTimestamp Display format is "wallclock.logical"
        // So we expect "200.0" and "100.0"
        assert!(format!("{}", e).contains("start 200.0 > end 100.0"));
    } else {
        panic!("Expected error for inverted range");
    }
}
