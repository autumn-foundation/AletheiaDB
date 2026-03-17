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
fn test_timerange_from_accepts_valid_timestamps() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{MAX_VALID_TIMESTAMP, TIMESTAMP_MAX, TimeRange};

    // Should NOT panic
    let t1 = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap();
    let _ = TimeRange::from(t1);

    let t2 = HybridTimestamp::new(MAX_VALID_TIMESTAMP - 1, 0).unwrap();
    let _ = TimeRange::from(t2);

    let _ = TimeRange::from(TIMESTAMP_MAX);
}

// We cannot easily create a HybridTimestamp > MAX_VALID_TIMESTAMP safely in an integration test
// because the constructor and deserialize logic explicitly forbid it. Thus we can't test
// `TimeRange::from` or `TimeRange::at` directly with a too-large timestamp without UB.
// Since we have removed the UB and invalid test logic, we remove this test.

#[test]
fn test_timerange_at_accepts_valid_timestamps() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{MAX_VALID_TIMESTAMP, TIMESTAMP_MAX, TimeRange};

    // Should NOT panic
    let t1 = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap();
    let _ = TimeRange::at(t1);

    let t2 = HybridTimestamp::new(MAX_VALID_TIMESTAMP - 1, 0).unwrap();
    let _ = TimeRange::at(t2);

    let _ = TimeRange::at(TIMESTAMP_MAX);
}

// Similarly, we remove this test as it's impossible to safely create an invalid HybridTimestamp.

#[test]
fn test_timerange_contains_strict_inequalities() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    let start = HybridTimestamp::new(100, 0).unwrap();
    let end = HybridTimestamp::new(200, 0).unwrap();
    let range = TimeRange::new(start, end).unwrap();

    // timestamp >= self.start && timestamp < self.end

    // >= self.start
    assert!(range.contains(start), "Must contain exact start");
    let just_before_start = HybridTimestamp::new(99, 0).unwrap();
    assert!(
        !range.contains(just_before_start),
        "Must not contain before start"
    );

    // < self.end
    let just_before_end = HybridTimestamp::new(199, 0).unwrap();
    assert!(
        range.contains(just_before_end),
        "Must contain just before end"
    );
    assert!(!range.contains(end), "Must not contain exact end");

    let past_end = HybridTimestamp::new(201, 0).unwrap();
    assert!(!range.contains(past_end), "Must not contain past end");

    // AND / OR check (mutant `&&` replaced with `||`)
    // If it was ||, containing just_before_start would be true because 99 < 200.
    // We already asserted that's false!
    // If it was ||, containing end would be true because 200 >= 100.
    // We already asserted that's false!
}

#[test]
fn test_timerange_is_closed_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{MAX_VALID_TIMESTAMP, TimeRange};

    let start = HybridTimestamp::new(100, 0).unwrap();

    let end1 = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap();
    let range1 = TimeRange::new(start, end1).unwrap();
    assert!(
        range1.is_closed(),
        "range with end < TIMESTAMP_MAX is closed"
    );

    // end == TIMESTAMP_MAX
    let range2 = TimeRange::from(start);
    assert!(
        !range2.is_closed(),
        "range with end == TIMESTAMP_MAX is NOT closed"
    );
}

#[test]
fn test_timerange_overlaps_logical_operators() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    // We want to test `if self.is_empty() || other.is_empty()` where `||` is replaced with `&&`.
    // If it was `&&`, it would only return false early if BOTH were empty.
    // So if only ONE is empty, it wouldn't return false early, it would go to the overlap check.

    // For an empty range, start == end.
    // The overlap check is `self.start < other.end && other.start < self.end`.
    // Let's craft an empty range and a normal range such that if it falls through, it returns true!
    let empty = TimeRange::at(HybridTimestamp::new(150, 0).unwrap());
    let normal = TimeRange::new(
        HybridTimestamp::new(100, 0).unwrap(),
        HybridTimestamp::new(200, 0).unwrap(),
    )
    .unwrap();

    // empty.start (150) < normal.end (200) -> true
    // normal.start (100) < empty.end (150) -> true
    // So `empty.start < normal.end && normal.start < empty.end` -> true && true = true!

    // Therefore, if the early return `||` is replaced by `&&`, it won't early return for `empty.overlaps(normal)`,
    // and it will evaluate to true!

    assert!(!empty.overlaps(&normal), "Empty range should not overlap");
    assert!(
        !normal.overlaps(&empty),
        "Normal range should not overlap empty"
    );
}

