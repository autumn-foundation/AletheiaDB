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
fn test_timerange_is_empty_exact() {
    use aletheiadb::core::temporal::TimeRange;
    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();
    let r2 = TimeRange::new(100.into(), 100.into()).unwrap();

    // Explicitly testing exact `==` vs `!=` mutant
    assert!(!r1.is_empty(), "r1 should not be empty");
    assert!(r2.is_empty(), "r2 should be empty");
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
    assert!(open.is_current(), "open range should be current");
    assert!(!closed.is_current(), "closed range should not be current");
    assert!(
        !edge_closed.is_current(),
        "range ending right before TIMESTAMP_MAX should not be current"
    );

    // is_closed -> end < TIMESTAMP_MAX
    assert!(!open.is_closed(), "open range should not be closed");
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

#[test]
fn test_timerange_is_closed_exact_bounds() {
    use aletheiadb::core::temporal::TimeRange;

    // A point in time is implicitly closed unless it is MAX_VALID_TIMESTAMP? Wait, `is_closed` checks `< MAX_VALID_TIMESTAMP`.
    let start = 100_000.into();
    let end = 200_000.into();
    let closed_range = TimeRange::new(start, end).unwrap();
    assert!(
        closed_range.is_closed(),
        "range with end < MAX_VALID_TIMESTAMP should be closed"
    );

    let open_range = TimeRange::from(start);
    assert!(
        !open_range.is_closed(),
        "range with end == MAX_VALID_TIMESTAMP should NOT be closed"
    );
}

#[test]
fn test_timerange_contains_or_after_exact_bounds() {
    use aletheiadb::core::temporal::TimeRange;

    let start = 100_000.into();
    let end = 200_000.into();
    let range = TimeRange::new(start, end).unwrap();

    assert!(
        range.contains_or_after(start),
        "range should contain or be after its start"
    );
    let after_start = 100_001.into();
    assert!(
        range.contains_or_after(after_start),
        "range should contain or be after points after its start"
    );

    let before_start = 99_999.into();
    assert!(
        !range.contains_or_after(before_start),
        "range should not contain or be after points before its start"
    );
}

#[test]
fn test_timerange_overlaps_exact_bounds() {
    use aletheiadb::core::temporal::TimeRange;

    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();
    let empty = TimeRange::at(100.into());

    assert!(!r1.overlaps(&empty), "cannot overlap with empty");
    assert!(!empty.overlaps(&r1), "cannot overlap with empty");
}

#[test]
fn test_bitemporal_is_visible_at_exact_bounds() {
    use aletheiadb::core::temporal::BiTemporalInterval;

    let valid_start = 100.into();
    let tx_start = 200.into();
    let interval = BiTemporalInterval::now(valid_start, tx_start);

    assert!(
        interval.is_visible_at(100.into(), 200.into()),
        "should be visible at exact start bounds"
    );
    assert!(
        !interval.is_visible_at(99.into(), 200.into()),
        "should not be visible before valid start"
    );
    assert!(
        !interval.is_visible_at(100.into(), 199.into()),
        "should not be visible before tx start"
    );
}

#[test]
fn test_time_to_secs_millis_exact_math_strict() {
    use aletheiadb::core::temporal::time;

    // Test exact math (not just round trip, but exact result for a known value)
    let ts_us: i64 = 5_000_000;

    assert_eq!(
        time::to_secs(ts_us.into()),
        5,
        "to_secs should exactly divide by 1_000_000"
    );
    assert_ne!(
        time::to_secs(ts_us.into()),
        0,
        "to_secs should not return default 0"
    );
    assert_ne!(
        time::to_secs(ts_us.into()),
        1,
        "to_secs should not return default 1"
    );
    assert_ne!(
        time::to_secs(ts_us.into()),
        -1,
        "to_secs should not return default -1"
    );

    assert_eq!(
        time::to_millis(ts_us.into()),
        5_000,
        "to_millis should exactly divide by 1_000"
    );
    assert_ne!(
        time::to_millis(ts_us.into()),
        0,
        "to_millis should not return default 0"
    );
    assert_ne!(
        time::to_millis(ts_us.into()),
        1,
        "to_millis should not return default 1"
    );
    assert_ne!(
        time::to_millis(ts_us.into()),
        -1,
        "to_millis should not return default -1"
    );
}

#[test]
fn test_timerange_close_at_invalid_timestamp_strict() {
    use aletheiadb::core::temporal::TimeRange;

    use aletheiadb::core::temporal::MAX_VALID_TIMESTAMP;
    let range = TimeRange::from(100.into());
    let err = range
        .close_at((MAX_VALID_TIMESTAMP + 1).into())
        .unwrap_err();

    match err {
        aletheiadb::core::error::TemporalError::InvalidTimestamp { timestamp, .. } => {
            assert_eq!(timestamp.wallclock(), MAX_VALID_TIMESTAMP + 1);
        }
        _ => panic!("Expected InvalidTimestamp error"),
    }
}

#[test]
fn test_timerange_deserialize_exact_bounds() {
    use aletheiadb::core::temporal::TimeRange;

    // Test exact deserialization byte length boundaries
    // Deserialization expects exactly 24 bytes: 12 for start, 12 for end.
    let range = TimeRange::new(100.into(), 200.into()).unwrap();
    let bytes = range.serialize();

    // Exactly 24 bytes should be OK
    let (_deserialized, read_len) = TimeRange::deserialize(&bytes).unwrap();
    assert_eq!(read_len, 24);

    // Less than 24 bytes should err cleanly
    let err_short = TimeRange::deserialize(&bytes[0..23]).unwrap_err();
    assert!(matches!(
        err_short,
        aletheiadb::core::error::StorageError::CorruptedData(_)
    ));

    // More than 24 bytes should be OK, only reading 24
    let mut long_bytes = bytes.clone();
    long_bytes.push(0xFF);
    let (_, read_len_long) = TimeRange::deserialize(&long_bytes).unwrap();
    assert_eq!(read_len_long, 24);
}

#[test]
fn test_bitemporal_deserialize_exact_bounds() {
    use aletheiadb::core::temporal::BiTemporalInterval;

    let interval = BiTemporalInterval::now(100.into(), 200.into());
    let bytes = interval.serialize();

    // Exactly 48 bytes should be OK
    let (_, read_len) = BiTemporalInterval::deserialize(&bytes).unwrap();
    assert_eq!(read_len, 48);

    // Less than 48 bytes should err cleanly
    let err_short = BiTemporalInterval::deserialize(&bytes[0..47]).unwrap_err();
    assert!(matches!(
        err_short,
        aletheiadb::core::error::StorageError::CorruptedData(_)
    ));
}

#[test]
fn test_timerange_duration_micros_logic_strict() {
    use aletheiadb::core::temporal::TimeRange;
    // duration_micros calculates (end.wallclock - start.wallclock) ignoring logical
    let ts_start = aletheiadb::core::hlc::HybridTimestamp::new(100, 10).unwrap();
    let ts_end = aletheiadb::core::hlc::HybridTimestamp::new(100, 20).unwrap();

    let range = TimeRange::new(ts_start, ts_end).unwrap();

    let duration = range.duration_micros().expect("Duration should be some");
    assert_eq!(
        duration, 0,
        "Duration only measures wallclock differences, ignoring logical counters in microseconds output"
    );

    assert_ne!(duration, 1, "Duration should not be default 1");
    assert_ne!(duration, -1, "Duration should not be default -1");
}

#[test]
fn test_timerange_contains_range_strict_bounds() {
    use aletheiadb::core::temporal::TimeRange;

    // contains_range is true if self.start <= other.start && other.end <= self.end
    // So if self.start > other.start OR other.end > self.end, it's false

    let self_range = TimeRange::new(100.into(), 200.into()).unwrap();

    // Exactly same bounds => contains
    let other_exact = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(
        self_range.contains_range(&other_exact),
        "Must contain exact match"
    );

    // other.start < self.start by exactly 1 => not contains
    let other_early_start = TimeRange::new(99.into(), 200.into()).unwrap();
    assert!(
        !self_range.contains_range(&other_early_start),
        "Must not contain if other starts 1 unit before"
    );

    // other.end > self.end by exactly 1 => not contains
    let other_late_end = TimeRange::new(100.into(), 201.into()).unwrap();
    assert!(
        !self_range.contains_range(&other_late_end),
        "Must not contain if other ends 1 unit after"
    );

    // Inverted bounds just in case (should fail to construct anyway, but `from` allows unbounded end)
    let unbounded = TimeRange::from(150.into());
    assert!(
        !self_range.contains_range(&unbounded),
        "Bounded range cannot contain unbounded range"
    );
}

#[test]
fn test_timerange_contains_range_replace_leq_with_gt_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    let outer = TimeRange::new(
        HybridTimestamp::new(100, 0).unwrap(),
        HybridTimestamp::new(200, 0).unwrap(),
    )
    .unwrap();

    let inner1 = TimeRange::new(
        HybridTimestamp::new(150, 0).unwrap(),
        HybridTimestamp::new(180, 0).unwrap(),
    )
    .unwrap();
    assert!(
        outer.contains_range(&inner1),
        "Mutant test: inner start strictly greater than outer start"
    );

    let not_inner = TimeRange::new(
        HybridTimestamp::new(50, 0).unwrap(),
        HybridTimestamp::new(250, 0).unwrap(),
    )
    .unwrap();
    assert!(
        !outer.contains_range(&not_inner),
        "Mutant test: should not contain overlapping but strictly wider range"
    );

    let left_overlapping = TimeRange::new(
        HybridTimestamp::new(50, 0).unwrap(),
        HybridTimestamp::new(150, 0).unwrap(),
    )
    .unwrap();
    assert!(
        !outer.contains_range(&left_overlapping),
        "Mutant test: && -> || where 1st is false, 2nd is true"
    );

    let right_overlapping = TimeRange::new(
        HybridTimestamp::new(150, 0).unwrap(),
        HybridTimestamp::new(250, 0).unwrap(),
    )
    .unwrap();
    assert!(
        !outer.contains_range(&right_overlapping),
        "Mutant test: && -> || where 1st is true, 2nd is false"
    );
}

