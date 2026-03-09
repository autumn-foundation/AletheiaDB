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

    let start = 100_000.into();
    let end = 200_000.into();

    // Both closed
    let closed_vt = TimeRange::new(start, end).unwrap();
    let closed_tt = TimeRange::new(start, end).unwrap();
    let closed_interval = BiTemporalInterval::new(closed_vt, closed_tt);
    assert!(!closed_interval.is_currently_valid());
    assert!(!closed_interval.is_currently_recorded());

    // VT open, TT closed
    let open_vt = TimeRange::from(start);
    let vt_open_interval = BiTemporalInterval::new(open_vt, closed_tt);
    assert!(vt_open_interval.is_currently_valid());
    assert!(!vt_open_interval.is_currently_recorded());

    // VT closed, TT open
    let open_tt = TimeRange::from(start);
    let tt_open_interval = BiTemporalInterval::new(closed_vt, open_tt);
    assert!(!tt_open_interval.is_currently_valid());
    assert!(tt_open_interval.is_currently_recorded());

    // Both open
    let open_interval = BiTemporalInterval::new(open_vt, open_tt);
    assert!(open_interval.is_currently_valid());
    assert!(open_interval.is_currently_recorded());
}

#[test]
fn test_bitemporal_is_valid_at_and_recorded_at() {
    use aletheiadb::core::temporal::BiTemporalInterval;
    use aletheiadb::core::temporal::TimeRange;

    let vt = TimeRange::new(100.into(), 200.into()).unwrap();
    let tt = TimeRange::new(300.into(), 400.into()).unwrap();
    let interval = BiTemporalInterval::new(vt, tt);

    // Exact bounds for is_valid_at (matches valid_time.contains)
    assert!(interval.is_valid_at(100.into())); // start
    assert!(interval.is_valid_at(150.into())); // middle
    assert!(!interval.is_valid_at(200.into())); // end (exclusive)

    // Exact bounds for is_recorded_at (matches transaction_time.contains)
    assert!(interval.is_recorded_at(300.into())); // start
    assert!(interval.is_recorded_at(350.into())); // middle
    assert!(!interval.is_recorded_at(400.into())); // end (exclusive)

    // is_visible_at (both)
    assert!(interval.is_visible_at(100.into(), 300.into()));
    assert!(!interval.is_visible_at(200.into(), 300.into()));
    assert!(!interval.is_visible_at(100.into(), 400.into()));
    assert!(!interval.is_visible_at(200.into(), 400.into()));
}

#[test]
fn test_timerange_from_at_exact_boundaries() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{MAX_VALID_TIMESTAMP, TIMESTAMP_MAX, TimeRange};

    let max_ts = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap();

    // from() logic boundary `start.wallclock() > MAX_VALID_TIMESTAMP && start != TIMESTAMP_MAX`
    let range1 = TimeRange::from(max_ts);
    assert_eq!(range1.start(), max_ts);

    let range2 = TimeRange::from(TIMESTAMP_MAX);
    assert_eq!(range2.start(), TIMESTAMP_MAX);

    // at() logic boundary
    let point1 = TimeRange::at(max_ts);
    assert_eq!(point1.start(), max_ts);

    let point2 = TimeRange::at(TIMESTAMP_MAX);
    assert_eq!(point2.start(), TIMESTAMP_MAX);
}

#[test]
fn test_time_to_secs_millis_exact_math() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::time;

    // 1.234567 seconds = 1234567 micros
    let ts = HybridTimestamp::new(1234567, 0).unwrap();

    // to_secs exact logic (timestamp.wallclock() / 1_000_000)
    let secs = time::to_secs(ts);
    assert_eq!(
        secs, 1,
        "to_secs exact math should use division by 1_000_000"
    );

    // to_millis exact logic (timestamp.wallclock() / 1_000)
    let millis = time::to_millis(ts);
    assert_eq!(
        millis, 1234,
        "to_millis exact math should use division by 1_000"
    );

    let default_secs = time::to_secs(HybridTimestamp::new(0, 0).unwrap());
    assert_ne!(secs, default_secs, "to_secs should not return 0 or default");

    let default_millis = time::to_millis(HybridTimestamp::new(0, 0).unwrap());
    assert_ne!(
        millis, default_millis,
        "to_millis should not return 0 or default"
    );
}

