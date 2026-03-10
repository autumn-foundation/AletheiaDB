use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX, TimeRange, time};

#[test]
fn test_temporal_exact_math_bounds() {
    let ts = HybridTimestamp::new(5_000_000, 0).unwrap();
    assert_eq!(time::to_secs(ts), 5);
    assert_ne!(time::to_secs(ts), 0); // Not default

    assert_eq!(time::to_millis(ts), 5000);
    assert_ne!(time::to_millis(ts), 0); // Not default

    // Check iso8601 formatting default values and max boundaries
    assert_eq!(time::to_iso8601(TIMESTAMP_MAX), "current");
    assert_ne!(time::to_iso8601(ts), "current");
    assert!(!time::to_iso8601(ts).is_empty());
}

#[test]
fn test_timerange_boundaries() {
    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();

    // Contains exclusive end boundary
    assert!(r1.contains(199.into()));
    assert!(!r1.contains(200.into())); // strict <

    // Overlaps specific boundaries
    let mut other = TimeRange::new(199.into(), 300.into()).unwrap();
    assert!(r1.overlaps(&other));

    other = TimeRange::new(200.into(), 300.into()).unwrap();
    assert!(!r1.overlaps(&other)); // exactly touching is not overlapping

    // contains_or_after
    assert!(r1.contains_or_after(100.into()));
    assert!(r1.contains_or_after(200.into()));
    assert!(!r1.contains_or_after(99.into()));
}

#[test]
fn test_timerange_closures_and_defaults() {
    let open_range = TimeRange::from(100.into());
    assert!(open_range.is_current());
    assert!(!open_range.is_closed());

    let closed_range = open_range.close_at(200.into()).unwrap();
    assert!(!closed_range.is_current());
    assert!(closed_range.is_closed());
    assert_eq!(closed_range.end(), 200.into());

    // Bitemporal current closures
    let bt = BiTemporalInterval::current(100.into());
    assert!(bt.is_currently_valid());
    assert!(bt.is_currently_recorded());
    assert!(bt.is_current());

    let valid_closed = bt.close_valid_time(200.into()).unwrap();
    assert!(!valid_closed.is_currently_valid());
    assert!(valid_closed.is_currently_recorded());
    assert!(!valid_closed.is_current());

    let tx_closed = bt.close_transaction_time(200.into()).unwrap();
    assert!(tx_closed.is_currently_valid());
    assert!(!tx_closed.is_currently_recorded());
    assert!(!tx_closed.is_current());
}

#[test]
fn test_duration_exact_returns() {
    let r_empty = TimeRange::new(100.into(), 100.into()).unwrap();
    assert_eq!(r_empty.duration_micros(), Some(0)); // EXACT 0 check

    let r_normal = TimeRange::new(100.into(), 200.into()).unwrap();
    assert_eq!(r_normal.duration_micros(), Some(100)); // EXACT value check

    let r_open = TimeRange::from(100.into());
    assert_eq!(r_open.duration_micros(), None); // EXACT None check
}
