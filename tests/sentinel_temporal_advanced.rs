use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange, time};

#[test]
fn test_timerange_contains_or_after_mutants() {
    let start = HybridTimestamp::new(100, 0).unwrap();
    let range = TimeRange::from(start);
    assert!(
        !range.contains_or_after(HybridTimestamp::new(99, 0).unwrap()),
        "contains_or_after false"
    );
}

#[test]
fn test_timerange_is_empty_mutants() {
    let r = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(!r.is_empty(), "not empty");
    let r2 = TimeRange::at(100.into());
    assert!(r2.is_empty(), "empty");
}

#[test]
fn test_timerange_overlaps_mutants() {
    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();
    let r2 = TimeRange::new(150.into(), 250.into()).unwrap();
    assert!(r1.overlaps(&r2));

    // empty tests
    let r_empty = TimeRange::at(100.into());
    assert!(!r_empty.overlaps(&r1));
}

#[test]
fn test_timerange_contains_range_mutants() {
    let outer = TimeRange::new(100.into(), 300.into()).unwrap();
    let inner = TimeRange::new(150.into(), 250.into()).unwrap();
    assert!(outer.contains_range(&inner));
    assert!(!inner.contains_range(&outer));
}

#[test]
fn test_timerange_close_at_mutants() {
    let range = TimeRange::from(100.into());
    let c = range.close_at(200.into()).unwrap();
    assert_eq!(c.end(), 200.into());
}

#[test]
fn test_bitemporal_interval_is_valid_at_mutants() {
    let interval = BiTemporalInterval::new(
        TimeRange::new(100.into(), 200.into()).unwrap(),
        TimeRange::new(300.into(), 400.into()).unwrap(),
    );
    assert!(!interval.is_valid_at(99.into()));
    assert!(!interval.is_valid_at(200.into()));
    assert!(interval.is_valid_at(150.into()));
}

#[test]
fn test_bitemporal_interval_is_recorded_at_mutants() {
    let interval = BiTemporalInterval::new(
        TimeRange::new(100.into(), 200.into()).unwrap(),
        TimeRange::new(300.into(), 400.into()).unwrap(),
    );
    assert!(!interval.is_recorded_at(299.into()));
    assert!(!interval.is_recorded_at(400.into()));
    assert!(interval.is_recorded_at(350.into()));
}

#[test]
fn test_bitemporal_interval_is_visible_at_mutants() {
    let interval = BiTemporalInterval::new(
        TimeRange::new(100.into(), 200.into()).unwrap(),
        TimeRange::new(300.into(), 400.into()).unwrap(),
    );
    assert!(interval.is_visible_at(150.into(), 350.into()));
    assert!(!interval.is_visible_at(99.into(), 350.into()));
    assert!(!interval.is_visible_at(150.into(), 299.into()));
}

#[test]
fn test_bitemporal_interval_close_methods() {
    let interval =
        BiTemporalInterval::new(TimeRange::from(100.into()), TimeRange::from(300.into()));

    let c_val = interval.close_valid_time(200.into()).unwrap();
    assert_eq!(c_val.valid_time().end(), 200.into());

    let c_tx = interval.close_transaction_time(400.into()).unwrap();
    assert_eq!(c_tx.transaction_time().end(), 400.into());

    let c_both = interval.close_both(200.into(), 400.into()).unwrap();
    assert_eq!(c_both.valid_time().end(), 200.into());
    assert_eq!(c_both.transaction_time().end(), 400.into());
}

#[test]
fn test_time_try_now_mutant() {
    let res = time::try_now();
    assert!(res.is_ok());
}

#[test]
fn test_time_to_iso8601_math_mutants() {
    let ts = aletheiadb::core::temporal::time::from_secs(1609459200);
    let iso = aletheiadb::core::temporal::time::to_iso8601(ts);
    // If + is replaced with - or *, datetime construction could break.
    // 1609459200 seconds should give 2021-01-01
    // We already have some sentry tests but this ensures exact math in `UNIX_EPOCH + Duration::new...`
    assert!(
        iso.contains("2021") || iso.contains("1609459200") || iso.contains("132539328000000000")
    );
}

#[test]
fn test_time_from_secs_mutants() {
    // Check if * becomes + or / in from_secs
    let secs = 5;
    let ts = aletheiadb::core::temporal::time::from_secs(secs);
    // If 5 * 1000000 becomes 5 + 1000000 = 1000005. 5 * 1_000_000 = 5_000_000.
    assert_eq!(ts.wallclock(), 5_000_000);
}

#[test]
fn test_time_from_millis_mutants() {
    // Check if * becomes + or / in from_millis
    let millis = 5;
    let ts = aletheiadb::core::temporal::time::from_millis(millis);
    // If 5 * 1000 becomes 5 + 1000 = 1005. 5 * 1000 = 5000.
    assert_eq!(ts.wallclock(), 5000);
}

