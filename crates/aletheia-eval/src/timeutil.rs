//! Time-anchor parsing for dataset valid-time strings (Issue #3366).
//!
//! Dataset anchors are human-readable: either full RFC 3339
//! (`2019-06-01T00:00:00Z`) or a bare calendar date (`2019-06-01`, interpreted
//! as midnight UTC). Both parse to an [`aletheiadb::Timestamp`] at second
//! granularity, which is ample for the year-scale temporal questions the
//! bundled datasets pose.

use aletheiadb::Timestamp;
use aletheiadb::time;
use chrono::{NaiveDate, TimeZone, Utc};

/// Parse an RFC 3339 timestamp or a bare `YYYY-MM-DD` date into a
/// [`Timestamp`]. Returns a descriptive error string on failure.
pub fn parse_anchor(s: &str) -> Result<Timestamp, String> {
    // Full RFC 3339 first.
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(time::from_secs(dt.timestamp()));
    }
    // Bare calendar date → midnight UTC.
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let dt = Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("valid midnight"));
        return Ok(time::from_secs(dt.timestamp()));
    }
    Err(format!(
        "invalid time anchor '{s}': expected RFC 3339 (e.g. 2019-06-01T00:00:00Z) or YYYY-MM-DD"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aletheiadb::time;

    #[test]
    fn parses_bare_date() {
        let ts = parse_anchor("2019-06-01").unwrap();
        // 2019-06-01T00:00:00Z = 1559347200 seconds since epoch.
        assert_eq!(time::to_secs(ts), 1_559_347_200);
    }

    #[test]
    fn parses_rfc3339() {
        let ts = parse_anchor("2019-06-01T00:00:00Z").unwrap();
        assert_eq!(time::to_secs(ts), 1_559_347_200);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_anchor("not-a-date").is_err());
    }

    #[test]
    fn ordering_reflects_chronology() {
        let a = parse_anchor("2019-01-01").unwrap();
        let b = parse_anchor("2023-01-01").unwrap();
        assert!(time::to_secs(a) < time::to_secs(b));
    }
}