#[test]
fn test_bitemporal_methods_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};

    let valid_ts = HybridTimestamp::new(100, 0).unwrap();
    let tx_ts = HybridTimestamp::new(200, 0).unwrap();
    let end_ts = HybridTimestamp::new(300, 0).unwrap();

    let interval = BiTemporalInterval::new(
        TimeRange::new(valid_ts, end_ts).unwrap(),
        TimeRange::from(tx_ts),
    );

    // We already have some tests, but let's make sure we assert exact values for every mutant
    // is_currently_valid: false -> true
    assert!(!interval.is_currently_valid());

    // is_currently_recorded: true -> false
    assert!(interval.is_currently_recorded());

    // valid_time -> default
    let vt = interval.valid_time();
    assert_eq!(vt.start(), valid_ts);
    assert_eq!(vt.end(), end_ts);

    // transaction_time -> default
    let tt = interval.transaction_time();
    assert_eq!(tt.start(), tx_ts);

    // is_valid_at -> true / false
    assert!(interval.is_valid_at(HybridTimestamp::new(150, 0).unwrap()));
    assert!(!interval.is_valid_at(HybridTimestamp::new(50, 0).unwrap()));

    // is_recorded_at -> true / false
    assert!(interval.is_recorded_at(HybridTimestamp::new(250, 0).unwrap()));
    assert!(!interval.is_recorded_at(HybridTimestamp::new(150, 0).unwrap()));

    // is_visible_at
    assert!(interval.is_visible_at(
        HybridTimestamp::new(150, 0).unwrap(),
        HybridTimestamp::new(250, 0).unwrap()
    ));
    // Check AND logic `self.valid_time.contains(valid_time) && self.transaction_time.contains(tx_time)`
    // replaced with ||
    assert!(!interval.is_visible_at(
        HybridTimestamp::new(50, 0).unwrap(),
        HybridTimestamp::new(250, 0).unwrap()
    )); // False && True -> False (if || this would be True!)
}

#[test]
fn test_bitemporal_serialize_into_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::BiTemporalInterval;

    let interval = BiTemporalInterval::now(
        HybridTimestamp::new(100, 0).unwrap(),
        HybridTimestamp::new(200, 0).unwrap(),
    );

    // replace BiTemporalInterval::serialize -> Vec<u8> with vec![] / vec![0] / vec![1]
    let bytes = interval.serialize();
    // Instead of using assert_ne! we should assert exactly the expected length and content.
    assert_eq!(bytes.len(), 48);

    // replace BiTemporalInterval::serialize_into with ()
    let mut buf = vec![];
    interval.serialize_into(&mut buf);
    assert_eq!(buf.len(), 48);
}

#[test]
fn test_time_to_iso8601_operators() {
    use aletheiadb::core::temporal::{TIMESTAMP_MAX, time};

    // test `==` vs `!=` with TIMESTAMP_MAX
    // If `==` is mutated to `!=`, TIMESTAMP_MAX won't return "current", but format normal
    let max_str = time::to_iso8601(TIMESTAMP_MAX);
    assert_eq!(max_str, "current");

    // test `/`, `%`, `*`, `+`, `-` with 1609459200
    // let secs = wallclock / 1_000_000;
    // let nanos = ((wallclock % 1_000_000) * 1000) as u32;
    // let datetime = UNIX_EPOCH + std::time::Duration::new(secs as u64, nanos);

    // Ensure that it exactly converts it.
    let ts = time::from_secs(1609459200);
    let iso = time::to_iso8601(ts);
    // Since we know what it should roughly be, let's just make sure it contains "1609459200"
    // or the windows interval. We already assert this in `test_time_to_iso8601_exact_content`.
    // Instead of `assert_ne!`, we verify the known structure.
    assert!(iso.len() > 10, "ISO string should be fully formatted");

    // We already did exact contents check in `test_time_to_iso8601_exact_content`
}

#[test]
fn test_timerange_serialize_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    let range = TimeRange::new(
        HybridTimestamp::new(100, 0).unwrap(),
        HybridTimestamp::new(200, 0).unwrap(),
    )
    .unwrap();

    // replace TimeRange::serialize -> Vec<u8> with vec![] / vec![0] / vec![1]
    let bytes = range.serialize();
    // Instead of using assert_ne! we should assert exactly the expected length and content.
    assert_eq!(bytes.len(), 24);

    // replace TimeRange::serialize_into with ()
    let mut buf = vec![];
    range.serialize_into(&mut buf);
    assert_eq!(buf.len(), 24);
}

#[test]
fn test_timerange_deserialize_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::TimeRange;

    let range = TimeRange::new(
        HybridTimestamp::new(100, 0).unwrap(),
        HybridTimestamp::new(200, 0).unwrap(),
    )
    .unwrap();
    let bytes = range.serialize();

    let (de, len) = TimeRange::deserialize(&bytes).unwrap();
    assert_eq!(de.start(), HybridTimestamp::new(100, 0).unwrap());
    assert_eq!(de.end(), HybridTimestamp::new(200, 0).unwrap());
    assert_eq!(len, 24);
}

#[test]
fn test_bitemporal_deserialize_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::BiTemporalInterval;

    let interval = BiTemporalInterval::now(
        HybridTimestamp::new(100, 0).unwrap(),
        HybridTimestamp::new(200, 0).unwrap(),
    );
    let bytes = interval.serialize();

    let (de, len) = BiTemporalInterval::deserialize(&bytes).unwrap();
    assert_eq!(
        de.valid_time().start(),
        HybridTimestamp::new(100, 0).unwrap()
    );
    assert_eq!(len, 48);
}

#[test]
fn test_fmt_display_strict() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};

    let range = TimeRange::new(
        HybridTimestamp::new(100, 0).unwrap(),
        HybridTimestamp::new(200, 0).unwrap(),
    )
    .unwrap();
    let fmt_range = format!("{}", range);
    assert!(fmt_range.contains("100"), "format must contain bounds");
    assert!(fmt_range.contains("200"), "format must contain bounds");

    let interval = BiTemporalInterval::now(
        HybridTimestamp::new(100, 0).unwrap(),
        HybridTimestamp::new(200, 0).unwrap(),
    );
    let fmt_interval = format!("{}", interval);
    assert!(
        fmt_interval.contains("valid:"),
        "must format both dimensions"
    );
}
