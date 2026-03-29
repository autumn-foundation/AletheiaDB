import re

with open("src/core/temporal.rs", "r") as f:
    content = f.read()

tests = """
    #[test]
    fn test_sentry_bitemporal_methods() {
        let valid_start = crate::core::temporal::HybridTimestamp::new(100, 0).unwrap();
        let valid_end = crate::core::temporal::HybridTimestamp::new(200, 0).unwrap();
        let tx_start = crate::core::temporal::HybridTimestamp::new(150, 0).unwrap();
        let tx_end = crate::core::temporal::HybridTimestamp::new(250, 0).unwrap();

        let bti = BiTemporalInterval::new(
            TimeRange::new(valid_start, valid_end).unwrap(),
            TimeRange::new(tx_start, tx_end).unwrap()
        );

        assert_eq!(bti.valid_time().start(), valid_start);
        assert_eq!(bti.transaction_time().start(), tx_start);

        let now = BiTemporalInterval::now(valid_start, tx_start);
        assert!(now.is_current());
        assert!(now.is_currently_valid());
        assert!(now.is_currently_recorded());

        let with_valid = BiTemporalInterval::with_valid_time(valid_start, tx_start);
        assert_eq!(with_valid.valid_time().start(), valid_start);
        assert_eq!(with_valid.transaction_time().start(), tx_start);
        assert!(with_valid.is_current());

        assert!(with_valid.is_valid_at(crate::core::temporal::HybridTimestamp::new(100, 0).unwrap()));
        assert!(!with_valid.is_valid_at(crate::core::temporal::HybridTimestamp::new(50, 0).unwrap()));

        assert!(with_valid.is_recorded_at(crate::core::temporal::HybridTimestamp::new(150, 0).unwrap()));
        assert!(!with_valid.is_recorded_at(crate::core::temporal::HybridTimestamp::new(100, 0).unwrap()));

        assert!(with_valid.is_visible_at(crate::core::temporal::HybridTimestamp::new(100, 0).unwrap(), crate::core::temporal::HybridTimestamp::new(150, 0).unwrap()));
        assert!(!with_valid.is_visible_at(crate::core::temporal::HybridTimestamp::new(50, 0).unwrap(), crate::core::temporal::HybridTimestamp::new(150, 0).unwrap()));
    }

    #[test]
    fn test_sentry_time_conversions() {
        let ts = time::from_secs(10);
        assert_eq!(time::to_secs(ts), 10);

        let ts2 = time::from_millis(10000);
        assert_eq!(time::to_millis(ts2), 10000);
    }

    #[test]
    fn test_sentry_timerange_contains_or_after_mutants() {
        let range = TimeRange::new(100.into(), 200.into()).unwrap();
        assert!(!range.contains_or_after(99.into()));
        assert!(range.contains_or_after(100.into()));
        assert!(range.contains_or_after(150.into()));
        assert!(range.contains_or_after(200.into()));
        assert!(range.contains_or_after(250.into()));
    }

    #[test]
    fn test_sentry_timerange_is_empty_mutants() {
        let range1 = TimeRange::new(100.into(), 200.into()).unwrap();
        assert!(!range1.is_empty());

        let range2 = TimeRange::at(100.into());
        assert!(range2.is_empty());
    }

    #[test]
    fn test_sentry_bitemporal_close_mutants() {
        let bti = BiTemporalInterval::now(100.into(), 100.into());

        let closed_valid = bti.close_valid_time(200.into()).unwrap();
        assert!(!closed_valid.is_currently_valid());
        assert!(closed_valid.is_currently_recorded());
        assert_eq!(closed_valid.valid_time().end(), 200.into());

        let closed_tx = bti.close_transaction_time(200.into()).unwrap();
        assert!(closed_tx.is_currently_valid());
        assert!(!closed_tx.is_currently_recorded());
        assert_eq!(closed_tx.transaction_time().end(), 200.into());

        let closed_both = bti.close_both(200.into(), 300.into()).unwrap();
        assert!(!closed_both.is_currently_valid());
        assert!(!closed_both.is_currently_recorded());
        assert_eq!(closed_both.valid_time().end(), 200.into());
        assert_eq!(closed_both.transaction_time().end(), 300.into());
    }

    #[test]
    fn test_sentry_timerange_duration_micros_mutants() {
        let range1 = TimeRange::new(100.into(), 200.into()).unwrap();
        assert_eq!(range1.duration_micros(), Some(100));

        let range2 = TimeRange::from(100.into());
        assert_eq!(range2.duration_micros(), None);
    }

    #[test]
    fn test_sentry_timerange_is_closed_mutants() {
        let range1 = TimeRange::new(100.into(), 200.into()).unwrap();
        assert!(range1.is_closed());

        let range2 = TimeRange::from(100.into());
        assert!(!range2.is_closed());
    }

    #[test]
    fn test_sentry_timerange_serialize_mutants() {
        let range = TimeRange::new(100.into(), 200.into()).unwrap();
        let bytes = range.serialize();
        assert_eq!(bytes.len(), 24); // 12 bytes each for start and end

        // We can test deserialization returns same value
        let (deserialized, size) = TimeRange::deserialize(&bytes).unwrap();
        assert_eq!(size, 24);
        assert_eq!(deserialized.start(), crate::core::temporal::Timestamp::from(100));
        assert_eq!(deserialized.end(), crate::core::temporal::Timestamp::from(200));
    }

    #[test]
    fn test_sentry_bitemporal_serialize_mutants() {
        let valid_start = crate::core::temporal::HybridTimestamp::new(100, 0).unwrap();
        let valid_end = crate::core::temporal::HybridTimestamp::new(200, 0).unwrap();
        let tx_start = crate::core::temporal::HybridTimestamp::new(150, 0).unwrap();
        let tx_end = crate::core::temporal::HybridTimestamp::new(250, 0).unwrap();

        let bti = BiTemporalInterval::new(
            TimeRange::new(valid_start, valid_end).unwrap(),
            TimeRange::new(tx_start, tx_end).unwrap()
        );
        let bytes = bti.serialize();
        assert_eq!(bytes.len(), 48);

        let (deserialized, size) = BiTemporalInterval::deserialize(&bytes).unwrap();
        assert_eq!(size, 48);
        assert_eq!(deserialized.valid_time().start(), valid_start);
    }
"""

if "mod sentry_tests {" in content and "test_sentry_bitemporal_methods" not in content:
    content = content.replace("mod sentry_tests {", "mod sentry_tests {\n" + tests)

with open("src/core/temporal.rs", "w") as f:
    f.write(content)
