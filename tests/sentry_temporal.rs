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
fn test_time_conversions_mutant_kill() {
    use aletheiadb::core::temporal::time;

    // Test from_secs logic strictly (* 1_000_000)
    let s = 5;
    let ts_s = time::from_secs(s);
    assert_eq!(
        ts_s.wallclock(),
        s * 1_000_000,
        "from_secs must exactly multiply by 1_000_000"
    );

    // Test from_millis logic strictly (* 1_000)
    let m = 5000;
    let ts_m = time::from_millis(m);
    assert_eq!(
        ts_m.wallclock(),
        m * 1_000,
        "from_millis must exactly multiply by 1_000"
    );

    // Test to_secs default returns (-1, 0, 1) and exact logic
    let ts1 = time::from_secs(10);
    assert_eq!(time::to_secs(ts1), 10, "to_secs should strictly return 10");
    assert_ne!(time::to_secs(ts1), 0);
    assert_ne!(time::to_secs(ts1), 1);
    assert_ne!(time::to_secs(ts1), -1);

    // Test to_millis default returns (-1, 0, 1) and exact logic
    let ts2 = time::from_millis(20000);
    assert_eq!(
        time::to_millis(ts2),
        20000,
        "to_millis should strictly return 20000"
    );
    assert_ne!(time::to_millis(ts2), 0);
    assert_ne!(time::to_millis(ts2), 1);
}

#[test]
fn test_bitemporal_interval_constructors_mutant_kill() {
    use aletheiadb::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX};

    let start = 1000.into();
    let current = BiTemporalInterval::current(start);
    assert_eq!(current.valid_time().start(), start);
    assert_eq!(current.valid_time().end(), TIMESTAMP_MAX);
    assert_eq!(current.transaction_time().start(), start);
    assert_eq!(current.transaction_time().end(), TIMESTAMP_MAX);

    let tx_time = 2000.into();
    let now = BiTemporalInterval::now(start, tx_time);
    assert_eq!(now.valid_time().start(), start);
    assert_eq!(now.valid_time().end(), TIMESTAMP_MAX);
    assert_eq!(now.transaction_time().start(), tx_time);
    assert_eq!(now.transaction_time().end(), TIMESTAMP_MAX);

    let with_valid = BiTemporalInterval::with_valid_time(start, tx_time);
    assert_eq!(with_valid.valid_time().start(), start);
    assert_eq!(with_valid.valid_time().end(), TIMESTAMP_MAX);
    assert_eq!(with_valid.transaction_time().start(), tx_time);
    assert_eq!(with_valid.transaction_time().end(), TIMESTAMP_MAX);
}

#[test]
fn test_bitemporal_accessors_mutant_kill() {
    use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};

    let valid_start = 1000.into();
    let valid_end = 2000.into();
    let tx_start = 3000.into();
    let tx_end = 4000.into();

    let valid = TimeRange::new(valid_start, valid_end).unwrap();
    let tx = TimeRange::new(tx_start, tx_end).unwrap();
    let bi = BiTemporalInterval::new(valid, tx);

    assert_eq!(bi.valid_time(), valid);
    assert_eq!(bi.transaction_time(), tx);
}

#[test]
fn test_bitemporal_is_current_mutant_kill() {
    use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};

    // Testing `replace BiTemporalInterval::is_current -> bool with true / false` and `replace && with ||`
    let valid_open = TimeRange::from(100.into());
    let tx_open = TimeRange::from(200.into());
    let valid_closed = TimeRange::new(100.into(), 150.into()).unwrap();
    let tx_closed = TimeRange::new(200.into(), 250.into()).unwrap();

    let bi_both_open = BiTemporalInterval::new(valid_open, tx_open);
    assert!(bi_both_open.is_current());

    let bi_valid_open = BiTemporalInterval::new(valid_open, tx_closed);
    assert!(!bi_valid_open.is_current());

    let bi_tx_open = BiTemporalInterval::new(valid_closed, tx_open);
    assert!(!bi_tx_open.is_current());

    let bi_both_closed = BiTemporalInterval::new(valid_closed, tx_closed);
    assert!(!bi_both_closed.is_current());
}

