//! SQL:2011 Temporal Clause Handling.
//!
//! This module handles parsing and conversion of SQL:2011 temporal clauses:
//! - `FOR SYSTEM_TIME AS OF timestamp` - Point-in-time transaction time query
//! - `FOR SYSTEM_TIME BETWEEN t1 AND t2` - Transaction time range query
//! - `FOR VALID_TIME AS OF timestamp` - Point-in-time valid time query
//! - Combined temporal specifications

use crate::core::temporal::{TimeRange, Timestamp};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, Utc};

use super::error::SqlError;

/// Represents a parsed SQL:2011 temporal clause.
#[derive(Debug, Clone, PartialEq)]
pub enum TemporalClause {
    /// Point-in-time query for system (transaction) time.
    /// `FOR SYSTEM_TIME AS OF timestamp`
    SystemTimeAsOf(Timestamp),

    /// Time range query for system (transaction) time.
    /// `FOR SYSTEM_TIME BETWEEN t1 AND t2`
    SystemTimeBetween(TimeRange),

    /// Point-in-time query for valid (application) time.
    /// `FOR VALID_TIME AS OF timestamp`
    ValidTimeAsOf(Timestamp),

    /// Time range query for valid (application) time.
    /// `FOR VALID_TIME BETWEEN t1 AND t2`
    ValidTimeBetween(TimeRange),

    /// Combined bi-temporal query.
    BiTemporal {
        /// System time specification
        system_time: Box<TemporalClause>,
        /// Valid time specification
        valid_time: Box<TemporalClause>,
    },
}

impl TemporalClause {
    /// Parse a timestamp string.
    ///
    /// Supports the following formats:
    /// - **Unix microseconds**: `1705315200000000` (fast path)
    /// - **ISO 8601 UTC**: `2024-01-15T10:00:00Z`
    /// - **ISO 8601 with offset**: `2024-01-15T15:30:00+05:30`
    /// - **SQL timestamp**: `2024-01-15 10:00:00` (assumes UTC)
    /// - **Date only**: `2024-01-15` (assumes midnight UTC)
    ///
    /// All formats support optional single or double quotes and whitespace.
    ///
    /// # Examples
    ///
    /// ```
    /// use gallifreydb::sql::temporal::TemporalClause;
    ///
    /// // Unix microseconds
    /// let ts = TemporalClause::parse_timestamp("1705315200000000").unwrap();
    ///
    /// // ISO 8601 UTC
    /// let ts = TemporalClause::parse_timestamp("2024-01-15T10:00:00Z").unwrap();
    ///
    /// // ISO 8601 with timezone
    /// let ts = TemporalClause::parse_timestamp("2024-01-15T15:30:00+05:30").unwrap();
    ///
    /// // SQL timestamp format
    /// let ts = TemporalClause::parse_timestamp("2024-01-15 10:00:00").unwrap();
    ///
    /// // Date only
    /// let ts = TemporalClause::parse_timestamp("2024-01-15").unwrap();
    /// ```
    pub fn parse_timestamp(s: &str) -> Result<Timestamp, SqlError> {
        let trimmed = s.trim().trim_matches('\'').trim_matches('"');

        // Fast path: Try parsing as Unix microseconds first
        if let Ok(micros) = trimmed.parse::<i64>() {
            return Ok(Timestamp::from(micros));
        }

        // Try parsing as ISO 8601 with UTC timezone (e.g., "2024-01-15T10:00:00Z")
        if let Ok(dt) = trimmed.parse::<DateTime<Utc>>() {
            return Ok(Timestamp::from(dt.timestamp_micros()));
        }

        // Try parsing as ISO 8601 with timezone offset (e.g., "2024-01-15T15:30:00+05:30")
        if let Ok(dt) = trimmed.parse::<DateTime<FixedOffset>>() {
            return Ok(Timestamp::from(dt.with_timezone(&Utc).timestamp_micros()));
        }

        // Try parsing as SQL timestamp format (e.g., "2024-01-15 10:00:00")
        // Assume UTC timezone
        if let Ok(dt) = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S") {
            return Ok(Timestamp::from(dt.and_utc().timestamp_micros()));
        }

        // Try parsing as date-only format (e.g., "2024-01-15")
        // Assume midnight UTC
        if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
            // Use and_hms_opt to safely construct a time
            if let Some(dt) = date.and_hms_opt(0, 0, 0) {
                return Ok(Timestamp::from(dt.and_utc().timestamp_micros()));
            }
        }

        // None of the formats worked, return detailed error
        Err(SqlError::InvalidTimestamp(format!(
            "Cannot parse timestamp '{}'. Supported formats:\n\
             - Unix microseconds: 1705315200000000\n\
             - ISO 8601 UTC: 2024-01-15T10:00:00Z\n\
             - ISO 8601 with offset: 2024-01-15T15:30:00+05:30\n\
             - SQL timestamp: 2024-01-15 10:00:00 (assumes UTC)\n\
             - Date only: 2024-01-15 (assumes midnight UTC)",
            s
        )))
    }

    /// Create a SYSTEM_TIME AS OF clause.
    pub fn system_time_as_of(timestamp: Timestamp) -> Self {
        TemporalClause::SystemTimeAsOf(timestamp)
    }

    /// Create a VALID_TIME AS OF clause.
    pub fn valid_time_as_of(timestamp: Timestamp) -> Self {
        TemporalClause::ValidTimeAsOf(timestamp)
    }

    /// Create a SYSTEM_TIME BETWEEN clause.
    pub fn system_time_between(start: Timestamp, end: Timestamp) -> Result<Self, SqlError> {
        let range = TimeRange::new(start, end)
            .map_err(|e| SqlError::InvalidTemporalClause(format!("Invalid time range: {}", e)))?;
        Ok(TemporalClause::SystemTimeBetween(range))
    }

    /// Create a VALID_TIME BETWEEN clause.
    pub fn valid_time_between(start: Timestamp, end: Timestamp) -> Result<Self, SqlError> {
        let range = TimeRange::new(start, end)
            .map_err(|e| SqlError::InvalidTemporalClause(format!("Invalid time range: {}", e)))?;
        Ok(TemporalClause::ValidTimeBetween(range))
    }
}
