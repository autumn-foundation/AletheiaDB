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

    // Test a specific mutant that returns Some(0) or Some(1) instead of exact calculation
    let start_ts2 = 5_000_000.into();
    let end_ts2 = 8_000_000.into();
    let range2 = aletheiadb::core::temporal::TimeRange::new(start_ts2, end_ts2).unwrap();
    assert_eq!(range2.duration_micros(), Some(3_000_000));
    assert_ne!(range2.duration_micros(), Some(0));
    assert_ne!(range2.duration_micros(), Some(1));
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
fn test_timerange_is_empty_exact() {
    use aletheiadb::core::temporal::TimeRange;
    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();
    let r2 = TimeRange::new(100.into(), 100.into()).unwrap();

    // Explicitly testing exact `==` vs `!=` mutant
    let r1_empty = r1.is_empty();
    assert!(
        !r1_empty,
        "r1 should not be empty (specifically preventing mutant returning true)"
    );
    let r2_empty = r2.is_empty();
    assert!(
        r2_empty,
        "r2 should be empty (specifically preventing mutant returning false)"
    );
}

#[test]
fn test_timerange_is_current_closed_exact() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{MAX_VALID_TIMESTAMP, TimeRange};
    let open = TimeRange::from(100.into());
    let closed = TimeRange::new(100.into(), 200.into()).unwrap();
    let edge_closed = TimeRange::new(
        100.into(),
        HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap(),
    )
    .unwrap();

    // is_current -> end == TIMESTAMP_MAX
    let open_curr = open.is_current();
    assert!(
        open_curr,
        "open range should be current (prevent false mutant)"
    );
    let closed_curr = closed.is_current();
    assert!(
        !closed_curr,
        "closed range should not be current (prevent true mutant)"
    );
    let edge_curr = edge_closed.is_current();
    assert!(
        !edge_curr,
        "range ending right before TIMESTAMP_MAX should not be current"
    );

    // is_closed -> end < TIMESTAMP_MAX
    let open_closed = open.is_closed();
    assert!(!open_closed, "open range should not be closed");
    assert!(closed.is_closed(), "closed range should be closed");
    assert!(
        edge_closed.is_closed(),
        "range ending right before TIMESTAMP_MAX should be closed"
    );
}

#[test]
fn test_timerange_close_at_exact() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{MAX_VALID_TIMESTAMP, TIMESTAMP_MAX, TimeRange};

    let start = HybridTimestamp::new(100, 0).unwrap();
    let range = TimeRange::from(start);

    // Exact equal test (should not error, produces empty range)
    let closed_exact = range.close_at(start).unwrap();
    assert_eq!(closed_exact.start(), start);
    assert_eq!(closed_exact.end(), start);
    assert!(closed_exact.is_empty());

    // Less than test (should error)
    let err_ts = HybridTimestamp::new(99, 0).unwrap();
    assert!(
        range.close_at(err_ts).is_err(),
        "Closing at a time before start should error"
    );

    // MAX_VALID_TIMESTAMP exactly (should succeed)
    let max_valid = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap();
    assert!(
        range.close_at(max_valid).is_ok(),
        "Closing exactly at MAX_VALID_TIMESTAMP should succeed"
    );

    // MAX_VALID_TIMESTAMP + 1 (should error)
    // Note: since the constructor prevents this, we bypass it via unchecked or create a dummy HybridTimestamp
    // Wait, HybridTimestamp::new(MAX_VALID_TIMESTAMP + 1) will fail.
    // However, the `close_at` logic explicitly checks `end.wallclock() > MAX_VALID_TIMESTAMP && end != TIMESTAMP_MAX`.
    // Let's test with `TIMESTAMP_MAX` (should succeed)
    assert!(
        range.close_at(TIMESTAMP_MAX).is_ok(),
        "Closing with TIMESTAMP_MAX should succeed"
    );
}

