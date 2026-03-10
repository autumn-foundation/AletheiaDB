use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::temporal::TimeRange;

#[test]
fn test_timerange_is_current_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let ts200 = HybridTimestamp::new(200, 0).unwrap();
    let range_closed = TimeRange::new(ts100, ts200).unwrap();
    let range_open = TimeRange::from(ts100);

    assert!(
        !range_closed.is_current(),
        "is_current should not be true when bounded"
    );
    assert!(
        range_open.is_current(),
        "is_current should not be false when open"
    );
}

#[test]
fn test_timerange_is_closed_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let ts200 = HybridTimestamp::new(200, 0).unwrap();
    let range_closed = TimeRange::new(ts100, ts200).unwrap();
    let range_open = TimeRange::from(ts100);

    assert!(
        range_closed.is_closed(),
        "is_closed should not be false when bounded"
    );
    assert!(
        !range_open.is_closed(),
        "is_closed should not be true when open"
    );
}

#[test]
fn test_timerange_contains_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let ts200 = HybridTimestamp::new(200, 0).unwrap();
    let ts300 = HybridTimestamp::new(300, 0).unwrap();
    let range = TimeRange::new(ts100, ts200).unwrap();

    assert!(
        range.contains(ts100),
        "contains should not be false within bounds"
    );
    assert!(
        !range.contains(ts300),
        "contains should not be true outside bounds"
    );
}

#[test]
fn test_timerange_contains_or_after_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let ts200 = HybridTimestamp::new(200, 0).unwrap();
    let ts50 = HybridTimestamp::new(50, 0).unwrap();
    let range = TimeRange::new(ts100, ts200).unwrap();

    assert!(
        range.contains_or_after(ts200),
        "contains_or_after should not be false after start"
    );
    assert!(
        !range.contains_or_after(ts50),
        "contains_or_after should not be true before start"
    );
}

#[test]
fn test_timerange_is_empty_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let ts200 = HybridTimestamp::new(200, 0).unwrap();
    let range_valid = TimeRange::new(ts100, ts200).unwrap();
    let range_empty = TimeRange::at(ts100);

    assert!(
        !range_valid.is_empty(),
        "is_empty should not be true on valid range"
    );
    assert!(
        range_empty.is_empty(),
        "is_empty should not be false on empty point"
    );
}

#[test]
fn test_timerange_overlaps_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let ts200 = HybridTimestamp::new(200, 0).unwrap();
    let ts300 = HybridTimestamp::new(300, 0).unwrap();

    let range1 = TimeRange::new(ts100, ts200).unwrap();
    let range2 = TimeRange::new(ts200, ts300).unwrap();
    let range3 = TimeRange::new(ts100, ts300).unwrap();

    assert!(
        !range1.overlaps(&range2),
        "overlaps should not be true when touching"
    );
    assert!(
        range3.overlaps(&range2),
        "overlaps should not be false when intersecting"
    );
}

#[test]
fn test_timerange_contains_range_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let ts200 = HybridTimestamp::new(200, 0).unwrap();
    let ts300 = HybridTimestamp::new(300, 0).unwrap();

    let range1 = TimeRange::new(ts100, ts200).unwrap();
    let range2 = TimeRange::new(ts200, ts300).unwrap();
    let range3 = TimeRange::new(ts100, ts300).unwrap();

    assert!(
        !range1.contains_range(&range2),
        "contains_range should not be true when outside bounds"
    );
    assert!(
        range3.contains_range(&range1),
        "contains_range should not be false when strictly inside bounds"
    );
}

#[test]
fn test_timerange_duration_micros_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let ts200 = HybridTimestamp::new(200, 0).unwrap();

    assert_eq!(
        TimeRange::at(ts100).duration_micros(),
        Some(0),
        "duration_micros on point should be exactly Some(0)"
    );

    let diff_us = 200 - 100;
    assert_eq!(
        TimeRange::new(ts100, ts200).unwrap().duration_micros(),
        Some(diff_us),
        "duration_micros should compute exact values"
    );
}
