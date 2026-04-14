use aletheiadb::core::error::TemporalError;
use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::temporal::{MAX_VALID_TIMESTAMP, TIMESTAMP_MAX, TimeRange};

#[test]
#[should_panic(expected = "Start timestamp exceeds MAX_VALID_TIMESTAMP")]
fn test_timerange_from_panics_on_invalid_timestamp() {
    let invalid_wallclock = MAX_VALID_TIMESTAMP + 1;
    let invalid_ts: HybridTimestamp = invalid_wallclock.into();
    let _ = TimeRange::from(invalid_ts);
}

#[test]
#[should_panic(expected = "Timestamp exceeds MAX_VALID_TIMESTAMP")]
fn test_timerange_at_panics_on_invalid_timestamp() {
    let invalid_wallclock = MAX_VALID_TIMESTAMP + 1;
    let invalid_ts: HybridTimestamp = invalid_wallclock.into();
    let _ = TimeRange::at(invalid_ts);
}

#[test]
fn test_timerange_close_at_returns_error_on_invalid_timestamp() {
    let invalid_wallclock = MAX_VALID_TIMESTAMP + 1;
    let invalid_ts: HybridTimestamp = invalid_wallclock.into();

    let valid_start = HybridTimestamp::new(100, 0).unwrap();
    let range = TimeRange::from(valid_start);

    let result = range.close_at(invalid_ts);
    assert!(result.is_err());

    match result {
        Err(TemporalError::InvalidTimestamp { .. }) => (),
        _ => panic!("Expected InvalidTimestamp error, got {:?}", result),
    }
}

#[test]
fn test_timerange_methods_accept_timestamp_max() {
    // TIMESTAMP_MAX is a special sentinel that should be accepted
    let _ = TimeRange::from(TIMESTAMP_MAX);
    let _ = TimeRange::at(TIMESTAMP_MAX);

    let valid_start = HybridTimestamp::new(100, 0).unwrap();
    let range = TimeRange::from(valid_start);
    let closed = range.close_at(TIMESTAMP_MAX).unwrap();
    assert!(closed.is_current());
}
