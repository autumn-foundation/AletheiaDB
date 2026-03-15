#[test]
fn test_timerange_duration_micros_exact() {
    let start_ts = 150_000.into();
    let end_ts = 250_000.into();

    // Test a closed range
    let range = aletheiadb::core::temporal::TimeRange::new(start_ts, end_ts).unwrap();
    assert_eq!(range.duration_micros(), Some(100_000));

    // Test point in time
    let point = aletheiadb::core::temporal::TimeRange::at(start_ts);
    assert_eq!(point.duration_micros(), Some(0));

    // Test current range
    let current = aletheiadb::core::temporal::TimeRange::from(start_ts);
    assert_eq!(current.duration_micros(), None);
}

#[test]
fn test_bitemporal_methods_exact() {
    use aletheiadb::core::temporal::BiTemporalInterval;
    use aletheiadb::core::temporal::TimeRange;

    let valid_ts = 100_000.into();
    let tx_ts = 200_000.into();

    let interval_now = BiTemporalInterval::now(valid_ts, tx_ts);
    assert_eq!(interval_now.valid_time(), TimeRange::from(valid_ts));
    assert_eq!(interval_now.transaction_time(), TimeRange::from(tx_ts));

    let interval_current = BiTemporalInterval::current(valid_ts);
    assert_eq!(interval_current.valid_time(), TimeRange::from(valid_ts));
    assert_eq!(
        interval_current.transaction_time(),
        TimeRange::from(valid_ts)
    );

    let interval_with_valid = BiTemporalInterval::with_valid_time(valid_ts, tx_ts);
    assert_eq!(interval_with_valid.valid_time(), TimeRange::from(valid_ts));
    assert_eq!(
        interval_with_valid.transaction_time(),
        TimeRange::from(tx_ts)
    );
}

#[test]
fn test_bitemporal_is_currently_methods() {
    use aletheiadb::core::temporal::BiTemporalInterval;
    use aletheiadb::core::temporal::TimeRange;

    let valid_start = 1000.into();
    let valid_end = 2000.into();
    let tx_start = 3000.into();
    let tx_end = 4000.into();

    // Both closed
    let interval_closed = BiTemporalInterval::new(
        TimeRange::new(valid_start, valid_end).unwrap(),
        TimeRange::new(tx_start, tx_end).unwrap(),
    );
    assert!(!interval_closed.is_currently_valid());
    assert!(!interval_closed.is_currently_recorded());
    assert!(!interval_closed.is_current());

    // Only valid open
    let interval_valid_open = BiTemporalInterval::new(
        TimeRange::from(valid_start),
        TimeRange::new(tx_start, tx_end).unwrap(),
    );
    assert!(interval_valid_open.is_currently_valid());
    assert!(!interval_valid_open.is_currently_recorded());
    assert!(!interval_valid_open.is_current());

    // Only tx open
    let interval_tx_open = BiTemporalInterval::new(
        TimeRange::new(valid_start, valid_end).unwrap(),
        TimeRange::from(tx_start),
    );
    assert!(!interval_tx_open.is_currently_valid());
    assert!(interval_tx_open.is_currently_recorded());
    assert!(!interval_tx_open.is_current());

    // Both open
    let interval_both_open =
        BiTemporalInterval::new(TimeRange::from(valid_start), TimeRange::from(tx_start));
    assert!(interval_both_open.is_currently_valid());
    assert!(interval_both_open.is_currently_recorded());
    assert!(interval_both_open.is_current());
}

#[test]
fn test_bitemporal_is_valid_at_and_recorded_at() {
    use aletheiadb::core::temporal::BiTemporalInterval;
    use aletheiadb::core::temporal::TimeRange;

    let valid_start = 1000.into();
    let valid_end = 2000.into();
    let tx_start = 3000.into();
    let tx_end = 4000.into();

    let interval = BiTemporalInterval::new(
        TimeRange::new(valid_start, valid_end).unwrap(),
        TimeRange::new(tx_start, tx_end).unwrap(),
    );

    assert!(interval.is_valid_at(1500.into()));
    assert!(!interval.is_valid_at(500.into()));
    assert!(!interval.is_valid_at(2500.into()));

    assert!(interval.is_recorded_at(3500.into()));
    assert!(!interval.is_recorded_at(2500.into()));
    assert!(!interval.is_recorded_at(4500.into()));

    // Visibility matrix
    assert!(interval.is_visible_at(1500.into(), 3500.into()));
    assert!(!interval.is_visible_at(500.into(), 3500.into()));
    assert!(!interval.is_visible_at(1500.into(), 4500.into()));
    assert!(!interval.is_visible_at(500.into(), 4500.into()));
}