#[test]
fn test_timerange_overlaps_mutants_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    let self_range = TimeRange::new(
        HybridTimestamp::new(100, 0).unwrap(),
        HybridTimestamp::new(200, 0).unwrap(),
    )
    .unwrap();

    let overlapping = TimeRange::new(
        HybridTimestamp::new(150, 0).unwrap(),
        HybridTimestamp::new(250, 0).unwrap(),
    )
    .unwrap();
    assert!(self_range.overlaps(&overlapping), "Should overlap");

    let not_overlapping_right = TimeRange::new(
        HybridTimestamp::new(200, 0).unwrap(),
        HybridTimestamp::new(300, 0).unwrap(),
    )
    .unwrap();
    assert!(
        !self_range.overlaps(&not_overlapping_right),
        "Should not overlap"
    );

    let empty_self = TimeRange::at(HybridTimestamp::new(150, 0).unwrap());
    assert!(
        !empty_self.overlaps(&overlapping),
        "Empty range should not overlap"
    );

    let disjoint_right = TimeRange::new(
        HybridTimestamp::new(250, 0).unwrap(),
        HybridTimestamp::new(300, 0).unwrap(),
    )
    .unwrap();
    assert!(
        !self_range.overlaps(&disjoint_right),
        "Disjoint right: && -> || mutant"
    );

    let disjoint_left = TimeRange::new(
        HybridTimestamp::new(10, 0).unwrap(),
        HybridTimestamp::new(50, 0).unwrap(),
    )
    .unwrap();
    assert!(
        !self_range.overlaps(&disjoint_left),
        "Disjoint left: && -> || mutant"
    );

    let left_touch = TimeRange::new(
        HybridTimestamp::new(50, 0).unwrap(),
        HybridTimestamp::new(100, 0).unwrap(),
    )
    .unwrap();
    assert!(
        !self_range.overlaps(&left_touch),
        "Mutant test: replace < with <= (self.start < other.end)"
    );

    let right_touch = TimeRange::new(
        HybridTimestamp::new(200, 0).unwrap(),
        HybridTimestamp::new(250, 0).unwrap(),
    )
    .unwrap();
    assert!(
        !self_range.overlaps(&right_touch),
        "Mutant test: replace < with <= (other.start < self.end)"
    );

    assert!(
        !self_range.overlaps(&disjoint_left),
        "Mutant test: replace < with > (self.start < other.end)"
    );

    assert!(
        self_range.overlaps(&overlapping),
        "Mutant test: replace < with =="
    );
}