#[test]
fn test_timerange_contains_range_exact() {
    use aletheiadb::core::temporal::TimeRange;
    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();

    // Testing inner `self.start <= other.start && other.end <= self.end` boundaries
    let exact_match = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(
        r1.contains_range(&exact_match),
        "range should contain exact match"
    );

    let past_start = TimeRange::new(99.into(), 200.into()).unwrap();
    assert!(
        !r1.contains_range(&past_start),
        "range should not contain something starting before it"
    );

    let past_end = TimeRange::new(100.into(), 201.into()).unwrap();
    assert!(
        !r1.contains_range(&past_end),
        "range should not contain something ending after it"
    );

    let partial_inside = TimeRange::new(101.into(), 199.into()).unwrap();
    assert!(
        r1.contains_range(&partial_inside),
        "range should contain internal subset"
    );
}

#[test]
fn test_timerange_serialization_exact() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    let start = HybridTimestamp::new(100, 1).unwrap();
    let end = HybridTimestamp::new(200, 2).unwrap();
    let range = TimeRange::new(start, end).unwrap();

    let bytes = range.serialize();
    // length check (24 bytes for two HybridTimestamps)
    assert_eq!(bytes.len(), 24);

    let (deserialized, size) = TimeRange::deserialize(&bytes).unwrap();
    assert_eq!(size, 24);
    assert_eq!(deserialized.start(), start);
    assert_eq!(deserialized.end(), end);

    // Test buffer too small for deserialize
    let too_small = &bytes[0..23];
    assert!(TimeRange::deserialize(too_small).is_err());

    // Test inverted range
    let inverted_start = HybridTimestamp::new(200, 0).unwrap();
    let inverted_end = HybridTimestamp::new(100, 0).unwrap();

    // We construct the inverted range bytes manually to bypass `new()` checks
    let mut inverted_bytes = inverted_start.serialize();
    inverted_bytes.extend(inverted_end.serialize());

    // Deserialize should fail exactly on `end < start` boundary
    assert!(TimeRange::deserialize(&inverted_bytes).is_err());
}

#[test]
fn test_bitemporal_serialization_exact() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::BiTemporalInterval;

    let valid_ts = HybridTimestamp::new(100, 1).unwrap();
    let tx_ts = HybridTimestamp::new(200, 2).unwrap();

    let interval = BiTemporalInterval::with_valid_time(valid_ts, tx_ts);

    let bytes = interval.serialize();
    assert_eq!(bytes.len(), 48); // 2 TimeRanges * 24 bytes

    let (deserialized, size) = BiTemporalInterval::deserialize(&bytes).unwrap();
    assert_eq!(size, 48);
    assert_eq!(deserialized.valid_time(), interval.valid_time());
    assert_eq!(deserialized.transaction_time(), interval.transaction_time());

    // Test buffer too small for deserialize
    let too_small = &bytes[0..47];
    assert!(BiTemporalInterval::deserialize(too_small).is_err());
}

#[test]
fn test_bitemporal_close_exact() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX};

    let valid_start = HybridTimestamp::new(100, 0).unwrap();
    let tx_start = HybridTimestamp::new(200, 0).unwrap();
    let interval = BiTemporalInterval::with_valid_time(valid_start, tx_start);

    let close_ts = HybridTimestamp::new(300, 0).unwrap();

    let closed_valid = interval.close_valid_time(close_ts).unwrap();
    assert_eq!(closed_valid.valid_time().end(), close_ts);
    assert_eq!(closed_valid.transaction_time().end(), TIMESTAMP_MAX);

    let closed_tx = interval.close_transaction_time(close_ts).unwrap();
    assert_eq!(closed_tx.valid_time().end(), TIMESTAMP_MAX);
    assert_eq!(closed_tx.transaction_time().end(), close_ts);

    let closed_both = interval.close_both(close_ts, close_ts).unwrap();
    assert_eq!(closed_both.valid_time().end(), close_ts);
    assert_eq!(closed_both.transaction_time().end(), close_ts);

    // Exact fail values for Default::default()
    assert_ne!(closed_valid.valid_time().end(), TIMESTAMP_MAX);
    assert_ne!(closed_tx.transaction_time().end(), TIMESTAMP_MAX);
    assert_ne!(closed_both.valid_time().end(), TIMESTAMP_MAX);
}

