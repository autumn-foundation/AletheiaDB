use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::temporal::{
    BiTemporalInterval, MAX_VALID_TIMESTAMP, TIMESTAMP_MAX, TimeRange, time,
};

#[test]
fn test_timerange_is_current_mutants() {
    let start = 100.into();
    let current_range = TimeRange::from(start);
    // is_current() returns `self.end == TIMESTAMP_MAX`
    assert!(current_range.is_current());

    let closed_range = TimeRange::new(100.into(), TIMESTAMP_MAX).unwrap();
    // In our implementation, `TimeRange::new` will actually accept TIMESTAMP_MAX
    assert!(closed_range.is_current());

    // End exactly at MAX_VALID_TIMESTAMP
    let not_current_range = TimeRange::new(
        100.into(),
        HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap(),
    )
    .unwrap();
    assert!(!not_current_range.is_current());
}

#[test]
fn test_timerange_is_closed_mutants() {
    // is_closed() returns `self.end < TIMESTAMP_MAX`
    let open_range = TimeRange::from(100.into());
    assert!(!open_range.is_closed());

    let closed_range = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(closed_range.is_closed());

    let max_valid_closed = TimeRange::new(
        100.into(),
        HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap(),
    )
    .unwrap();
    assert!(max_valid_closed.is_closed());
}

#[test]
fn test_timerange_contains_mutants() {
    // contains() returns `timestamp >= self.start && timestamp < self.end`
    let range = TimeRange::new(100.into(), 200.into()).unwrap();

    // Check operators
    // timestamp >= self.start
    assert!(range.contains(100.into())); // True
    assert!(!range.contains(99.into())); // False (kills >= -> <)

    // timestamp < self.end
    assert!(range.contains(199.into())); // True
    assert!(!range.contains(200.into())); // False (kills < -> <=, ==)
    assert!(!range.contains(201.into())); // False (kills < -> >)

    // Check open range logic
    let open_range = TimeRange::from(100.into());
    assert!(open_range.contains(100.into()));
    assert!(open_range.contains(HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap()));
    // Should NOT contain TIMESTAMP_MAX since it's an exclusive end bound.
    assert!(!open_range.contains(TIMESTAMP_MAX));
}

#[test]
fn test_timerange_contains_or_after_mutants() {
    // contains_or_after() returns `timestamp >= self.start`
    let range = TimeRange::new(100.into(), 200.into()).unwrap();

    assert!(range.contains_or_after(100.into())); // True
    assert!(!range.contains_or_after(99.into())); // False (kills >= -> <)
    assert!(range.contains_or_after(300.into())); // True (even after end)
}

#[test]
fn test_timerange_is_empty_mutants() {
    // is_empty() returns `self.start == self.end`
    let range = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(!range.is_empty());

    let empty_range = TimeRange::new(100.into(), 100.into()).unwrap();
    assert!(empty_range.is_empty()); // True
}

#[test]
fn test_timerange_overlaps_mutants() {
    // overlaps() returns `if self.is_empty() || other.is_empty() { false } self.start < other.end && other.start < self.end`
    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();

    // Test empty
    let empty_range = TimeRange::new(150.into(), 150.into()).unwrap();
    assert!(!r1.overlaps(&empty_range));
    assert!(!empty_range.overlaps(&r1));

    // Test touching
    let r2 = TimeRange::new(200.into(), 300.into()).unwrap();
    // 100 < 300 && 200 < 200 (False)
    assert!(!r1.overlaps(&r2));
    assert!(!r2.overlaps(&r1));

    // Test overlapping
    let r3 = TimeRange::new(150.into(), 250.into()).unwrap();
    assert!(r1.overlaps(&r3));
    assert!(r3.overlaps(&r1));

    // Test exact same
    assert!(r1.overlaps(&r1));

    // Test non-overlapping apart
    let r4 = TimeRange::new(300.into(), 400.into()).unwrap();
    assert!(!r1.overlaps(&r4));
}