#[test]
fn test_timerange_contains_mutants_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    let range = TimeRange::new(
        HybridTimestamp::new(100, 0).unwrap(),
        HybridTimestamp::new(200, 0).unwrap(),
    )
    .unwrap();

    assert!(
        range.contains(HybridTimestamp::new(150, 0).unwrap()),
        "Should contain"
    );
    assert!(
        !range.contains(HybridTimestamp::new(250, 0).unwrap()),
        "Should not contain"
    );
    assert!(
        !range.contains(HybridTimestamp::new(50, 0).unwrap()),
        "Should not contain"
    );

    assert!(
        !range.contains(HybridTimestamp::new(50, 0).unwrap()),
        "Mutant test: && -> || (50)"
    );
    assert!(
        !range.contains(HybridTimestamp::new(250, 0).unwrap()),
        "Mutant test: && -> || (250)"
    );

    assert!(
        !range.contains(HybridTimestamp::new(50, 0).unwrap()),
        "Mutant test: >= -> <"
    );

    assert!(
        !range.contains(HybridTimestamp::new(200, 0).unwrap()),
        "Mutant test: < -> <= in contains"
    );

    assert!(
        !range.contains(HybridTimestamp::new(200, 0).unwrap()),
        "Mutant test: < -> == in contains"
    );
    assert!(
        range.contains(HybridTimestamp::new(150, 0).unwrap()),
        "Mutant test: < -> == (inner)"
    );

    assert!(
        !range.contains(HybridTimestamp::new(250, 0).unwrap()),
        "Mutant test: < -> > in contains"
    );
}

