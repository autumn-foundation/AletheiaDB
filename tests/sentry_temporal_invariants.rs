#[cfg(test)]
mod tests {
    use aletheiadb::core::error::TemporalError;
    use aletheiadb::core::hlc::HybridTimestamp;
    use aletheiadb::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX, TimeRange, time};

    #[test]
    fn test_timerange_close_at_enforces_invariant() {
        let start = time::from_secs(100);
        let range = TimeRange::from(start);

        // Attempt to close at a time BEFORE start
        let invalid_end = time::from_secs(50);
        let result = range.close_at(invalid_end);

        // This MUST fail now
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            TemporalError::InvalidTimeRange { .. }
        ));
    }

    #[test]
    fn test_bitemporal_close_enforces_invariant() {
        let start = time::from_secs(100);
        let interval = BiTemporalInterval::current(start);

        let invalid_end = time::from_secs(50);
        let result = interval.close_valid_time(invalid_end);

        // This MUST fail now
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            TemporalError::InvalidTimeRange { .. }
        ));
    }

    #[test]
    fn test_timerange_is_closed_strict() {
        // kill replace < with ==, >, <= in is_closed
        let closed = TimeRange::new(100.into(), 200.into()).unwrap();
        assert!(closed.is_closed()); // 200 < TIMESTAMP_MAX is true

        let current = TimeRange::from(100.into());
        assert!(!current.is_closed()); // TIMESTAMP_MAX < TIMESTAMP_MAX is false
    }

    #[test]
    fn test_timerange_contains_strict() {
        // kill replace < with ==, >, <= in contains
        // kill replace >= with < in contains
        // kill replace && with || in contains
        let range = TimeRange::new(100.into(), 200.into()).unwrap();

        assert!(!range.contains(99.into())); // timestamp >= start is false, testing &&
        assert!(range.contains(100.into())); // testing boundary
        assert!(range.contains(199.into())); // testing boundary
        assert!(!range.contains(200.into())); // testing boundary, timestamp < end is false
    }

    #[test]
    fn test_timerange_contains_or_after_strict() {
        // kill replace >= with < in contains_or_after
        let range = TimeRange::new(100.into(), 200.into()).unwrap();

        assert!(range.contains_or_after(100.into())); // testing boundary >=
        assert!(!range.contains_or_after(99.into()));
        assert!(range.contains_or_after(250.into()));
    }

    #[test]
    fn test_timerange_is_empty_strict() {
        // kill replace == with != in is_empty
        let empty = TimeRange::new(100.into(), 100.into()).unwrap();
        assert!(empty.is_empty());

        let not_empty = TimeRange::new(100.into(), 101.into()).unwrap();
        assert!(!not_empty.is_empty());
    }

    #[test]
    fn test_timerange_overlaps_strict() {
        // kill replace || with && in overlaps early exit
        let empty_r1 = TimeRange::new(100.into(), 100.into()).unwrap();
        let r2 = TimeRange::new(100.into(), 200.into()).unwrap();

        // empty_r1 is empty, r2 is not empty. empty_r1.is_empty() || r2.is_empty() should be true.
        // If mutated to &&, it would be false, continuing to evaluate overlaps and might return true.
        assert!(!empty_r1.overlaps(&r2));
        assert!(!r2.overlaps(&empty_r1));

        // kill replace && with || and < with ==, >, <= in overlaps main logic
        let r3 = TimeRange::new(150.into(), 250.into()).unwrap();
        assert!(r2.overlaps(&r3)); // 100 < 250 && 150 < 200 (true)

        let r4 = TimeRange::new(200.into(), 300.into()).unwrap();
        assert!(!r2.overlaps(&r4)); // 100 < 300 && 200 < 200 (false, touching)

        let r5 = TimeRange::new(50.into(), 100.into()).unwrap();
        assert!(!r2.overlaps(&r5)); // 100 < 100 && 50 < 200 (false, touching)
    }

    #[test]
    fn test_timerange_contains_range_strict() {
        // kill replace <= with > in contains_range
        // kill replace && with || in contains_range
        let outer = TimeRange::new(100.into(), 300.into()).unwrap();

        let inner1 = TimeRange::new(150.into(), 250.into()).unwrap();
        assert!(outer.contains_range(&inner1));

        let same_start = TimeRange::new(100.into(), 200.into()).unwrap();
        assert!(outer.contains_range(&same_start)); // 100 <= 100 && 200 <= 300

        let same_end = TimeRange::new(150.into(), 300.into()).unwrap();
        assert!(outer.contains_range(&same_end)); // 100 <= 150 && 300 <= 300

        let before = TimeRange::new(50.into(), 150.into()).unwrap();
        assert!(!outer.contains_range(&before)); // 100 <= 50 (false)

        let after = TimeRange::new(250.into(), 350.into()).unwrap();
        assert!(!outer.contains_range(&after)); // 350 <= 300 (false)
    }

    #[test]
    fn test_timerange_close_at_strict() {
        // kill replace < with ==, >, <= in close_at boundary check
        let range = TimeRange::from(100.into());

        let err = range.close_at(99.into());
        assert!(err.is_err(), "Cannot close before start");
        assert!(matches!(
            err.unwrap_err(),
            TemporalError::InvalidTimeRange { .. }
        ));

        let ok = range.close_at(100.into());
        assert!(ok.is_ok(), "Can close at exactly start time");

        let ok2 = range.close_at(200.into());
        assert!(ok2.is_ok());

        // kill replace > with ==, <, >= in MAX_VALID_TIMESTAMP check
        use aletheiadb::core::temporal::MAX_VALID_TIMESTAMP;
        let max_valid = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap();

        let ok_max = range.close_at(max_valid);
        assert!(ok_max.is_ok(), "Can close at MAX_VALID_TIMESTAMP");
    }

    #[test]
    fn test_bitemporal_methods_not_default() {
        // kill replace BiTemporalInterval::current -> Self with Default::default()
        let current = BiTemporalInterval::current(100.into());
        assert_ne!(current.valid_time().start(), 0.into());
        assert_ne!(current.transaction_time().start(), 0.into());
        assert_eq!(current.valid_time().start(), 100.into());

        // kill replace BiTemporalInterval::now -> Self with Default::default()
        let now = BiTemporalInterval::now(100.into(), 200.into());
        assert_eq!(now.valid_time().start(), 100.into());
        assert_eq!(now.transaction_time().start(), 200.into());

        // kill replace BiTemporalInterval::with_valid_time -> Self with Default::default()
        let with = BiTemporalInterval::with_valid_time(100.into(), 200.into());
        assert_eq!(with.valid_time().start(), 100.into());
        assert_eq!(with.transaction_time().start(), 200.into());

        // kill replace BiTemporalInterval getters -> TimeRange with Default::default()
        assert_eq!(now.valid_time(), TimeRange::from(100.into()));
        assert_eq!(now.transaction_time(), TimeRange::from(200.into()));
    }

    #[test]
    fn test_bitemporal_boolean_strict() {
        // kill replacing is_currently_valid, is_currently_recorded, etc with true/false
        let open_valid_closed_tx = BiTemporalInterval::new(
            TimeRange::from(100.into()),
            TimeRange::new(200.into(), 300.into()).unwrap(),
        );
        assert!(open_valid_closed_tx.is_currently_valid());
        assert!(!open_valid_closed_tx.is_currently_recorded());
        assert!(!open_valid_closed_tx.is_current());

        let closed_valid_open_tx = BiTemporalInterval::new(
            TimeRange::new(100.into(), 200.into()).unwrap(),
            TimeRange::from(300.into()),
        );
        assert!(!closed_valid_open_tx.is_currently_valid());
        assert!(closed_valid_open_tx.is_currently_recorded());
        assert!(!closed_valid_open_tx.is_current());

        let fully_open = BiTemporalInterval::current(100.into());
        assert!(fully_open.is_currently_valid());
        assert!(fully_open.is_currently_recorded());
        assert!(fully_open.is_current());

        let fully_closed = BiTemporalInterval::new(
            TimeRange::new(100.into(), 200.into()).unwrap(),
            TimeRange::new(300.into(), 400.into()).unwrap(),
        );
        assert!(!fully_closed.is_currently_valid());
        assert!(!fully_closed.is_currently_recorded());
        assert!(!fully_closed.is_current());

        // is_valid_at / is_recorded_at
        assert!(fully_closed.is_valid_at(150.into()));
        assert!(!fully_closed.is_valid_at(250.into()));

        assert!(fully_closed.is_recorded_at(350.into()));
        assert!(!fully_closed.is_recorded_at(450.into()));

        // is_visible_at
        assert!(fully_closed.is_visible_at(150.into(), 350.into()));
        assert!(!fully_closed.is_visible_at(250.into(), 350.into())); // valid_time out
        assert!(!fully_closed.is_visible_at(150.into(), 450.into())); // tx_time out
    }

    #[test]
    fn test_bitemporal_close_methods() {
        let now = BiTemporalInterval::current(100.into());

        let closed_valid = now.close_valid_time(200.into()).unwrap();
        assert!(!closed_valid.is_currently_valid());
        assert!(closed_valid.is_currently_recorded());
        assert_eq!(closed_valid.valid_time().end(), 200.into());

        let now2 = BiTemporalInterval::current(100.into());
        let closed_tx = now2.close_transaction_time(300.into()).unwrap();
        assert!(closed_tx.is_currently_valid());
        assert!(!closed_tx.is_currently_recorded());
        assert_eq!(closed_tx.transaction_time().end(), 300.into());

        let now3 = BiTemporalInterval::current(100.into());
        let closed_both = now3.close_both(200.into(), 300.into()).unwrap();
        assert!(!closed_both.is_currently_valid());
        assert!(!closed_both.is_currently_recorded());
        assert_eq!(closed_both.valid_time().end(), 200.into());
        assert_eq!(closed_both.transaction_time().end(), 300.into());
    }

    #[test]
    fn test_serialize_deserialize_strict() {
        let range = TimeRange::new(100.into(), 200.into()).unwrap();
        let bytes = range.serialize();
        assert_eq!(bytes.len(), 24);

        let (deser, count) = TimeRange::deserialize(&bytes).unwrap();
        assert_eq!(deser, range);
        assert_eq!(count, 24);

        // check length mutant
        let err = TimeRange::deserialize(&bytes[..23]);
        assert!(err.is_err(), "Must fail if less than 24 bytes");

        let bitemp = BiTemporalInterval::new(
            TimeRange::new(100.into(), 200.into()).unwrap(),
            TimeRange::new(300.into(), 400.into()).unwrap(),
        );
        let bytes2 = bitemp.serialize();
        assert_eq!(bytes2.len(), 48);

        let (deser2, count2) = BiTemporalInterval::deserialize(&bytes2).unwrap();
        assert_eq!(deser2, bitemp);
        assert_eq!(count2, 48);

        // check length mutant
        let err2 = BiTemporalInterval::deserialize(&bytes2[..47]);
        assert!(err2.is_err(), "Must fail if less than 48 bytes");
    }

    #[test]
    fn test_time_conversions_math_strict() {
        // kill replace * with + or /
        let ts_secs = time::from_secs(5);
        assert_eq!(ts_secs.wallclock(), 5_000_000);

        let ts_millis = time::from_millis(5);
        assert_eq!(ts_millis.wallclock(), 5_000);

        // kill replace / with * or %
        let ts = HybridTimestamp::new(5_000_000, 0).unwrap();
        assert_eq!(time::to_secs(ts), 5);

        let ts2 = HybridTimestamp::new(5_000, 0).unwrap();
        assert_eq!(time::to_millis(ts2), 5);
    }

    #[test]
    fn test_timerange_duration_micros_all_cases() {
        let closed = TimeRange::new(150_000.into(), 250_000.into()).unwrap();
        assert_eq!(closed.duration_micros(), Some(100_000));

        let point = TimeRange::at(150_000.into());
        assert_eq!(point.duration_micros(), Some(0));

        let open = TimeRange::from(150_000.into());
        assert_eq!(open.duration_micros(), None);
    }

    #[test]
    fn test_time_now_strict() {
        // kill replace try_now / now with Ok(Default::default())
        let t1 = time::now();
        assert_ne!(t1.wallclock(), 0);

        let t2 = time::try_now().unwrap();
        assert_ne!(t2.wallclock(), 0);
    }

    #[test]
    fn test_time_to_iso8601_strict() {
        // kill replace == with !=
        let t_max = TIMESTAMP_MAX;
        assert_eq!(time::to_iso8601(t_max), "current");

        // kill replace time::to_iso8601 -> String with String::new() or "xyzzy"
        let t = time::from_secs(1609459200);
        let iso = time::to_iso8601(t);
        assert!(!iso.is_empty());
        assert_ne!(iso, "xyzzy");

        // exact math logic killed by earlier tests
    }
}