#[test]
fn test_timerange_from_at_exact_boundaries() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{MAX_VALID_TIMESTAMP, TIMESTAMP_MAX, TimeRange};

    // Test exact MAX_VALID_TIMESTAMP boundary
    let exact_max = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap();

    // TimeRange::from should accept exact MAX_VALID_TIMESTAMP
    let range_from_max = TimeRange::from(exact_max);
    assert_eq!(range_from_max.start(), exact_max);
    assert_eq!(range_from_max.end(), TIMESTAMP_MAX);

    // TimeRange::at should accept exact MAX_VALID_TIMESTAMP
    let range_at_max = TimeRange::at(exact_max);
    assert_eq!(range_at_max.start(), exact_max);
    assert_eq!(range_at_max.end(), exact_max);
}

#[test]
fn test_time_to_secs_millis_exact_math() {
    use aletheiadb::core::temporal::time;

    let secs = 5;
    let ts_secs = time::from_secs(secs);
    assert_eq!(
        time::to_secs(ts_secs),
        secs,
        "to_secs should exactly match input"
    );

    let millis = 5000;
    let ts_millis = time::from_millis(millis);
    assert_eq!(
        time::to_millis(ts_millis),
        millis,
        "to_millis should exactly match input"
    );

    // Math mutant checks. If / becomes %, to_secs(5_000_000) = 0.
    // We already check it equals 5, so 0 would fail.
    // If * becomes /, from_secs(5) = 5 / 1000000 = 0.
    // Then to_secs(0) = 0, which fails our assert_eq!(_, 5).
}

#[test]
fn test_time_to_iso8601_exact_content() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::time;

    let ts = time::from_secs(1609459200); // 2021-01-01 00:00:00 UTC
    let output = time::to_iso8601(ts);

    // Assert exact behavior instead of anti-mutant shape.
    // The exact internal representation for SystemTime debug differs per OS.
    if cfg!(windows) {
        assert!(
            output.contains("132539328000000000"),
            "Windows output did not match expected exact interval: {}",
            output
        );
    } else {
        assert!(
            output.contains("1609459200"),
            "Unix output did not match exact seconds timestamp: {}",
            output
        );
    }

    // Test with fractional seconds
    let ts_frac = HybridTimestamp::new(1_609_459_200_123_456, 0).unwrap();
    let output_frac = time::to_iso8601(ts_frac);

    if cfg!(windows) {
        assert!(
            output_frac.contains("1234560"),
            "Windows output did not match exact sub-second interval: {}",
            output_frac
        );
    } else {
        assert!(
            output_frac.contains("123456000"),
            "Unix output did not match exact nanoseconds timestamp: {}",
            output_frac
        );
    }
}

#[test]
fn test_timerange_contains_exact_boundary() {
    use aletheiadb::core::temporal::TimeRange;

    // Check bounds exactly, preventing `>=` replacing `>` in overlaps/contains
    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();
    let r2 = TimeRange::new(200.into(), 300.into()).unwrap();

    // r1 contains logic: start <= ts < end
    assert!(
        !r1.contains(200.into()),
        "Range should NOT contain its exclusive end boundary"
    );

    // contains_or_after: ts >= start
    assert!(
        r1.contains_or_after(100.into()),
        "contains_or_after should be true for exact start"
    );
    assert!(
        !r1.contains_or_after(99.into()),
        "contains_or_after should be false right before start"
    );

    // overlaps logic: self.start < other.end && other.start < self.end
    assert!(
        !r1.overlaps(&r2),
        "Touching ranges should NOT overlap (exact boundary check)"
    );
    assert!(
        !r2.overlaps(&r1),
        "Touching ranges should NOT overlap (exact boundary check reverse)"
    );
}

#[test]
fn test_timerange_contains_range_mutant_bounds() {
    use aletheiadb::core::temporal::TimeRange;

    // Mutants replace && with || and <= with >
    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();

    // Exact inner matching
    let r2 = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(
        r1.contains_range(&r2),
        "Identical range should be contained"
    );

    // Left boundary failure (start > start) - tests left side of &&
    let r3 = TimeRange::new(99.into(), 150.into()).unwrap();
    assert!(
        !r1.contains_range(&r3),
        "Range starting before should NOT be contained"
    );

    // Right boundary failure (end < end) - tests right side of &&
    let r4 = TimeRange::new(150.into(), 201.into()).unwrap();
    assert!(
        !r1.contains_range(&r4),
        "Range ending after should NOT be contained"
    );

    // Entirely outside
    let r5 = TimeRange::new(300.into(), 400.into()).unwrap();
    assert!(
        !r1.contains_range(&r5),
        "Entirely outside range should NOT be contained"
    );
}

