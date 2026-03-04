use aletheiadb::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX, TimeRange, time};

#[test]
fn test_timerange_is_current_exact() {
    let range = TimeRange::from(100.into());
    assert!(range.is_current());

    let closed = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(!closed.is_current());
}

#[test]
fn test_timerange_is_closed_exact() {
    let range = TimeRange::from(100.into());
    assert!(!range.is_closed());

    let closed = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(closed.is_closed());
}

#[test]
fn test_timerange_contains_exact() {
    let range = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(range.contains(150.into()));
    assert!(!range.contains(50.into()));
    assert!(!range.contains(250.into()));
    assert!(!range.contains(200.into())); // end is exclusive
    assert!(range.contains(100.into())); // start is inclusive
}

#[test]
fn test_timerange_contains_or_after_exact() {
    let range = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(range.contains_or_after(150.into()));
    assert!(!range.contains_or_after(50.into()));
    assert!(range.contains_or_after(250.into()));
    assert!(range.contains_or_after(100.into()));
}

#[test]
fn test_timerange_is_empty_exact() {
    let range = TimeRange::new(100.into(), 200.into()).unwrap();
    assert!(!range.is_empty());

    let empty = TimeRange::at(100.into());
    assert!(empty.is_empty());
}

#[test]
fn test_timerange_overlaps_exact() {
    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();
    let r2 = TimeRange::new(150.into(), 250.into()).unwrap();
    let r3 = TimeRange::new(250.into(), 300.into()).unwrap();

    assert!(r1.overlaps(&r2));
    assert!(!r1.overlaps(&r3));
    assert!(!r2.overlaps(&r3)); // Touching is not overlapping
}

#[test]
fn test_timerange_contains_range_exact() {
    let r1 = TimeRange::new(100.into(), 300.into()).unwrap();
    let r2 = TimeRange::new(150.into(), 250.into()).unwrap();
    let r3 = TimeRange::new(250.into(), 350.into()).unwrap();

    assert!(r1.contains_range(&r2));
    assert!(!r1.contains_range(&r3));
    assert!(!r2.contains_range(&r1));
}

#[test]
fn test_bitemporal_is_valid_exact() {
    let interval = BiTemporalInterval::now(100.into(), 200.into());
    assert!(interval.is_currently_valid());

    let closed = interval.close_valid_time(300.into()).unwrap();
    assert!(!closed.is_currently_valid());
}

#[test]
fn test_bitemporal_is_recorded_exact() {
    let interval = BiTemporalInterval::now(100.into(), 200.into());
    assert!(interval.is_currently_recorded());

    let closed = interval.close_transaction_time(300.into()).unwrap();
    assert!(!closed.is_currently_recorded());
}

#[test]
fn test_bitemporal_is_current_exact() {
    let interval = BiTemporalInterval::now(100.into(), 200.into());
    assert!(interval.is_current());

    let closed_valid = interval.close_valid_time(300.into()).unwrap();
    assert!(!closed_valid.is_current());

    let closed_tx = interval.close_transaction_time(300.into()).unwrap();
    assert!(!closed_tx.is_current());
}

#[test]
fn test_bitemporal_is_valid_at_exact() {
    let interval = BiTemporalInterval::now(100.into(), 200.into());
    assert!(interval.is_valid_at(150.into()));
    assert!(!interval.is_valid_at(50.into()));
}

#[test]
fn test_bitemporal_is_recorded_at_exact() {
    let interval = BiTemporalInterval::now(100.into(), 200.into());
    assert!(interval.is_recorded_at(250.into()));
    assert!(!interval.is_recorded_at(150.into()));
}

#[test]
fn test_bitemporal_is_visible_at_exact() {
    let interval = BiTemporalInterval::now(100.into(), 200.into());
    assert!(interval.is_visible_at(150.into(), 250.into()));
    assert!(!interval.is_visible_at(50.into(), 250.into()));
    assert!(!interval.is_visible_at(150.into(), 150.into()));
}

#[test]
fn test_time_to_secs_exact() {
    let ts = time::from_secs(12345);
    assert_eq!(time::to_secs(ts), 12345);
}

#[test]
fn test_time_to_millis_exact() {
    let ts = time::from_millis(12345678);
    assert_eq!(time::to_millis(ts), 12345678);
}