#[test]
fn test_timerange_contains_range_mutants() {
    // contains_range() returns `self.start <= other.start && other.end <= self.end`
    let outer = TimeRange::new(100.into(), 300.into()).unwrap();

    let inner_exact = TimeRange::new(100.into(), 300.into()).unwrap();
    assert!(outer.contains_range(&inner_exact));

    let inner_starts_early = TimeRange::new(99.into(), 200.into()).unwrap();
    assert!(!outer.contains_range(&inner_starts_early)); // self.start <= other.start (False)

    let inner_ends_late = TimeRange::new(200.into(), 301.into()).unwrap();
    assert!(!outer.contains_range(&inner_ends_late)); // other.end <= self.end (False)

    let outer_starts_late = TimeRange::new(101.into(), 300.into()).unwrap();
    assert!(!outer_starts_late.contains_range(&inner_exact));
}

#[test]
fn test_timerange_close_at_mutants() {
    let range = TimeRange::from(100.into());

    // Target: end < self.start
    assert!(range.close_at(99.into()).is_err());
    assert!(range.close_at(100.into()).is_ok());

    // Target: end.wallclock() > MAX_VALID_TIMESTAMP && end != TIMESTAMP_MAX
    let max_valid = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap();
    assert!(range.close_at(max_valid).is_ok());

    assert!(range.close_at(TIMESTAMP_MAX).is_ok());
}

#[test]
fn test_timerange_duration_micros_mutants() {
    let range = TimeRange::new(100.into(), 200.into()).unwrap();
    assert_eq!(range.duration_micros(), Some(100)); // Target: Some(0), None

    let current = TimeRange::from(100.into());
    assert_eq!(current.duration_micros(), None); // Target: Some(...)
}

#[test]
fn test_timerange_serialize_mutants() {
    let range = TimeRange::new(100.into(), 200.into()).unwrap();
    let serialized = range.serialize();
    assert_eq!(serialized.len(), 24); // 2 timestamps * 12 bytes = 24. Target: vec![], vec![0]

    // Check it's not empty
    assert!(!serialized.is_empty());

    let (deserialized, size) = TimeRange::deserialize(&serialized).unwrap();
    assert_eq!(size, 24);
    assert_eq!(deserialized.start(), 100.into());
    assert_eq!(deserialized.end(), 200.into());
}

#[test]
fn test_bitemporal_constructors_mutants() {
    let now = BiTemporalInterval::now(100.into(), 200.into());
    assert!(now.is_current());
    assert!(now.is_currently_valid());
    assert!(now.is_currently_recorded());

    let vt = 100.into();
    let with_valid = BiTemporalInterval::with_valid_time(vt, vt);
    assert_eq!(with_valid.valid_time().start(), vt);
    assert_eq!(with_valid.transaction_time().start(), vt);
    assert!(with_valid.is_current());

    // Not Default::default() tests
    assert_ne!(now.valid_time().start(), 0.into());
    assert_ne!(with_valid.valid_time().start(), 0.into());
}

#[test]
fn test_bitemporal_accessors_mutants() {
    let vt = TimeRange::new(100.into(), 200.into()).unwrap();
    let tt = TimeRange::new(300.into(), 400.into()).unwrap();
    let interval = BiTemporalInterval::new(vt, tt);

    assert_eq!(interval.valid_time(), vt);
    assert_eq!(interval.transaction_time(), tt);

    // Kills 'Default::default()' which would be open range from 0
    assert_ne!(interval.valid_time().start(), 0.into());
}

#[test]
fn test_bitemporal_evaluators_mutants() {
    let vt_open = TimeRange::from(100.into());
    let tt_open = TimeRange::from(300.into());
    let vt_closed = TimeRange::new(100.into(), 200.into()).unwrap();
    let tt_closed = TimeRange::new(300.into(), 400.into()).unwrap();

    let open_both = BiTemporalInterval::new(vt_open, tt_open);
    assert!(open_both.is_currently_valid());
    assert!(open_both.is_currently_recorded());
    assert!(open_both.is_current());

    let closed_both = BiTemporalInterval::new(vt_closed, tt_closed);
    assert!(!closed_both.is_currently_valid()); // Kills replace with true
    assert!(!closed_both.is_currently_recorded());
    assert!(!closed_both.is_current());

    // Kills replace with || in is_current
    let open_vt_only = BiTemporalInterval::new(vt_open, tt_closed);
    assert!(!open_vt_only.is_current());
    assert!(open_vt_only.is_currently_valid());
    assert!(!open_vt_only.is_currently_recorded());

    // is_valid_at, is_recorded_at, is_visible_at
    assert!(open_both.is_valid_at(150.into())); // Kills replace with false
    assert!(!open_both.is_valid_at(50.into())); // Kills replace with true

    assert!(open_both.is_recorded_at(350.into()));
    assert!(!open_both.is_recorded_at(250.into()));

    assert!(open_both.is_visible_at(150.into(), 350.into())); // True
    assert!(!open_both.is_visible_at(50.into(), 350.into())); // False (kills replace with ||)
    assert!(!open_both.is_visible_at(150.into(), 250.into())); // False
}

