
use aletheiadb::core::hlc::{evaluate_clock_skew, ClockSkewDirection};
use aletheiadb::core::temporal::{TimeRange, MAX_VALID_TIMESTAMP, Timestamp};

#[test]
fn test_evaluate_clock_skew_overflow() {
    // This represents a scenario where the system clock is extremely messed up (before epoch)
    // and the frontier is far in the future (near MAX_VALID_TIMESTAMP).
    // The implementation MUST NOT panic and should report a violation.
    let current_wallclock = i64::MIN;
    let frontier_wallclock = MAX_VALID_TIMESTAMP;

    // We expect this NOT to panic, but to return a violation due to massive backward drift.
    let result = evaluate_clock_skew(current_wallclock, frontier_wallclock, None, false);

    match result {
        Ok(_) => panic!("Expected ClockSkewViolation, got Ok"),
        Err(violation) => {
            assert_eq!(violation.direction, ClockSkewDirection::Backward);
            // The drift should be saturated to i64::MIN because current << frontier
            assert!(violation.drift_us < -1_000_000_000);
        }
    }
}

#[test]
fn test_evaluate_clock_skew_forward_overflow() {
    // This represents a scenario where the frontier is far in the past (e.g. ancient backup restored)
    // and the current wallclock is far in the future (or system clock is wrong).
    // drift = current - frontier = MAX - MIN = Overflow.
    let current_wallclock = MAX_VALID_TIMESTAMP;
    let frontier_wallclock = i64::MIN;

    // We expect this NOT to panic, but to return a violation due to massive forward drift.
    // We must provide a max_forward_jump_us for a violation to occur, otherwise forward drift is allowed.
    let result = evaluate_clock_skew(current_wallclock, frontier_wallclock, Some(1000), false);

    match result {
        Ok(_) => panic!("Expected ClockSkewViolation, got Ok"),
        Err(violation) => {
            assert_eq!(violation.direction, ClockSkewDirection::Forward);
            // The drift should be saturated to i64::MAX because current >> frontier
            assert!(violation.drift_us > 1_000_000_000);
        }
    }
}

#[test]
fn test_time_range_duration_overflow() {
    // Create a range spanning from i64::MIN to MAX_VALID_TIMESTAMP.
    // This duration exceeds i64::MAX but fits in u64.
    // Note: i64::MIN is a valid wallclock (before 1970).

    let start_ts: Timestamp = i64::MIN.into();
    let end_ts: Timestamp = MAX_VALID_TIMESTAMP.into();

    let range = TimeRange::new(start_ts, end_ts).expect("Should create valid range");

    // Expect the duration to be roughly u64::MAX - 1000 + i64::MIN.abs()
    // i64::MIN is -9223372036854775808.
    // MAX_VALID_TIMESTAMP is 9223372036854775807 - 1000 = 9223372036854774807.
    // Duration = 9223372036854774807 - (-9223372036854775808) = 18446744073709550615.

    // Check type is u64
    let duration: Option<u64> = range.duration_micros();
    let duration = duration.expect("Should return duration");

    assert!(duration > i64::MAX as u64); // Confirm it exceeds i64 range
}