#[test]
fn test_timerange_contains_or_after_mutants_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    let range = TimeRange::new(
        HybridTimestamp::new(100, 0).unwrap(),
        HybridTimestamp::new(200, 0).unwrap(),
    )
    .unwrap();

    assert!(
        range.contains_or_after(HybridTimestamp::new(150, 0).unwrap()),
        "Should contain_or_after"
    );
    assert!(
        !range.contains_or_after(HybridTimestamp::new(50, 0).unwrap()),
        "Should not contain_or_after"
    );

    assert!(
        !range.contains_or_after(HybridTimestamp::new(50, 0).unwrap()),
        "Mutant test: >= -> < in contains_or_after"
    );

    assert!(
        range.contains_or_after(HybridTimestamp::new(100, 0).unwrap()),
        "Mutant test boundary: >= -> <"
    );
}

#[test]
fn test_timerange_from_at_mutants_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{MAX_VALID_TIMESTAMP, TIMESTAMP_MAX, TimeRange};

    let valid_start = HybridTimestamp::new(100, 0).unwrap();
    let range = TimeRange::from(valid_start);

    assert_eq!(range.start(), valid_start, "Mutant test: from -> default");
    assert_eq!(range.end(), TIMESTAMP_MAX, "Mutant test: from -> default");

    let max_valid = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap();
    let res = std::panic::catch_unwind(|| {
        TimeRange::from(max_valid);
    });
    assert!(res.is_ok(), "Mutant test: > -> >= (should succeed for ==)");

    let res = std::panic::catch_unwind(|| {
        TimeRange::from(TIMESTAMP_MAX);
    });
    assert!(
        res.is_ok(),
        "Mutant test: != -> == (should succeed for TIMESTAMP_MAX)"
    );

    // For TimeRange::at
    let valid_ts = HybridTimestamp::new(100, 0).unwrap();
    let range = TimeRange::at(valid_ts);

    assert_eq!(range.start(), valid_ts, "Mutant test: at -> default");
    assert_eq!(range.end(), valid_ts, "Mutant test: at -> default");

    let max_valid = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap();
    let res = std::panic::catch_unwind(|| {
        TimeRange::at(max_valid);
    });
    assert!(res.is_ok(), "Mutant test: > -> >= in at");

    let res = std::panic::catch_unwind(|| {
        TimeRange::at(TIMESTAMP_MAX);
    });
    assert!(res.is_ok(), "Mutant test: != -> == in at");
}

