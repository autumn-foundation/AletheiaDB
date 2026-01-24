//! SQL:2011 Temporal Clause Handling.
//!
//! This module handles parsing and conversion of SQL:2011 temporal clauses:
//! - `FOR SYSTEM_TIME AS OF timestamp` - Point-in-time transaction time query
//! - `FOR SYSTEM_TIME BETWEEN t1 AND t2` - Transaction time range query
//! - `FOR VALID_TIME AS OF timestamp` - Point-in-time valid time query
//! - Combined temporal specifications

use crate::core::temporal::{TimeRange, Timestamp};

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
    /// Currently supports:
    /// - Unix microseconds: `1705315200000000`
    ///
    /// Support for ISO 8601 (`2024-01-15T10:00:00Z`) and SQL timestamp
    /// (`2024-01-15 10:00:00`) formats is planned for a future update.
    pub fn parse_timestamp(s: &str) -> Result<Timestamp, SqlError> {
        let trimmed = s.trim().trim_matches('\'').trim_matches('"');

        // Try parsing as microseconds
        if let Ok(micros) = trimmed.parse::<i64>() {
            return Ok(Timestamp::from(micros));
        }

        // Try parsing as ISO 8601 / SQL timestamp format
        // For now, we'll require microseconds format and add ISO 8601 support later
        Err(SqlError::InvalidTimestamp(format!(
            "Cannot parse timestamp '{}'. Use microseconds since epoch.",
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