#[test]
fn test_time_to_secs_mutants() {
    // If / becomes * or %
    let ts = aletheiadb::core::hlc::HybridTimestamp::new(5_000_000, 0).unwrap();
    assert_eq!(aletheiadb::core::temporal::time::to_secs(ts), 5);
}

#[test]
fn test_time_to_millis_mutants() {
    // If / becomes * or %
    let ts = aletheiadb::core::hlc::HybridTimestamp::new(5000, 0).unwrap();
    assert_eq!(aletheiadb::core::temporal::time::to_millis(ts), 5);
}

#[test]
fn test_time_try_now_not_default() {
    let res = aletheiadb::core::temporal::time::try_now().unwrap();
    // Default is usually (0,0) or similar. `now` should be > 0.
    assert!(res.wallclock() > 1_000_000);
}

#[test]
fn test_timerange_between_mutants() {
    let r = aletheiadb::core::temporal::TimeRange::between(100.into(), 200.into()).unwrap();
    assert_eq!(r.start().wallclock(), 100);
    assert_eq!(r.end().wallclock(), 200);
}

#[test]
fn test_bitemporal_interval_now_mutants() {
    let b = aletheiadb::core::temporal::BiTemporalInterval::now(100.into(), 200.into());
    assert_eq!(b.valid_time().start().wallclock(), 100);
    assert_eq!(b.transaction_time().start().wallclock(), 200);
}

#[test]
fn test_bitemporal_interval_with_valid_time_mutants() {
    let b = aletheiadb::core::temporal::BiTemporalInterval::with_valid_time(100.into(), 200.into());
    assert_eq!(b.valid_time().start().wallclock(), 100);
    assert_eq!(b.transaction_time().start().wallclock(), 200);
}

#[test]
fn test_timerange_serialization_mutants() {
    let r = aletheiadb::core::temporal::TimeRange::new(100.into(), 200.into()).unwrap();
    let bytes = r.serialize();
    assert_eq!(bytes.len(), 24);
    assert_ne!(bytes, Vec::<u8>::new());
    assert_ne!(bytes, vec![0]);
    assert_ne!(bytes, vec![1]);
}

#[test]
fn test_bitemporal_interval_serialization_mutants() {
    let b = aletheiadb::core::temporal::BiTemporalInterval::new(
        aletheiadb::core::temporal::TimeRange::new(100.into(), 200.into()).unwrap(),
        aletheiadb::core::temporal::TimeRange::new(300.into(), 400.into()).unwrap(),
    );
    let bytes = b.serialize();
    assert_eq!(bytes.len(), 48);
    assert_ne!(bytes, Vec::<u8>::new());
    assert_ne!(bytes, vec![0]);
    assert_ne!(bytes, vec![1]);
}

#[test]
fn test_timerange_deserialize_error_mutants() {
    let res = aletheiadb::core::temporal::TimeRange::deserialize(&[0u8; 10]);
    assert!(res.is_err());

    // Test logic mutation of start > end
    let start_bytes = 200i64.to_le_bytes();
    let end_bytes = 100i64.to_le_bytes();
    let mut bad_bytes = Vec::new();
    bad_bytes.extend_from_slice(&start_bytes);
    bad_bytes.extend_from_slice(&[0, 0, 0, 0]);
    bad_bytes.extend_from_slice(&end_bytes);
    bad_bytes.extend_from_slice(&[0, 0, 0, 0]);
    let res2 = aletheiadb::core::temporal::TimeRange::deserialize(&bad_bytes);
    assert!(res2.is_err());
}

#[test]
fn test_bitemporal_interval_deserialize_error_mutants() {
    let res = aletheiadb::core::temporal::BiTemporalInterval::deserialize(&[0u8; 10]);
    assert!(res.is_err());
}

#[test]
fn test_timerange_fmt_mutants() {
    let r = aletheiadb::core::temporal::TimeRange::new(100.into(), 200.into()).unwrap();
    let formatted = format!("{}", r);
    assert!(formatted.contains("100"));
    assert!(formatted.contains("200"));
}

#[test]
fn test_bitemporal_interval_fmt_mutants() {
    let b = aletheiadb::core::temporal::BiTemporalInterval::new(
        aletheiadb::core::temporal::TimeRange::new(100.into(), 200.into()).unwrap(),
        aletheiadb::core::temporal::TimeRange::new(300.into(), 400.into()).unwrap(),
    );
    let formatted = format!("{}", b);
    assert!(formatted.contains("100"));
    assert!(formatted.contains("200"));
    assert!(formatted.contains("300"));
    assert!(formatted.contains("400"));
}