#[test]
fn test_time_to_iso8601_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::time;

    let default_ts = HybridTimestamp::new(0, 0).unwrap();
    let default_str = time::to_iso8601(default_ts);
    if cfg!(windows) {
        assert!(
            default_str.contains("116444736000000000"),
            "to_iso8601 should output exact Windows epoch start"
        );
    } else {
        assert!(
            default_str.contains("1970-01-01T00:00:00Z") || default_str.contains("tv_sec: 0"),
            "to_iso8601 should output exact Unix epoch start. Got: {}",
            default_str
        );
    }
}

#[test]
fn test_timerange_overlaps_mutants_extra() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    let range1 = TimeRange::new(
        HybridTimestamp::new(100, 0).unwrap(),
        HybridTimestamp::new(200, 0).unwrap(),
    )
    .unwrap();

    let range2 = TimeRange::new(
        HybridTimestamp::new(150, 0).unwrap(),
        HybridTimestamp::new(250, 0).unwrap(),
    )
    .unwrap();

    // mutant: replace || with && in TimeRange::overlaps (self.is_empty() || other.is_empty())
    // For &&, both must be empty to return false. So if one is empty, it continues.
    let empty_range = TimeRange::at(HybridTimestamp::new(150, 0).unwrap());
    assert!(
        !range1.overlaps(&empty_range),
        "Should not overlap with empty"
    );
    assert!(
        !empty_range.overlaps(&range2),
        "Should not overlap with empty"
    );

    // mutant: replace < with == in TimeRange::overlaps (self.start < other.end)
    let left_almost_touch = TimeRange::new(
        HybridTimestamp::new(50, 0).unwrap(),
        HybridTimestamp::new(99, 0).unwrap(),
    )
    .unwrap();
    assert!(
        !range1.overlaps(&left_almost_touch),
        "Should not overlap disjoint"
    );

    // mutant: replace < with == in TimeRange::overlaps (other.start < self.end)
    let right_almost_touch = TimeRange::new(
        HybridTimestamp::new(201, 0).unwrap(),
        HybridTimestamp::new(250, 0).unwrap(),
    )
    .unwrap();
    assert!(
        !range1.overlaps(&right_almost_touch),
        "Should not overlap disjoint"
    );
}

#[test]
fn test_timerange_close_at_mutants() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    let start = HybridTimestamp::new(100, 0).unwrap();
    let range = TimeRange::from(start);

    // mutant: replace < with == in TimeRange::close_at (end < self.start)
    let end_eq = HybridTimestamp::new(100, 0).unwrap();
    assert!(
        range.close_at(end_eq).is_ok(),
        "Should close at exact start time"
    );

    let end_lt = HybridTimestamp::new(99, 0).unwrap();
    assert!(
        range.close_at(end_lt).is_err(),
        "Should error if end < start"
    );

    // mutant: replace < with > in TimeRange::close_at
    let end_gt = HybridTimestamp::new(101, 0).unwrap();
    assert!(range.close_at(end_gt).is_ok(), "Should close at later time");

    // mutant: replace && with ||
    // mutant: replace > with == (end.wallclock() > MAX_VALID_TIMESTAMP)
    use aletheiadb::core::temporal::MAX_VALID_TIMESTAMP;

    let max_ts = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap();
    assert!(
        range.close_at(max_ts).is_ok(),
        "Should close at exact MAX_VALID_TIMESTAMP"
    );

    use aletheiadb::core::temporal::TIMESTAMP_MAX;
    assert!(
        range.close_at(TIMESTAMP_MAX).is_ok(),
        "Should allow TIMESTAMP_MAX"
    );
}

#[test]
fn test_timerange_duration_micros_mutants() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    let ts_start = HybridTimestamp::new(100, 0).unwrap();
    let ts_end = HybridTimestamp::new(200, 0).unwrap();
    let range = TimeRange::new(ts_start, ts_end).unwrap();

    let duration = range.duration_micros();
    // mutants: None, Some(0), Some(1), Some(-1)
    assert!(duration.is_some(), "Duration should not be None");
    assert_eq!(duration.unwrap(), 100, "Duration should be exactly 100");

    let start_neg = HybridTimestamp::new(-2000, 0).unwrap();
    let end_max = HybridTimestamp::new(i64::MAX - 1000, 0).unwrap();
    let overflow_range = TimeRange::new(start_neg, end_max).unwrap();
    let overflow_duration = overflow_range.duration_micros();
    assert_eq!(
        overflow_duration.unwrap(),
        i64::MAX,
        "Should saturate to i64::MAX on overflow"
    );
}

