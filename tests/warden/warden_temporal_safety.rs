//! Warden: Temporal Safety Audit Tests
//!
//! These tests verify that the system correctly rejects invalid temporal states
//! and prevents time-travel paradoxes or DoS attacks via malicious timestamps.

use aletheiadb::core::error::TemporalError;
use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::temporal::{MAX_VALID_TIMESTAMP, TIMESTAMP_MAX, TimeRange};

/// 🎯 Target: TimeRange::new validation
/// 💣 Risk: Creating TimeRange with invalid timestamps via unchecked constructors
///    could allow invalid state to propagate through the system, potentially
///    causing panics or logical errors in query execution.
/// 🧪 Strategy: Construct invalid timestamps using From<i64> (which bypasses validation)
///    and attempt to create a TimeRange.
/// 🔬 Verification: TimeRange::new should return Err(InvalidTimestamp).
#[test]
fn test_warden_timerange_rejects_invalid_timestamps() {
    // Create an invalid timestamp (one past MAX_VALID_TIMESTAMP)
    // using the internal new_unchecked via the From<i64> impl.
    let invalid_val = MAX_VALID_TIMESTAMP + 1;
    let invalid_ts: HybridTimestamp = invalid_val.into();

    // Attempt to create a TimeRange with the invalid start timestamp
    let result = TimeRange::new(invalid_ts, TIMESTAMP_MAX);

    // Should fail with InvalidTimestamp error
    assert!(
        result.is_err(),
        "TimeRange::new accepted invalid start timestamp"
    );

    match result {
        Err(TemporalError::InvalidTimestamp { timestamp, .. }) => {
            assert_eq!(timestamp.wallclock(), invalid_val);
        }
        _ => panic!("Expected InvalidTimestamp error, got {:?}", result),
    }

    // Attempt to create a TimeRange with the invalid end timestamp
    let valid_ts: HybridTimestamp = 100.into();
    let result = TimeRange::new(valid_ts, invalid_ts);

    // Should fail with InvalidTimestamp error
    assert!(
        result.is_err(),
        "TimeRange::new accepted invalid end timestamp"
    );

    match result {
        Err(TemporalError::InvalidTimestamp { timestamp, .. }) => {
            assert_eq!(timestamp.wallclock(), invalid_val);
        }
        _ => panic!("Expected InvalidTimestamp error, got {:?}", result),
    }
}