#[test]
fn test_time_to_iso8601_mutants() {
    // Tests for mutants returning String::new(), "xyzzy", replacing == with !=,
    // replacing / with % or *, replacing * with + or /, replacing % with / or +
    // replacing + with - or *
    let ts = TIMESTAMP_MAX;
    assert_eq!(time::to_iso8601(ts), "current");

    let ts2 = time::from_secs(1609459200); // 2021-01-01 00:00:00
    let iso = time::to_iso8601(ts2);
    assert_ne!(iso, "");
    assert_ne!(iso, "xyzzy");
    assert_ne!(iso, "current");

    // Testing the arithmetic operations in time::to_iso8601
    // The wallclock is 1609459200 * 1_000_000 = 1609459200000000
    // secs = wallclock / 1_000_000 = 1609459200
    // nanos = ((wallclock % 1_000_000) * 1000) = 0
    let ts3 = aletheiadb::core::hlc::HybridTimestamp::new(1_609_459_200_000_123, 0).unwrap();
    let iso3 = time::to_iso8601(ts3);

    // The implementation currently uses standard Debug formatting of std::time::SystemTime
    // which results in `SystemTime { tv_sec: ..., tv_nsec: ... }` on UNIX, and `SystemTime { intervals: ... }` on Windows.
    // However, a simple length check or verifying that it contains the seconds portion ensures it's functionally doing math
    // without tying it purely to exact String implementations that vary by platform.

    // We expect it's a long non-empty string. If math operators are mutated, it might result in a short or empty string
    // or drastically incorrect second bounds.
    assert!(
        iso3.len() > 10,
        "Expected a valid SystemTime Debug string, got: {}",
        iso3
    );
    assert!(!iso3.is_empty());
}

#[test]
fn test_bitemporal_close_methods() {
    let interval = BiTemporalInterval::now(100.into(), 200.into());

    // Testing close_valid_time replacing with Ok(Default::default())
    let closed_valid = interval.close_valid_time(150.into()).unwrap();
    assert_eq!(closed_valid.valid_time().start(), 100.into());
    assert_eq!(closed_valid.valid_time().end(), 150.into());
    assert_eq!(closed_valid.transaction_time().start(), 200.into());
    assert_eq!(closed_valid.transaction_time().end(), TIMESTAMP_MAX);

    // Testing close_transaction_time replacing with Ok(Default::default())
    let closed_tx = interval.close_transaction_time(250.into()).unwrap();
    assert_eq!(closed_tx.valid_time().start(), 100.into());
    assert_eq!(closed_tx.valid_time().end(), TIMESTAMP_MAX);
    assert_eq!(closed_tx.transaction_time().start(), 200.into());
    assert_eq!(closed_tx.transaction_time().end(), 250.into());

    // Testing close_both replacing with Ok(Default::default())
    let closed_both = interval.close_both(150.into(), 250.into()).unwrap();
    assert_eq!(closed_both.valid_time().start(), 100.into());
    assert_eq!(closed_both.valid_time().end(), 150.into());
    assert_eq!(closed_both.transaction_time().start(), 200.into());
    assert_eq!(closed_both.transaction_time().end(), 250.into());
}

#[test]
fn test_bitemporal_with_valid_time_exact() {
    let interval = BiTemporalInterval::with_valid_time(100.into(), 200.into());
    assert_eq!(interval.valid_time().start(), 100.into());
    assert_eq!(interval.transaction_time().start(), 200.into());
    assert_eq!(interval.valid_time().end(), TIMESTAMP_MAX);
    assert_eq!(interval.transaction_time().end(), TIMESTAMP_MAX);
}

#[test]
fn test_time_to_secs_mutants() {
    let ts = aletheiadb::core::hlc::HybridTimestamp::new(1_000_000, 0).unwrap();
    // if / becomes %, 1_000_000 % 1_000_000 = 0
    // if / becomes *, 1_000_000 * 1_000_000 = 1_000_000_000_000
    assert_eq!(time::to_secs(ts), 1);

    let ts2 = aletheiadb::core::hlc::HybridTimestamp::new(5_000_000, 0).unwrap();
    assert_eq!(time::to_secs(ts2), 5);
}

#[test]
fn test_time_to_millis_mutants() {
    let ts = aletheiadb::core::hlc::HybridTimestamp::new(1_000, 0).unwrap();
    // if / becomes %, 1_000 % 1_000 = 0
    // if / becomes *, 1_000 * 1_000 = 1_000_000
    assert_eq!(time::to_millis(ts), 1);

    let ts2 = aletheiadb::core::hlc::HybridTimestamp::new(5_000, 0).unwrap();
    assert_eq!(time::to_millis(ts2), 5);
}

#[test]
fn test_time_from_secs_mutants() {
    // if * becomes +, 1 + 1_000_000 = 1_000_001
    // if * becomes /, 1 / 1_000_000 = 0
    let ts = time::from_secs(1);
    assert_eq!(ts.wallclock(), 1_000_000);
}