#[test]
fn test_timerange_deserialize_mutants() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    let start = HybridTimestamp::new(100, 0).unwrap();
    let end = HybridTimestamp::new(200, 0).unwrap();
    let range = TimeRange::new(start, end).unwrap();

    let bytes = range.serialize();

    // mutant: replace < with == in bytes.len() < 24
    assert!(
        TimeRange::deserialize(&bytes[0..23]).is_err(),
        "Should error on < 24"
    );
    assert!(
        TimeRange::deserialize(&bytes[0..24]).is_ok(),
        "Should succeed on == 24"
    );

    // mutant: replace > with == in start > end
    let eq_range = TimeRange::new(start, start).unwrap();
    let eq_bytes = eq_range.serialize();
    assert!(
        TimeRange::deserialize(&eq_bytes).is_ok(),
        "Should succeed if start == end"
    );

    let mut inv_bytes = end.serialize();
    inv_bytes.extend(start.serialize());
    assert!(
        TimeRange::deserialize(&inv_bytes).is_err(),
        "Should error if start > end"
    );
}

#[test]
fn test_bitemporal_deserialize_mutants() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};

    let start = HybridTimestamp::new(100, 0).unwrap();
    let end = HybridTimestamp::new(200, 0).unwrap();
    let interval = BiTemporalInterval::new(
        TimeRange::new(start, end).unwrap(),
        TimeRange::new(start, end).unwrap(),
    );

    let bytes = interval.serialize();

    // mutant: replace < with == in bytes.len() < 48
    assert!(
        BiTemporalInterval::deserialize(&bytes[0..47]).is_err(),
        "Should error on < 48"
    );
    assert!(
        BiTemporalInterval::deserialize(&bytes[0..48]).is_ok(),
        "Should succeed on == 48"
    );
}

#[test]
fn test_bitemporal_interval_mutants() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};

    let start1 = HybridTimestamp::new(100, 0).unwrap();
    let start2 = HybridTimestamp::new(200, 0).unwrap();

    // test BiTemporalInterval::current default value mutant
    let int_current = BiTemporalInterval::current(start1);
    // test explicitly that it returns a correct instance that is not e.g. wrapping 0
    let empty_ts = HybridTimestamp::new(0, 0).unwrap();
    let empty_range = TimeRange::from(empty_ts);
    let empty_int = BiTemporalInterval::new(empty_range, empty_range);

    assert_ne!(int_current, empty_int, "Should not be default");
    assert_eq!(int_current.valid_time().start(), start1);

    // test BiTemporalInterval::now default value mutant
    let int_now = BiTemporalInterval::now(start1, start2);
    assert_ne!(int_now, empty_int, "Should not be default");
    assert_eq!(int_now.valid_time().start(), start1);
    assert_eq!(int_now.transaction_time().start(), start2);

    // test BiTemporalInterval::with_valid_time default value mutant
    let int_with_valid = BiTemporalInterval::with_valid_time(start1, start2);
    assert_ne!(int_with_valid, empty_int, "Should not be default");

    // test valid_time / transaction_time default return mutants
    assert_ne!(
        int_now.valid_time(),
        empty_range,
        "valid_time should not be default"
    );
    assert_ne!(
        int_now.transaction_time(),
        empty_range,
        "transaction_time should not be default"
    );
}