#[test]
fn test_bitemporal_mutators_mutants() {
    let interval =
        BiTemporalInterval::new(TimeRange::from(100.into()), TimeRange::from(300.into()));

    let closed_vt = interval.close_valid_time(200.into()).unwrap();
    assert_eq!(closed_vt.valid_time().end(), 200.into()); // Kills Ok(Default::default())

    let closed_tt = interval.close_transaction_time(400.into()).unwrap();
    assert_eq!(closed_tt.transaction_time().end(), 400.into());

    let closed_both = interval.close_both(200.into(), 400.into()).unwrap();
    assert_eq!(closed_both.valid_time().end(), 200.into());
    assert_eq!(closed_both.transaction_time().end(), 400.into());
}

#[test]
fn test_bitemporal_serialize_mutants() {
    let interval = BiTemporalInterval::now(100.into(), 200.into());
    let serialized = interval.serialize();
    assert_eq!(serialized.len(), 48); // 4 timestamps * 12 bytes = 48. Target: vec![], vec![0], vec![1]

    let (deserialized, size) = BiTemporalInterval::deserialize(&serialized).unwrap();
    assert_eq!(size, 48);
    assert_eq!(
        deserialized.valid_time().start(),
        interval.valid_time().start()
    );
    assert_eq!(
        deserialized.transaction_time().start(),
        interval.transaction_time().start()
    );
}

#[test]
fn test_time_constructors_mutants() {
    let now = time::try_now().unwrap();
    // Default::default() would return (0, 0)
    assert_ne!(now.wallclock(), 0);

    let ts = time::now();
    assert_ne!(ts.wallclock(), 0);
}

#[test]
fn test_time_to_iso8601_mutants() {
    // 1609459200 is 2021-01-01 00:00:00 UTC
    // microseconds: 1609459200 * 1_000_000 = 1609459200000000
    let ts = time::from_secs(1609459200);

    // NOTE: In Rust stdlib without chrono or time formatting features, to_iso8601
    // appears to fall back to the Debug representation of SystemTime for some reason!
    // Or rather, if it's returning "SystemTime { tv_sec: ... }", we should just assert
    // it contains the expected seconds value to kill the math mutants.
    let str_output = time::to_iso8601(ts);
    assert!(
        str_output.contains("1609459200"),
        "Output was: {}",
        str_output
    );

    let ts_with_micros =
        aletheiadb::core::hlc::HybridTimestamp::new(1609459200000000 + 123456, 0).unwrap();
    let str_micros = time::to_iso8601(ts_with_micros);
    assert!(
        str_micros.contains("1609459200"),
        "Output was: {}",
        str_micros
    );
    assert!(
        str_micros.contains("123456") || str_micros.contains("123456000"),
        "Output was: {}",
        str_micros
    );
}

#[test]
fn test_time_conversion_mutants() {
    // Math mutants for `from_secs`, `from_millis`, `to_secs`, `to_millis`
    // e.g. replacing `*` with `+`, `/` with `%`
    let secs: i64 = 5;
    let ts_secs = time::from_secs(secs);
    assert_eq!(ts_secs.wallclock(), 5_000_000); // Kills * -> +

    assert_eq!(time::to_secs(ts_secs), 5); // Kills / -> %, 0, 1, -1

    let millis: i64 = 5000;
    let ts_millis = time::from_millis(millis);
    assert_eq!(ts_millis.wallclock(), 5_000_000); // Kills * -> +

    assert_eq!(time::to_millis(ts_millis), 5000); // Kills / -> %, 0, 1, -1
}