#[test]
fn test_bitemporal_constructors_exact() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX};

    let valid_ts = HybridTimestamp::new(100, 0).unwrap();
    let tx_ts = HybridTimestamp::new(200, 0).unwrap();

    let current = BiTemporalInterval::current(valid_ts);
    assert_eq!(current.valid_time().start(), valid_ts);
    assert_eq!(current.transaction_time().start(), valid_ts);
    assert_eq!(current.valid_time().end(), TIMESTAMP_MAX);
    assert_eq!(current.transaction_time().end(), TIMESTAMP_MAX);

    let now = BiTemporalInterval::now(valid_ts, tx_ts);
    assert_eq!(now.valid_time().start(), valid_ts);
    assert_eq!(now.transaction_time().start(), tx_ts);
    assert_eq!(now.valid_time().end(), TIMESTAMP_MAX);
    assert_eq!(now.transaction_time().end(), TIMESTAMP_MAX);

    let with_valid = BiTemporalInterval::with_valid_time(valid_ts, tx_ts);
    assert_eq!(with_valid.valid_time().start(), valid_ts);
    assert_eq!(with_valid.transaction_time().start(), tx_ts);
    assert_eq!(with_valid.valid_time().end(), TIMESTAMP_MAX);
    assert_eq!(with_valid.transaction_time().end(), TIMESTAMP_MAX);

    // Prevent Default::default() returns
    assert_ne!(
        current.valid_time().start(),
        HybridTimestamp::new(0, 0).unwrap()
    );
    assert_ne!(
        now.valid_time().start(),
        HybridTimestamp::new(0, 0).unwrap()
    );
    assert_ne!(
        with_valid.valid_time().start(),
        HybridTimestamp::new(0, 0).unwrap()
    );
}

#[test]
fn test_temporal_display_exact() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};

    let ts_start = HybridTimestamp::new(100_000, 0).unwrap();
    let ts_end = HybridTimestamp::new(200_000, 0).unwrap();

    let open_range = TimeRange::from(ts_start);
    let closed_range = TimeRange::new(ts_start, ts_end).unwrap();

    // TimeRange formatting
    let open_fmt = format!("{}", open_range);
    let closed_fmt = format!("{}", closed_range);

    assert!(
        open_fmt.contains("current"),
        "Open range should contain infinity symbol: {}",
        open_fmt
    );
    assert!(
        !closed_fmt.contains("current"),
        "Closed range should not contain infinity symbol: {}",
        closed_fmt
    );

    // Ensure it doesn't return Ok(Default::default())
    assert!(!open_fmt.is_empty());
    assert!(!closed_fmt.is_empty());

    // BiTemporalInterval formatting
    let interval = BiTemporalInterval::new(open_range, closed_range);
    let interval_fmt = format!("{}", interval);

    assert!(
        interval_fmt.contains("valid:"),
        "BiTemporalInterval should contain VT prefix: {}",
        interval_fmt
    );
    assert!(
        interval_fmt.contains("tx:"),
        "BiTemporalInterval should contain TT prefix: {}",
        interval_fmt
    );
    assert!(!interval_fmt.is_empty());
}

#[test]
fn test_time_try_now_exact() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::time;

    let now_result = time::try_now();
    assert!(now_result.is_ok(), "try_now should return Ok");

    let now = now_result.unwrap();
    // Verify it doesn't return Ok(Default::default())
    let default_ts = HybridTimestamp::new(0, 0).unwrap();
    assert_ne!(
        now, default_ts,
        "try_now should not return default timestamp"
    );

    let now2 = time::now();
    assert_ne!(now2, default_ts, "now should not return default timestamp");
}

#[test]
fn test_time_from_secs_millis_exact() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::time;

    let secs = 5;
    let ts_secs = time::from_secs(secs);
    let expected_secs = HybridTimestamp::new(5_000_000, 0).unwrap();
    assert_eq!(
        ts_secs, expected_secs,
        "from_secs arithmetic should be exact (secs * 1_000_000)"
    );

    let millis = 5000;
    let ts_millis = time::from_millis(millis);
    let expected_millis = HybridTimestamp::new(5_000_000, 0).unwrap();
    assert_eq!(
        ts_millis, expected_millis,
        "from_millis arithmetic should be exact (millis * 1000)"
    );

    let default_ts = HybridTimestamp::new(0, 0).unwrap();
    assert_ne!(ts_secs, default_ts, "from_secs should not return default");
    assert_ne!(
        ts_millis, default_ts,
        "from_millis should not return default"
    );
}