#[test]
fn test_bitemporal_is_methods_mutants() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};

    let ts_100 = HybridTimestamp::new(100, 0).unwrap();
    let ts_200 = HybridTimestamp::new(200, 0).unwrap();
    let ts_300 = HybridTimestamp::new(300, 0).unwrap();

    let int_open = BiTemporalInterval::current(ts_100);

    // mutants returning true/false for is_currently_valid
    assert!(int_open.is_currently_valid(), "Should be currently valid");
    let int_closed_valid = int_open.close_valid_time(ts_200).unwrap();
    assert!(
        !int_closed_valid.is_currently_valid(),
        "Should not be currently valid after close"
    );

    // mutants returning true/false for is_currently_recorded
    let int_open2 = BiTemporalInterval::current(ts_100);
    assert!(
        int_open2.is_currently_recorded(),
        "Should be currently recorded"
    );
    let int_closed_tx = int_open2.close_transaction_time(ts_200).unwrap();
    assert!(
        !int_closed_tx.is_currently_recorded(),
        "Should not be currently recorded after close"
    );

    // mutants for is_current
    assert!(int_open.is_current(), "Should be current in both");
    assert!(
        !int_closed_valid.is_current(),
        "Should not be current if valid closed"
    );
    assert!(
        !int_closed_tx.is_current(),
        "Should not be current if tx closed"
    );

    // replace && with || in is_current
    let mixed1 = BiTemporalInterval::new(
        TimeRange::from(ts_100),
        TimeRange::new(ts_100, ts_200).unwrap(),
    );
    assert!(
        !mixed1.is_current(),
        "Should not be current if one is closed"
    );

    let mixed2 = BiTemporalInterval::new(
        TimeRange::new(ts_100, ts_200).unwrap(),
        TimeRange::from(ts_100),
    );
    assert!(
        !mixed2.is_current(),
        "Should not be current if one is closed"
    );

    // mutants for is_valid_at, is_recorded_at, is_visible_at
    let int = BiTemporalInterval::new(
        TimeRange::new(ts_100, ts_300).unwrap(),
        TimeRange::new(ts_100, ts_200).unwrap(),
    );

    assert!(int.is_valid_at(ts_200), "Should be valid at 200");
    assert!(!int.is_valid_at(ts_300), "Should not be valid at 300");

    assert!(int.is_recorded_at(ts_100), "Should be recorded at 100");
    assert!(!int.is_recorded_at(ts_200), "Should not be recorded at 200");

    // replace && with || in is_visible_at
    assert!(
        !int.is_visible_at(ts_200, ts_200),
        "Should not be visible if tx not valid"
    );
    assert!(
        !int.is_visible_at(ts_300, ts_100),
        "Should not be visible if valid not valid"
    );
    assert!(
        int.is_visible_at(ts_200, ts_100),
        "Should be visible if both valid"
    );
}

#[test]
fn test_bitemporal_close_methods_mutants() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::BiTemporalInterval;

    let start = HybridTimestamp::new(100, 0).unwrap();
    let end = HybridTimestamp::new(200, 0).unwrap();

    let int = BiTemporalInterval::current(start);

    let empty_ts = HybridTimestamp::new(0, 0).unwrap();
    let empty_range = aletheiadb::core::temporal::TimeRange::from(empty_ts);
    let empty_int = BiTemporalInterval::new(empty_range, empty_range);

    let closed_valid = int.close_valid_time(end).unwrap();
    assert_ne!(
        closed_valid, empty_int,
        "close_valid_time should not return default"
    );
    assert_eq!(closed_valid.valid_time().end(), end);

    let closed_tx = int.close_transaction_time(end).unwrap();
    assert_ne!(
        closed_tx, empty_int,
        "close_transaction_time should not return default"
    );
    assert_eq!(closed_tx.transaction_time().end(), end);

    let closed_both = int.close_both(end, end).unwrap();
    assert_ne!(
        closed_both, empty_int,
        "close_both should not return default"
    );
    assert_eq!(closed_both.valid_time().end(), end);
    assert_eq!(closed_both.transaction_time().end(), end);
}

#[test]
fn test_bitemporal_serialize_mutants() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::BiTemporalInterval;

    let start = HybridTimestamp::new(100, 0).unwrap();
    let int = BiTemporalInterval::current(start);

    let bytes = int.serialize();

    // Precalculate the exact 48 byte expected representation.
    // Both Valid and TX are [100, 0] to [TIMESTAMP_MAX]
    // HybridTimestamp(100, 0) -> wallclock 100_i64.to_le_bytes(), logical 0_u32.to_le_bytes()
    // HybridTimestamp(TIMESTAMP_MAX) -> i64::MAX.to_le_bytes(), 0_u32.to_le_bytes()
    let mut expected_bytes = Vec::new();
    // Valid start
    expected_bytes.extend(100_i64.to_le_bytes());
    expected_bytes.extend(0_u32.to_le_bytes());
    // Valid end
    expected_bytes.extend(i64::MAX.to_le_bytes());
    expected_bytes.extend(0_u32.to_le_bytes());

    // TX start
    expected_bytes.extend(100_i64.to_le_bytes());
    expected_bytes.extend(0_u32.to_le_bytes());
    // TX end
    expected_bytes.extend(i64::MAX.to_le_bytes());
    expected_bytes.extend(0_u32.to_le_bytes());

    assert_eq!(
        bytes, expected_bytes,
        "serialize output must exactly match expected 48 bytes"
    );
    assert_eq!(bytes.len(), 48, "serialize length should be 48");

    let mut buf = Vec::new();
    int.serialize_into(&mut buf);
    assert_eq!(buf.len(), 48, "serialize_into should write 48 bytes");
    assert_eq!(
        buf, bytes,
        "serialize_into output should match serialize output exactly"
    );
}

