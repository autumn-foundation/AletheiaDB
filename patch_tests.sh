cat << 'INNER_EOF' >> src/core/temporal.rs

#[cfg(test)]
mod tests_sentry_exact {
    use super::*;
    use crate::core::hlc::HybridTimestamp;

    #[test]
    fn test_timerange_is_closed_exact() {
        let ts = 100_000.into();

        let current_range = TimeRange::from(ts);
        assert_eq!(current_range.is_closed(), false);

        let closed_range = TimeRange::new(ts, 200_000.into()).unwrap();
        assert_eq!(closed_range.is_closed(), true);

        let almost_max = TimeRange::new(ts, HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).unwrap()).unwrap();
        assert_eq!(almost_max.is_closed(), true, "almost max should be closed");
    }

    #[test]
    fn test_timerange_contains_exact() {
        let start = 100_000.into();
        let end = 200_000.into();
        let range = TimeRange::new(start, end).unwrap();

        assert_eq!(range.contains(start), true, "contains start");
        assert_eq!(range.contains(99_999.into()), false, "does not contain before start");

        assert_eq!(range.contains(end), false, "does not contain end (exclusive)");
        assert_eq!(range.contains(199_999.into()), true, "contains right before end");
    }

    #[test]
    fn test_timerange_contains_or_after_exact() {
        let start = 100_000.into();
        let range = TimeRange::from(start);

        assert_eq!(range.contains_or_after(start), true, "contains start");
        assert_eq!(range.contains_or_after(99_999.into()), false, "does not contain before start");
        assert_eq!(range.contains_or_after(200_000.into()), true, "contains after start");
    }

    #[test]
    fn test_timerange_overlaps_exact() {
        let start1 = 100.into();
        let end1 = 200.into();
        let r1 = TimeRange::new(start1, end1).unwrap();

        let r2 = TimeRange::new(150.into(), 250.into()).unwrap();
        assert_eq!(r1.overlaps(&r2), true);
        assert_eq!(r2.overlaps(&r1), true);

        let touch_right = TimeRange::new(200.into(), 300.into()).unwrap();
        assert_eq!(r1.overlaps(&touch_right), false, "touch right no overlap");

        let touch_left = TimeRange::new(0.into(), 100.into()).unwrap();
        assert_eq!(r1.overlaps(&touch_left), false, "touch left no overlap");

        let empty1 = TimeRange::at(150.into());
        assert_eq!(r1.overlaps(&empty1), false, "empty internal no overlap");

        let r_valid = TimeRange::new(10.into(), 20.into()).unwrap();
        let empty_other = TimeRange::at(50.into());
        assert_eq!(empty_other.overlaps(&r_valid), false);
    }

    #[test]
    fn test_timerange_close_at_boundaries_exact() {
        let start = HybridTimestamp::new(100, 0).unwrap();
        let range = TimeRange::from(start);

        let before_start = HybridTimestamp::new(99, 0).unwrap();
        assert!(range.clone().close_at(before_start).is_err());

        let at_start = HybridTimestamp::new(100, 0).unwrap();
        let res = range.clone().close_at(at_start).unwrap();
        assert_eq!(res.end(), at_start);

        let max_valid_plus_one = HybridTimestamp::new_unchecked(MAX_VALID_TIMESTAMP + 1, 0);
        assert!(range.clone().close_at(max_valid_plus_one).is_err());

        let max_range = range.clone().close_at(TIMESTAMP_MAX).unwrap();
        assert_eq!(max_range.end(), TIMESTAMP_MAX);
    }
}
INNER_EOF