#[test]
fn test_timerange_is_current_closed_empty_mutant_bounds() {
    use aletheiadb::core::temporal::TimeRange;

    let current_range = TimeRange::from(100.into());
    let closed_range = TimeRange::new(100.into(), 200.into()).unwrap();
    let empty_range = TimeRange::at(100.into());

    // is_current (end == TIMESTAMP_MAX)
    assert!(
        current_range.is_current(),
        "Current range should be current"
    );
    assert!(
        !closed_range.is_current(),
        "Closed range should NOT be current"
    );

    // is_closed (end < TIMESTAMP_MAX)
    assert!(
        !current_range.is_closed(),
        "Current range should NOT be closed"
    );
    assert!(closed_range.is_closed(), "Closed range should be closed");

    // Mutators on is_closed: replace `<` with `==` or `>`
    // The exact check against `!current_range.is_closed()` specifically catches `==`
    // because current_range.end is exactly TIMESTAMP_MAX.

    // is_empty (start == end)
    assert!(
        !current_range.is_empty(),
        "Current range should NOT be empty"
    );
    assert!(!closed_range.is_empty(), "Closed range should NOT be empty");
    assert!(empty_range.is_empty(), "Empty range should be empty");
}

#[test]
fn test_bitemporal_close_methods_mutants() {
    use aletheiadb::core::error::TemporalError;
    use aletheiadb::core::temporal::BiTemporalInterval;

    let valid_start = 1000.into();
    let tx_start = 2000.into();

    let interval = BiTemporalInterval::now(valid_start, tx_start);

    // Test close_valid_time
    let close_valid_ts = 1500.into();
    let closed_valid = interval.close_valid_time(close_valid_ts).unwrap();
    assert_eq!(
        closed_valid.valid_time().end(),
        close_valid_ts,
        "valid time should be exactly updated"
    );
    assert!(
        closed_valid.is_currently_recorded(),
        "tx time should remain open"
    );

    // Test close_transaction_time
    let close_tx_ts = 2500.into();
    let closed_tx = interval.close_transaction_time(close_tx_ts).unwrap();
    assert_eq!(
        closed_tx.transaction_time().end(),
        close_tx_ts,
        "tx time should be exactly updated"
    );
    assert!(
        closed_tx.is_currently_valid(),
        "valid time should remain open"
    );

    // Test close_both
    let closed_both = interval.close_both(close_valid_ts, close_tx_ts).unwrap();
    assert_eq!(
        closed_both.valid_time().end(),
        close_valid_ts,
        "valid time should be exactly updated"
    );
    assert_eq!(
        closed_both.transaction_time().end(),
        close_tx_ts,
        "tx time should be exactly updated"
    );
    assert!(
        !closed_both.is_currently_valid() && !closed_both.is_currently_recorded(),
        "both should be closed"
    );

    // Invalid closures should return TemporalError exactly rather than panicking or Ok(Default)
    let invalid_close_ts = 500.into(); // Before start
    assert!(
        matches!(
            interval.close_valid_time(invalid_close_ts),
            Err(TemporalError::InvalidTimeRange { .. })
        ),
        "Closing before start should return InvalidTimeRange"
    );
}

#[test]
fn test_bitemporal_serialization_mutants() {
    use aletheiadb::core::temporal::BiTemporalInterval;

    let interval = BiTemporalInterval::now(1000.into(), 2000.into());
    let serialized = interval.serialize();

    // Kill mutants replacing serialize body with vec![] or vec![0]/vec![1]
    assert_eq!(
        serialized.len(),
        48,
        "BiTemporalInterval serialization length must be exactly 48 bytes"
    );

    // Verify deserialization limits
    let mut bad_buffer = serialized.clone();
    bad_buffer.truncate(47); // Just 1 byte short
    assert!(
        BiTemporalInterval::deserialize(&bad_buffer).is_err(),
        "Deserialization of < 48 bytes must return error"
    );
}

#[test]
fn test_timerange_serialization_mutants() {
    use aletheiadb::core::temporal::TimeRange;

    let range = TimeRange::from(1000.into());
    let serialized = range.serialize();

    // Kill mutants replacing serialize body with vec![] or vec![0]/vec![1]
    assert_eq!(
        serialized.len(),
        24,
        "TimeRange serialization length must be exactly 24 bytes"
    );

    // Verify deserialization limits
    let mut bad_buffer = serialized.clone();
    bad_buffer.truncate(23); // Just 1 byte short
    assert!(
        TimeRange::deserialize(&bad_buffer).is_err(),
        "Deserialization of < 24 bytes must return error"
    );
}
