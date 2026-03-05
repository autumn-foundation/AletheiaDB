use aletheiadb::core::temporal::{
    BiTemporalInterval, MAX_VALID_TIMESTAMP, TIMESTAMP_MAX, TimeRange, time,
};

#[test]
fn test_sentry_temporal_time_range_from_is_default() {
    let ts = time::from_secs(100);
    let range = TimeRange::from(ts);
    // Mutant: replace TimeRange::from -> Self with Default::default()
    // Let's assert start and end.
    assert_eq!(range.start(), ts);
    assert_eq!(range.end(), TIMESTAMP_MAX);
}

#[test]
fn test_sentry_temporal_time_range_at_is_default() {
    let ts = time::from_secs(100);
    let range = TimeRange::at(ts);
    assert_eq!(range.start(), ts);
    assert_eq!(range.end(), ts);
}

#[test]
fn test_sentry_temporal_contains_or_after_mutants() {
    let start = time::from_secs(100);
    let end = time::from_secs(200);
    let range = TimeRange::between(start, end).unwrap();

    // mutant: replace >= with < in TimeRange::contains_or_after
    assert!(range.contains_or_after(time::from_secs(100)));
    assert!(range.contains_or_after(time::from_secs(150)));
    assert!(range.contains_or_after(time::from_secs(200)));
    assert!(!range.contains_or_after(time::from_secs(99)));
}

#[test]
fn test_sentry_temporal_contains_mutants() {
    let start = time::from_secs(100);
    let end = time::from_secs(200);
    let range = TimeRange::between(start, end).unwrap();

    // mutant: replace >= with < in TimeRange::contains
    assert!(range.contains(time::from_secs(100)));
    assert!(range.contains(time::from_secs(199)));
    assert!(!range.contains(time::from_secs(99)));
    // mutant: replace < with == in TimeRange::contains
    assert!(!range.contains(time::from_secs(200)));
}

#[test]
fn test_sentry_temporal_is_closed_mutants() {
    let closed_range = TimeRange::between(time::from_secs(100), time::from_secs(200)).unwrap();
    assert!(closed_range.is_closed());

    let open_range = TimeRange::from(time::from_secs(100));
    assert!(!open_range.is_closed());
}

#[test]
fn test_sentry_temporal_is_current_mutants() {
    let closed_range = TimeRange::between(time::from_secs(100), time::from_secs(200)).unwrap();
    assert!(!closed_range.is_current());

    let open_range = TimeRange::from(time::from_secs(100));
    assert!(open_range.is_current());
}

#[test]
fn test_sentry_temporal_is_empty_mutants() {
    let empty_range = TimeRange::at(time::from_secs(100));
    assert!(empty_range.is_empty());

    let non_empty_range = TimeRange::between(time::from_secs(100), time::from_secs(200)).unwrap();
    assert!(!non_empty_range.is_empty());
}

#[test]
fn test_sentry_temporal_bitemporal_is_valid_at_mutants() {
    let bitemporal = BiTemporalInterval::new(
        TimeRange::between(time::from_secs(100), time::from_secs(200)).unwrap(),
        TimeRange::between(time::from_secs(300), time::from_secs(400)).unwrap(),
    );

    assert!(bitemporal.is_valid_at(time::from_secs(150)));
    assert!(!bitemporal.is_valid_at(time::from_secs(250)));
}

#[test]
fn test_sentry_temporal_bitemporal_is_recorded_at_mutants() {
    let bitemporal = BiTemporalInterval::new(
        TimeRange::between(time::from_secs(100), time::from_secs(200)).unwrap(),
        TimeRange::between(time::from_secs(300), time::from_secs(400)).unwrap(),
    );

    assert!(bitemporal.is_recorded_at(time::from_secs(350)));
    assert!(!bitemporal.is_recorded_at(time::from_secs(250)));
}

#[test]
fn test_sentry_temporal_to_millis_mutants() {
    let ts = time::from_millis(1500);
    assert_eq!(time::to_millis(ts), 1500);

    // Test that the fractional seconds are completely truncated for from_secs
    let ts_secs = time::from_secs(2);
    assert_eq!(time::to_secs(ts_secs), 2);
}

#[test]
fn test_sentry_temporal_to_secs_mutants() {
    let ts = time::from_secs(1500);
    assert_eq!(time::to_secs(ts), 1500);
}

#[test]
fn test_sentry_temporal_duration_micros_mutants() {
    let range = TimeRange::between(time::from_millis(100), time::from_millis(200)).unwrap();
    assert_eq!(range.duration_micros(), Some(100_000));
}

#[test]
fn test_sentry_temporal_close_at_mutants() {
    let open_range = TimeRange::from(time::from_millis(100));
    let closed_range = open_range.close_at(time::from_millis(200)).unwrap();

    assert_eq!(closed_range.start(), time::from_millis(100));
    assert_eq!(closed_range.end(), time::from_millis(200));
}

#[test]
fn test_sentry_temporal_overlaps_mutants() {
    let range1 = TimeRange::between(time::from_secs(100), time::from_secs(200)).unwrap();
    let range2 = TimeRange::between(time::from_secs(150), time::from_secs(250)).unwrap();
    let range3 = TimeRange::between(time::from_secs(200), time::from_secs(300)).unwrap();

    assert!(range1.overlaps(&range2));
    assert!(!range1.overlaps(&range3)); // touching ranges shouldn't overlap
}

#[test]
fn test_sentry_temporal_max_valid_timestamp_mutant() {
    assert_eq!(MAX_VALID_TIMESTAMP, i64::MAX - 1000);
}