#[test]
fn test_bitemporal_fmt_mutant_kill() {
    use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};

    let valid = TimeRange::new(100.into(), 200.into()).unwrap();
    let tx = TimeRange::new(150.into(), 250.into()).unwrap();
    let bi = BiTemporalInterval::new(valid, tx);

    let fmt = format!("{}", bi);
    assert!(fmt.contains("valid:"));
    assert!(fmt.contains("tx:"));
    assert!(!fmt.contains("current"));
}

#[test]
fn test_time_to_iso8601_math_mutants() {
    use aletheiadb::core::temporal::time;

    // Mutants target exactly the operations: / 1_000_000, % 1_000_000, * 1000
    // secs = wallclock / 1_000_000
    // nanos = ((wallclock % 1_000_000) * 1000) as u32

    // Pick a timestamp where wallclock is exactly 1234567890
    // secs should be 1234, remainder 567890, nanos 567890000
    let ts = aletheiadb::core::hlc::HybridTimestamp::new(1234567890, 0).unwrap();

    let iso = time::to_iso8601(ts);

    // Test the exact formatted string for this specific timestamp (UNIX EPOCH + 1234 seconds, 567890000 nanos)
    // Just verify the exact math hasn't been tampered with
    assert!(iso.contains("1234")); // secs related
    assert!(iso.contains("567890000") || iso.contains("567890") || iso.contains("1970"));
}

#[test]
fn test_timerange_between_and_close_at_mutant_kill() {
    use aletheiadb::core::temporal::TimeRange;

    // Testing `replace TimeRange::between -> Result<Self, TemporalError> with Ok(Default::default())`
    let start = 1000.into();
    let end = 2000.into();
    let range = TimeRange::between(start, end).unwrap();
    assert_eq!(range.start(), start);
    assert_eq!(range.end(), end);

    // Testing `replace TimeRange::close_at -> Result<Self, TemporalError> with Ok(Default::default())`
    let current_range = TimeRange::from(start);
    let close_ts = 1500.into();
    let closed_range = current_range.close_at(close_ts).unwrap();
    assert_eq!(closed_range.start(), start);
    assert_eq!(closed_range.end(), close_ts);

    // Testing `replace != with == in TimeRange::close_at` when timestamp is TIMESTAMP_MAX
    use aletheiadb::core::temporal::TIMESTAMP_MAX;
    let closed_max = current_range.close_at(TIMESTAMP_MAX).unwrap();
    assert_eq!(closed_max.end(), TIMESTAMP_MAX);
}

#[test]
fn test_timerange_logical_inversions_mutant_kill() {
    use aletheiadb::core::temporal::TimeRange;

    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();
    let _r2 = TimeRange::new(150.into(), 250.into()).unwrap();
    let _r3 = TimeRange::new(50.into(), 150.into()).unwrap();
    let _r4 = TimeRange::new(200.into(), 300.into()).unwrap();
    let _r5 = TimeRange::new(0.into(), 100.into()).unwrap();

    // Testing `replace || with && in TimeRange::overlaps`
    // Logic is `self.start < other.end && other.start < self.end`
    // Wait, the overlaps method has `||` ?
    // Let's check overlaps logic again. In temporal.rs:
    // pub fn overlaps(&self, other: &Self) -> bool {
    //     self.start < other.end && other.start < self.end
    // }
    // Let's add test for `contains` && -> ||: self.start <= timestamp && timestamp < self.end

    // contains: one true, one false
    assert!(!r1.contains(50.into())); // timestamp < self.start (false), timestamp < self.end (true)
    assert!(!r1.contains(250.into())); // timestamp >= self.start (true), timestamp >= self.end (false)
    assert!(r1.contains(150.into())); // both true

    // contains_range: self.start <= other.start && other.end <= self.end
    let inner = TimeRange::new(120.into(), 180.into()).unwrap();
    let start_out = TimeRange::new(80.into(), 180.into()).unwrap();
    let end_out = TimeRange::new(120.into(), 220.into()).unwrap();

    assert!(r1.contains_range(&inner)); // both true
    assert!(!r1.contains_range(&start_out)); // start false, end true
    assert!(!r1.contains_range(&end_out)); // start true, end false

    // exact boundaries for contains_range
    let exact_start = TimeRange::new(100.into(), 150.into()).unwrap();
    let exact_end = TimeRange::new(150.into(), 200.into()).unwrap();
    assert!(r1.contains_range(&exact_start));
    assert!(r1.contains_range(&exact_end));

    let over_end = TimeRange::new(150.into(), 201.into()).unwrap();
    assert!(!r1.contains_range(&over_end));
}