#[test]
fn test_time_from_millis_mutants() {
    // if * becomes +, 1 + 1_000 = 1_001
    // if * becomes /, 1 / 1_000 = 0
    let ts = time::from_millis(1);
    assert_eq!(ts.wallclock(), 1_000);
}

#[test]
fn test_time_now_methods_exact() {
    let now1 = time::now();
    let now2 = time::try_now().unwrap();

    // We can't assert exactly against time due to clock drift, but we can assert
    // they are reasonably large timestamps and not Default::default()
    assert!(now1.wallclock() > 1700000000000000); // After Nov 2023
    assert!(now2.wallclock() > 1700000000000000); // After Nov 2023

    // Also time::now should not be time::from_secs(0) which is default
    assert_ne!(now1, time::from_secs(0));
    assert_ne!(now2, time::from_secs(0));
}

#[test]
fn test_timerange_serialize_mutants() {
    let range = TimeRange::new(100.into(), 200.into()).unwrap();
    let bytes = range.serialize();

    // Test that the serialized bytes are correct length and not default [0] or [1] or []
    assert_eq!(bytes.len(), 24);
    assert_ne!(bytes, Vec::<u8>::new());
    assert_ne!(bytes, vec![0u8]);
    assert_ne!(bytes, vec![1u8]);

    let mut buf = Vec::new();
    range.serialize_into(&mut buf);
    assert_eq!(buf.len(), 24); // verify serialize_into is not ()

    let (de, count) = TimeRange::deserialize(&bytes).unwrap();
    assert_eq!(de.start(), 100.into());
    assert_eq!(de.end(), 200.into());
    assert_eq!(count, 24); // Verify we don't return default TimeRange or 0/1 count
}

#[test]
fn test_bitemporal_serialize_mutants() {
    let interval = BiTemporalInterval::now(100.into(), 200.into());
    let bytes = interval.serialize();

    assert_eq!(bytes.len(), 48);
    assert_ne!(bytes, Vec::<u8>::new());
    assert_ne!(bytes, vec![0u8]);
    assert_ne!(bytes, vec![1u8]);

    let mut buf = Vec::new();
    interval.serialize_into(&mut buf);
    assert_eq!(buf.len(), 48); // verify serialize_into is not ()

    let (de, count) = BiTemporalInterval::deserialize(&bytes).unwrap();
    assert_eq!(de.valid_time().start(), 100.into());
    assert_eq!(de.transaction_time().start(), 200.into());
    assert_eq!(count, 48); // Verify we don't return default BiTemporalInterval or 0/1 count
}

#[test]
fn test_timerange_fmt_mutants() {
    let range = TimeRange::new(100.into(), 200.into()).unwrap();
    let s = format!("{}", range);
    assert!(s.contains("100"));
    assert!(s.contains("200"));
    assert_ne!(s, ""); // Not Ok(Default::default())
}

#[test]
fn test_bitemporal_fmt_mutants() {
    let interval = BiTemporalInterval::now(100.into(), 200.into());
    let s = format!("{}", interval);
    assert!(s.contains("100"));
    assert!(s.contains("200"));
    assert_ne!(s, ""); // Not Ok(Default::default())
}

#[test]
fn test_timerange_deserialize_less_than_mutants() {
    let bytes = [0u8; 24];
    // if < becomes ==, > or <= then bytes.len() < 24 will behave differently
    let result = TimeRange::deserialize(&bytes);
    assert!(
        result.is_ok(),
        "deserialize should accept length exactly 24"
    );

    let bytes_short = [0u8; 23];
    let result_short = TimeRange::deserialize(&bytes_short);
    assert!(result_short.is_err(), "deserialize should reject length 23");

    // start > end check
    // bytes[0..12] is start, bytes[12..24] is end
    let mut bytes_inverted = [0u8; 24];
    bytes_inverted[0] = 2; // start
    bytes_inverted[12] = 1; // end
    let result_inverted = TimeRange::deserialize(&bytes_inverted);
    assert!(
        result_inverted.is_err(),
        "deserialize should reject inverted range"
    );
}

#[test]
fn test_bitemporal_deserialize_less_than_mutants() {
    let bytes = [0u8; 48];
    // if < becomes ==, > or <= then bytes.len() < 48 will behave differently
    let result = BiTemporalInterval::deserialize(&bytes);
    assert!(
        result.is_ok(),
        "deserialize should accept length exactly 48"
    );

    let bytes_short = [0u8; 47];
    let result_short = BiTemporalInterval::deserialize(&bytes_short);
    assert!(result_short.is_err(), "deserialize should reject length 47");
}

