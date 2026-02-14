
#[cfg(test)]
mod tests {
    use aletheiadb::core::temporal::{TimeRange, BiTemporalInterval, time};
    use aletheiadb::utils::error::TemporalError;

    #[test]
    fn test_timerange_close_at_enforces_invariant() {
        let start = time::from_secs(100);
        let range = TimeRange::from(start);

        // Attempt to close at a time BEFORE start
        let invalid_end = time::from_secs(50);
        let result = range.close_at(invalid_end);

        // This MUST fail now
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TemporalError::InvalidTimeRange { .. }));
    }

    #[test]
    fn test_bitemporal_close_enforces_invariant() {
        let start = time::from_secs(100);
        let interval = BiTemporalInterval::current(start);

        let invalid_end = time::from_secs(50);
        let result = interval.close_valid_time(invalid_end);

        // This MUST fail now
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TemporalError::InvalidTimeRange { .. }));
    }
}