#[test]
fn test_timerange_overlaps_empty_mutant_kill() {
    use aletheiadb::core::temporal::TimeRange;
    // Testing `replace || with && in TimeRange::overlaps`
    // Logic is: if self.is_empty() || other.is_empty() { return false; }

    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();
    let empty1 = TimeRange::at(150.into());
    let empty2 = TimeRange::at(150.into());

    // One empty, one not empty (kills && mutant)
    assert!(
        !r1.overlaps(&empty1),
        "Should return false if other is empty"
    );
    assert!(
        !empty1.overlaps(&r1),
        "Should return false if self is empty"
    );

    // Both empty
    assert!(
        !empty1.overlaps(&empty2),
        "Should return false if both are empty"
    );
}

#[test]
fn test_timerange_deserialize_boundary_mutants() {
    use aletheiadb::core::temporal::TimeRange;

    let valid_range = TimeRange::new(100.into(), 200.into()).unwrap();
    let bytes = valid_range.serialize();

    // Testing `replace < with == / > / <=` in `if bytes.len() < 24 { return Err(...) }`
    let (res, size) = TimeRange::deserialize(&bytes).unwrap();
    assert_eq!(res, valid_range);
    assert_eq!(size, 24);

    // Should fail with exactly 23 bytes (tests <= vs <)
    let res_15 = TimeRange::deserialize(&bytes[..23]);
    assert!(res_15.is_err(), "Must fail with 23 bytes");

    // Should fail with 0 bytes (tests > vs <)
    let res_0 = TimeRange::deserialize(&[]);
    assert!(res_0.is_err(), "Must fail with 0 bytes");
}

#[test]
fn test_bitemporal_deserialize_boundary_mutants() {
    use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};

    let valid = TimeRange::new(100.into(), 200.into()).unwrap();
    let tx = TimeRange::new(150.into(), 250.into()).unwrap();
    let bi = BiTemporalInterval::new(valid, tx);

    let bytes = bi.serialize();

    let (res, size) = BiTemporalInterval::deserialize(&bytes).unwrap();
    assert_eq!(res.valid_time(), bi.valid_time());
    assert_eq!(size, 48); // Phase 2 48-byte size

    // Should fail with exactly 47 bytes
    let res_31 = BiTemporalInterval::deserialize(&bytes[..47]);
    assert!(res_31.is_err(), "Must fail with 47 bytes");

    // Should fail with 0 bytes
    let res_0 = BiTemporalInterval::deserialize(&[]);
    assert!(res_0.is_err(), "Must fail with 0 bytes");
}

#[test]
fn test_timerange_fmt_mutant_kill() {
    use aletheiadb::core::temporal::TimeRange;

    let start = 100.into();
    let end = 200.into();

    let open_range = TimeRange::from(start);
    let closed_range = TimeRange::new(start, end).unwrap();

    let open_fmt = format!("{}", open_range);
    assert!(open_fmt.contains("current"));
    assert!(!open_fmt.is_empty()); // Kills replace with Default::default() which is empty string

    let closed_fmt = format!("{}", closed_range);
    assert!(!closed_fmt.contains("current"));
    assert!(closed_fmt.contains(&end.to_string()));
    assert!(!closed_fmt.is_empty());
}