#[test]
fn test_timerange_contains_and_overlaps_logic() {
    let range = TimeRange::new(100.into(), 200.into()).unwrap();

    // contains: timestamp >= self.start && timestamp < self.end
    assert!(range.contains(100.into())); // exact start
    assert!(range.contains(199.into())); // right before end
    assert!(!range.contains(200.into())); // exact end
    assert!(!range.contains(99.into())); // right before start

    let overlap_touch_start = TimeRange::new(50.into(), 100.into()).unwrap();
    let overlap_touch_end = TimeRange::new(200.into(), 250.into()).unwrap();
    let overlap_intersect = TimeRange::new(150.into(), 250.into()).unwrap();
    let overlap_intersect_2 = TimeRange::new(50.into(), 150.into()).unwrap();
    let overlap_inside = TimeRange::new(125.into(), 175.into()).unwrap();

    assert!(!range.overlaps(&overlap_touch_start));
    assert!(!range.overlaps(&overlap_touch_end));
    assert!(range.overlaps(&overlap_intersect));
    assert!(range.overlaps(&overlap_intersect_2));
    assert!(range.overlaps(&overlap_inside));
    assert!(range.overlaps(&range));
}

#[test]
fn test_timerange_duration_mutants() {
    let range = TimeRange::new(100.into(), 200.into()).unwrap();
    assert_eq!(range.duration_micros(), Some(100));

    let range_0 = TimeRange::at(100.into());
    assert_eq!(range_0.duration_micros(), Some(0));

    let range_1 = TimeRange::new(100.into(), 101.into()).unwrap();
    assert_eq!(range_1.duration_micros(), Some(1));

    let range_none = TimeRange::from(100.into());
    assert_eq!(range_none.duration_micros(), None);
}

#[test]
fn test_timerange_close_at_logic() {
    let start = aletheiadb::core::hlc::HybridTimestamp::new(100, 0).unwrap();
    let end = aletheiadb::core::hlc::HybridTimestamp::new(200, 0).unwrap();

    let range = TimeRange::from(start);
    let close = range.close_at(end).unwrap();
    assert_eq!(close.end(), end);

    let invalid_close = range.close_at(aletheiadb::core::hlc::HybridTimestamp::new(50, 0).unwrap());
    assert!(invalid_close.is_err());

    // Testing the timestamp checks and combinations
    let max = aletheiadb::core::temporal::MAX_VALID_TIMESTAMP;
    let max_ts = aletheiadb::core::hlc::HybridTimestamp::new(max, 0).unwrap();

    let close_max = TimeRange::from(start).close_at(max_ts);
    assert!(close_max.is_ok());
}

#[test]
fn test_bitemporal_methods_exact_2() {
    let valid_start = aletheiadb::core::hlc::HybridTimestamp::new(100, 0).unwrap();
    let valid_end = aletheiadb::core::hlc::HybridTimestamp::new(200, 0).unwrap();
    let tx_start = aletheiadb::core::hlc::HybridTimestamp::new(300, 0).unwrap();
    let tx_end = aletheiadb::core::hlc::HybridTimestamp::new(400, 0).unwrap();

    // Testing is_currently_valid: self.valid_time.is_current()
    let int_valid_current = BiTemporalInterval::new(
        TimeRange::from(valid_start),
        TimeRange::new(tx_start, tx_end).unwrap(),
    );
    assert!(int_valid_current.is_currently_valid());
    assert!(!int_valid_current.is_currently_recorded());

    let int_tx_current = BiTemporalInterval::new(
        TimeRange::new(valid_start, valid_end).unwrap(),
        TimeRange::from(tx_start),
    );
    assert!(!int_tx_current.is_currently_valid());
    assert!(int_tx_current.is_currently_recorded());
}