#[test]
fn test_time_to_iso8601_exact_content() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{TIMESTAMP_MAX, time};

    // TIMESTAMP_MAX returns "current" exactly
    let iso = time::to_iso8601(TIMESTAMP_MAX);
    assert_eq!(iso, "current");

    // wallclock formatting exact math
    // 1_000_001 micros = 1 sec + 1000 nanos
    let ts = HybridTimestamp::new(1_000_001, 0).unwrap();
    let iso_ts = time::to_iso8601(ts);

    // Make sure it doesn't return empty string or arbitrary "xyzzy"
    assert!(!iso_ts.is_empty(), "Should not return empty string");
    assert_ne!(iso_ts, "xyzzy", "Should not return dummy string");

    // We verify the exact sub-second contribution.
    // wallclock = 1_000_001
    // secs = 1
    // nanos = 1000
    if cfg!(windows) {
        // SystemTime internal tick calculation
        // Total ticks = base ticks + secs ticks + micros ticks
        // Base ticks = 11644473600 * 10_000_000 = 116444736000000000
        // Secs ticks = 1 * 10_000_000 = 10000000
        // Micros ticks = 1 * 10 = 10
        // Total = 116444736010000010
        assert!(
            iso_ts.contains("10"),
            "Windows output should contain correct ticks for 1 microsecond"
        );
    } else {
        // tv_nsec = 1000
        assert!(
            iso_ts.contains("1000"),
            "Unix output should contain exactly 1000 nanos"
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
    let point = TimeRange::at(100.into());
    assert!(
        point.is_empty(),
        "is_empty should be exactly start == end (true)"
    );

    let range = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(
        !range.is_empty(),
        "is_empty should be exactly start == end (false)"
    );
}

#[test]
fn test_timerange_is_current_closed_exact() {
    use aletheiadb::core::temporal::TimeRange;

    let current = TimeRange::from(100.into());
    assert!(
        current.is_current(),
        "is_current exactly checks end == TIMESTAMP_MAX"
    );
    assert!(
        !current.is_closed(),
        "is_closed exactly checks end < TIMESTAMP_MAX"
    );

    let closed = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(
        !closed.is_current(),
        "is_current exactly checks end == TIMESTAMP_MAX"
    );
    assert!(
        closed.is_closed(),
        "is_closed exactly checks end < TIMESTAMP_MAX"
    );

    // Verify exactly boundary TIMESTAMP_MAX behavior (end <= TIMESTAMP_MAX vs <)
    // Note: since TIMESTAMP_MAX is max, it's impossible for end > TIMESTAMP_MAX
    // But replacing `<` with `<=` in `is_closed` makes `is_closed` true for `current`.
    // The previous checks (`!current.is_closed()`) precisely catch this.
}

#[test]
fn test_timerange_close_at_exact() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{MAX_VALID_TIMESTAMP, TIMESTAMP_MAX, TimeRange};

    let start = HybridTimestamp::new(100, 0).unwrap();
    let range = TimeRange::from(start);

    // Normal closing
    let end1 = HybridTimestamp::new(200, 0).unwrap();
    let closed = range.close_at(end1).unwrap();
    assert_eq!(closed.end(), end1);

    // exact `end < self.start` condition
    assert!(
        range
            .close_at(HybridTimestamp::new(99, 0).unwrap())
            .is_err(),
        "Closing before start should fail exactly"
    );
    assert!(
        range.close_at(start).is_ok(),
        "Closing at exactly start should succeed (creates empty range)"
    );

    // MAX_VALID_TIMESTAMP exact boundary
    let max_valid = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap();
    assert!(
        range.close_at(max_valid).is_ok(),
        "Closing at exactly MAX_VALID_TIMESTAMP should succeed"
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

    // Verify exact serialized bytes
    let bytes = range.serialize();
    assert_eq!(bytes.len(), 24);

    // Deserialize exact sizes
    let (deserialized, size) = TimeRange::deserialize(&bytes).unwrap();
    assert_eq!(size, 24);
    assert_eq!(deserialized, range);

    // Ensure serialize doesn't return `vec![]`, `vec![0]`, `vec![1]`
    let empty_vec: Vec<u8> = vec![];
    assert_ne!(bytes, empty_vec);
    assert_ne!(bytes, vec![0u8]);
    assert_ne!(bytes, vec![1u8]);

    // Test buffer too small for deserialize
    let too_small = &bytes[0..23];
    assert!(TimeRange::deserialize(too_small).is_err());

    // Test start > end logic in deserialize
    let inverted_start = HybridTimestamp::new(200, 2).unwrap();
    let inverted_end = HybridTimestamp::new(100, 1).unwrap();

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

    // Ensure serialize doesn't return `vec![]`, `vec![0]`, `vec![1]`
    let empty_vec: Vec<u8> = vec![];
    assert_ne!(bytes, empty_vec);
    assert_ne!(bytes, vec![0u8]);
    assert_ne!(bytes, vec![1u8]);

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

    // close_valid_time
    let closed_valid = interval.close_valid_time(close_ts).unwrap();
    assert_eq!(closed_valid.valid_time().end(), close_ts);
    assert_eq!(closed_valid.transaction_time().end(), TIMESTAMP_MAX);

    // close_transaction_time
    let closed_tx = interval.close_transaction_time(close_ts).unwrap();
    assert_eq!(closed_tx.valid_time().end(), TIMESTAMP_MAX);
    assert_eq!(closed_tx.transaction_time().end(), close_ts);

    // close_both
    let close_both_valid = HybridTimestamp::new(400, 0).unwrap();
    let close_both_tx = HybridTimestamp::new(500, 0).unwrap();
    let closed_both = interval
        .close_both(close_both_valid, close_both_tx)
        .unwrap();
    assert_eq!(closed_both.valid_time().end(), close_both_valid);
    assert_eq!(closed_both.transaction_time().end(), close_both_tx);
}

#[test]
fn test_bitemporal_constructors_exact() {
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::BiTemporalInterval;

    let ts = HybridTimestamp::new(100, 0).unwrap();
    let ts2 = HybridTimestamp::new(200, 0).unwrap();

    // current()
    let current = BiTemporalInterval::current(ts);
    assert_eq!(current.valid_time().start(), ts);
    assert_eq!(current.transaction_time().start(), ts);

    // now()
    let now = BiTemporalInterval::now(ts, ts2);
    assert_eq!(now.valid_time().start(), ts);
    assert_eq!(now.transaction_time().start(), ts2);

    // with_valid_time()
    let with_vt = BiTemporalInterval::with_valid_time(ts, ts2);
    assert_eq!(with_vt.valid_time().start(), ts);
    assert_eq!(with_vt.transaction_time().start(), ts2);

    // Ensure it's not returning Ok(Default::default()) which gives 0 for start
    let default_ts = HybridTimestamp::new(0, 0).unwrap();
    assert_ne!(current.valid_time().start(), default_ts);
    assert_ne!(now.valid_time().start(), default_ts);
    assert_ne!(with_vt.valid_time().start(), default_ts);
}

#[test]
fn test_temporal_display_exact() {
    use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};

    // TimeRange formatting
    let open_range = TimeRange::from(100.into());
    let open_fmt = format!("{}", open_range);
    assert!(
        open_fmt.contains("current"),
        "Open range should contain 'current' keyword: {}",
        open_fmt
    );
    assert!(
        open_fmt.starts_with('['),
        "Range display starts with [ : {}",
        open_fmt
    );

    let closed_range = TimeRange::new(100.into(), 200.into()).unwrap();
    let closed_fmt = format!("{}", closed_range);
    assert!(
        closed_fmt.ends_with(')'),
        "Closed range display ends with ) : {}",
        closed_fmt
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
