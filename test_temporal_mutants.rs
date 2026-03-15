#[cfg(test)]
mod additional_sentry_tests {
    use super::*;

    #[test]
    fn test_sentry_bitemporal_is_visible_at_strict_and() {
        // 🛡️ Sentry Test: Verify BiTemporalInterval::is_visible_at strictly requires BOTH dimensions.
        // This kills the mutant replacing `&&` with `||` in `is_visible_at`.
        let valid_start = 1000.into();
        let valid_end = 2000.into();
        let tx_start = 3000.into();
        let tx_end = 4000.into();

        let interval = BiTemporalInterval::new(
            TimeRange::new(valid_start, valid_end).unwrap(),
            TimeRange::new(tx_start, tx_end).unwrap(),
        );

        let valid_time_in_range = 1500.into();
        let tx_time_out_of_range = 5000.into();

        let valid_time_out_of_range = 5000.into();
        let tx_time_in_range = 3500.into();

        // Valid is in range, Tx is NOT. If `||` mutant is active, this returns true.
        assert!(
            !interval.is_visible_at(valid_time_in_range, tx_time_out_of_range),
            "is_visible_at should return false if tx_time is out of range"
        );

        // Tx is in range, Valid is NOT. If `||` mutant is active, this returns true.
        assert!(
            !interval.is_visible_at(valid_time_out_of_range, tx_time_in_range),
            "is_visible_at should return false if valid_time is out of range"
        );
    }

    #[test]
    fn test_sentry_duration_micros_exact() {
        // 🛡️ Sentry Test: Verify TimeRange::duration_micros returns exact duration.
        // This kills mutants replacing `duration_micros` with `Some(0)`, `Some(1)`, `Some(-1)`.
        let start = 1000.into();
        let end = 1005.into(); // Duration = 5

        let range = TimeRange::new(start, end).unwrap();
        assert_eq!(
            range.duration_micros(),
            Some(5),
            "duration_micros should return exact difference"
        );
    }

    #[test]
    fn test_sentry_is_currently_valid_and_recorded() {
        // 🛡️ Sentry Test: Verify is_currently_valid and is_currently_recorded.
        // This kills mutants replacing these methods with `true` or `false`.
        let start = 1000.into();
        let end = 2000.into();

        // Closed valid time, Open tx time
        let interval_closed_valid = BiTemporalInterval::new(
            TimeRange::new(start, end).unwrap(),
            TimeRange::from(start),
        );

        assert!(
            !interval_closed_valid.is_currently_valid(),
            "is_currently_valid should be false for closed valid time"
        );
        assert!(
            interval_closed_valid.is_currently_recorded(),
            "is_currently_recorded should be true for open tx time"
        );

        // Open valid time, Closed tx time
        let interval_closed_tx = BiTemporalInterval::new(
            TimeRange::from(start),
            TimeRange::new(start, end).unwrap(),
        );

        assert!(
            interval_closed_tx.is_currently_valid(),
            "is_currently_valid should be true for open valid time"
        );
        assert!(
            !interval_closed_tx.is_currently_recorded(),
            "is_currently_recorded should be false for closed tx time"
        );
    }

    #[test]
    fn test_sentry_time_helpers_exact_math() {
        // 🛡️ Sentry Test: Verify exact mathematical conversions in time:: helpers.
        // Kills mutants replacing `*` with `/`, `+` and `/` with `%`, `*` in:
        // to_secs, to_millis, from_secs, from_millis.

        let secs = 5;
        let millis = 5000;

        let ts_from_secs = time::from_secs(secs);
        assert_eq!(ts_from_secs.wallclock(), 5_000_000);

        let ts_from_millis = time::from_millis(millis);
        assert_eq!(ts_from_millis.wallclock(), 5_000_000);

        // to_secs and to_millis tests
        // Use a value that isn't symmetric or 1 to catch operator swaps
        use crate::core::hlc::HybridTimestamp;
        let ts = HybridTimestamp::new_unchecked(12_000_000, 0); // 12 seconds, 12000 millis

        assert_eq!(
            time::to_secs(ts),
            12,
            "to_secs should divide wallclock by 1_000_000"
        );
        assert_eq!(
            time::to_millis(ts),
            12000,
            "to_millis should divide wallclock by 1_000"
        );
    }

    #[test]
    fn test_sentry_time_try_now_error() {
        // 🛡️ Sentry Test: Verify time::try_now returns the expected TemporalError variant
        // if the system time is somehow before the UNIX epoch.
        // This is a bit tricky to mock, but we can verify that the function is at least
        // trying to do the right thing by running it and making sure it returns Ok(Timestamp)
        // instead of unconditionally returning Ok(Default::default())

        let now = time::try_now().unwrap();
        assert!(
            now.wallclock() > 1_000_000,
            "try_now should return a realistic timestamp, not default (0)"
        );
    }

    #[test]
    fn test_sentry_timerange_overlaps_mutants() {
        // 🛡️ Sentry Test: Kill overlaps mutants
        // Mutants:
        // replace || with &&
        // replace < with <=, ==, >
        let r1 = TimeRange::new(100.into(), 200.into()).unwrap();
        let r2 = TimeRange::new(150.into(), 250.into()).unwrap();

        // Ensure that overlaps works (kills `false` mutant)
        assert!(r1.overlaps(&r2));
        assert!(r2.overlaps(&r1));

        // Ensure that `is_empty` ranges do not overlap.
        // `self.is_empty() || other.is_empty()` -> mutated to `&&` means it would only
        // return false early if BOTH are empty. If only one is empty, it proceeds to check logic.
        // Let's create an empty range and a non-empty range.
        let empty = TimeRange::at(150.into());
        let non_empty = TimeRange::new(100.into(), 200.into()).unwrap();

        assert!(!empty.overlaps(&non_empty), "Empty range should not overlap (kills || to && mutant)");
        assert!(!non_empty.overlaps(&empty), "Empty range should not overlap (kills || to && mutant)");
    }
}
