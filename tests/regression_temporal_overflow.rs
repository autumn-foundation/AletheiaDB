use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::temporal::{TIMESTAMP_MAX, TimeRange};

#[test]
fn test_timerange_duration_overflow() {
    // Construct a range with start i64::MIN and end 0
    // to force overflow in duration calculation.

    let start_val = i64::MIN;
    let end_val = 0;

    // duration = end - start = 0 - (i64::MIN) = -(i64::MIN)
    // -i64::MIN is 2^63.
    // i64::MAX is 2^63 - 1.
    // So this must overflow i64.

    let start = HybridTimestamp::new(start_val, 0).unwrap();
    let end = HybridTimestamp::new(end_val, 0).unwrap();

    let range = TimeRange::new(start, end).unwrap();

    // Should return Some(i64::MAX) due to saturation
    assert_eq!(range.duration_micros(), Some(i64::MAX));
}

#[test]
fn test_timerange_new_with_timestamp_max() {
    let start = HybridTimestamp::new(100, 0).unwrap();
    // Should be able to create a range ending at TIMESTAMP_MAX via new()
    // This validates the `&& end != TIMESTAMP_MAX` check in new()
    let range = TimeRange::new(start, TIMESTAMP_MAX);
    assert!(range.is_ok());
    assert!(range.unwrap().is_current());
}