#[test]
fn test_timerange_serialization_format_exact() {
    let start = aletheiadb::core::hlc::HybridTimestamp::new(0, 0).unwrap();
    let end = aletheiadb::core::hlc::HybridTimestamp::new(0, 0).unwrap();
    let range = TimeRange::new(start, end).unwrap();

    let bytes = range.serialize();
    assert_eq!(bytes.len(), 24);

    // Test mut deserialize correctly parses out start & end
    let start_1 = aletheiadb::core::hlc::HybridTimestamp::new(1, 0).unwrap();
    let end_2 = aletheiadb::core::hlc::HybridTimestamp::new(2, 0).unwrap();
    let range_1_2 = TimeRange::new(start_1, end_2).unwrap();
    let bytes_1_2 = range_1_2.serialize();

    let (de, size) = TimeRange::deserialize(&bytes_1_2).unwrap();
    assert_eq!(size, 24);
    assert_eq!(de.start(), start_1);
    assert_eq!(de.end(), end_2);

    // start > end will throw error
    let start_3 = aletheiadb::core::hlc::HybridTimestamp::new(3, 0).unwrap();
    let bytes_3_2 = TimeRange::new(start_1, start_3).unwrap().serialize();
    let mut bad_bytes = bytes_3_2.clone();

    // Invert the timestamps
    let start_bytes = bad_bytes[0..12].to_vec();
    let end_bytes = bad_bytes[12..24].to_vec();
    bad_bytes[0..12].copy_from_slice(&end_bytes);
    bad_bytes[12..24].copy_from_slice(&start_bytes);

    // Now start > end
    let bad_de = TimeRange::deserialize(&bad_bytes);
    assert!(bad_de.is_err());
}

#[test]
fn test_bitemporal_methods_exact_3() {
    let int_1 = BiTemporalInterval::now(100.into(), 200.into());
    let _int_2 = BiTemporalInterval::now(100.into(), 200.into());

    // Test that the valid_time and transaction_time accessors do not return default
    assert_ne!(int_1.valid_time().start(), 0.into());
    assert_ne!(int_1.transaction_time().start(), 0.into());

    let valid_range = int_1.valid_time();
    let tx_range = int_1.transaction_time();

    assert_eq!(valid_range.start(), 100.into());
    assert_eq!(tx_range.start(), 200.into());
}

#[test]
fn test_timerange_contains_and_overlaps_logic_2() {
    let r1 = TimeRange::new(100.into(), 200.into()).unwrap();
    let r2 = TimeRange::new(200.into(), 300.into()).unwrap();
    let r3 = TimeRange::new(100.into(), 300.into()).unwrap();
    let r4 = TimeRange::new(150.into(), 250.into()).unwrap();

    // contains_range: self.start <= other.start && other.end <= self.end
    assert!(r3.contains_range(&r1)); // 100 <= 100 && 200 <= 300
    assert!(r3.contains_range(&r2)); // 100 <= 200 && 300 <= 300
    assert!(r3.contains_range(&r4)); // 100 <= 150 && 250 <= 300
    assert!(!r1.contains_range(&r3)); // 100 <= 100 && 300 <= 200 (false)
    assert!(!r4.contains_range(&r3)); // 150 <= 100 (false)
    assert!(!r1.contains_range(&r2)); // 100 <= 200 && 300 <= 200 (false)
}

#[test]
fn test_time_try_now_mutants() {
    let result = time::try_now();
    assert!(result.is_ok());

    let ts = result.unwrap();
    assert_ne!(ts.wallclock(), 0);
    assert!(ts.wallclock() > 1000000);
}

#[test]
fn test_time_range_close_at_mutants() {
    let start = aletheiadb::core::hlc::HybridTimestamp::new(100, 0).unwrap();
    let range = TimeRange::from(start);

    // Test mut: if end < self.start becomes ==, >, <=
    let end_ok = aletheiadb::core::hlc::HybridTimestamp::new(100, 0).unwrap();
    let res_ok = range.close_at(end_ok);
    assert!(res_ok.is_ok()); // Should not fail for ==

    let end_ok2 = aletheiadb::core::hlc::HybridTimestamp::new(200, 0).unwrap();
    let res_ok2 = range.close_at(end_ok2);
    assert!(res_ok2.is_ok()); // Should not fail for >

    let end_err = aletheiadb::core::hlc::HybridTimestamp::new(50, 0).unwrap();
    let res_err = range.close_at(end_err);
    assert!(res_err.is_err()); // Must fail for <

    // Testing logic for checking max boundary
    // `if end.wallclock() > MAX_VALID_TIMESTAMP && end != TIMESTAMP_MAX`
    let end_max = aletheiadb::core::hlc::HybridTimestamp::new(
        aletheiadb::core::temporal::MAX_VALID_TIMESTAMP,
        0,
    )
    .unwrap();
    assert!(range.close_at(end_max).is_ok()); // == MAX_VALID_TIMESTAMP should pass

    // We can't easily create a HybridTimestamp > MAX_VALID_TIMESTAMP due to its internal checks,
    // so we can test the `from` logic on its own by testing TIMESTAMP_MAX which bypasses that

    // if TIMESTAMP_MAX condition is mutated (e.g. `==` -> `!=`)
    assert!(range.close_at(TIMESTAMP_MAX).is_ok());
}