#[test]
fn test_time_conversions_mutants() {
    use aletheiadb::core::temporal::time;

    // test time::from_secs default return and exact math
    let empty_ts = aletheiadb::core::hlc::HybridTimestamp::new(0, 0).unwrap();
    let ts_secs = time::from_secs(10);
    assert_ne!(ts_secs, empty_ts, "from_secs should not return default");
    assert_eq!(
        ts_secs.wallclock(),
        10 * 1_000_000,
        "from_secs math is wrong"
    );

    // test time::from_millis default return and exact math
    let ts_millis = time::from_millis(10);
    assert_ne!(ts_millis, empty_ts, "from_millis should not return default");
    assert_eq!(
        ts_millis.wallclock(),
        10 * 1_000,
        "from_millis math is wrong"
    );

    // test time::to_secs default returns and exact math
    let secs = time::to_secs(ts_secs);
    assert_ne!(secs, 0, "to_secs should not return 0");
    assert_ne!(secs, 1, "to_secs should not return 1");
    assert_ne!(secs, -1, "to_secs should not return -1");
    assert_eq!(secs, 10, "to_secs math is wrong");

    // test time::to_millis default returns and exact math
    let millis = time::to_millis(ts_millis);
    assert_ne!(millis, 0, "to_millis should not return 0");
    assert_ne!(millis, 1, "to_millis should not return 1");
    assert_ne!(millis, -1, "to_millis should not return -1");
    assert_eq!(millis, 10, "to_millis math is wrong");
}

#[test]
fn test_to_iso8601_mutants() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{TIMESTAMP_MAX, time};

    // mutant: replace == with != in time::to_iso8601
    assert_eq!(
        time::to_iso8601(TIMESTAMP_MAX),
        "current",
        "TIMESTAMP_MAX should return 'current'"
    );

    // test empty / xyzzy returns exact output matching logic
    // we use a timestamp that has an exact expected formatting (e.g. 100 seconds after epoch)
    let ts = HybridTimestamp::new(100_000_000, 0).unwrap();
    let iso = time::to_iso8601(ts);

    if cfg!(windows) {
        assert!(
            iso.contains("116444737000000000"),
            "Should contain exact tick count: {}",
            iso
        );
    } else {
        // Since we simplify and use Debug representation of SystemTime,
        // it may look like `SystemTime { tv_sec: 100, tv_nsec: 0 }`
        assert!(
            iso.contains("100"),
            "Should contain exact formatted seconds: {}",
            iso
        );
    }

    // Test math mutations in to_iso8601 by ensuring exact nanoseconds and seconds calculation
    // 1500_500_000 microseconds = 1500 seconds + 500_000 microseconds
    // = 1500 seconds + 500_000_000 nanoseconds
    let ts_math = HybridTimestamp::new(1_500_500_000, 0).unwrap();
    let iso_math = time::to_iso8601(ts_math);

    // Using string matching to avoid depending on exact chrono logic formatting,
    // but ensuring the calculated time duration components are right.
    // If * was replaced with +, or / with %, the underlying Duration::new would fail
    // or format entirely wrong numbers
    if cfg!(windows) {
        // Windows might format SystemTime Debug differently, but it will have the value
        // tick math: 116444736000000000 + 15000000000 + 5000000 = 116444751005000000
        assert!(
            iso_math.contains("116444751005000000"),
            "Should contain exact tick count or related math: {}",
            iso_math
        );
    } else {
        assert!(
            iso_math.contains("500000000"),
            "Should contain exact nanos: {}",
            iso_math
        );
        assert!(
            iso_math.contains("1500"),
            "Should contain exact seconds: {}",
            iso_math
        );
    }
}
