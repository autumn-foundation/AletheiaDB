//! Temporal SQL:2011 Clause Preprocessing.
//!
//! This module handles extraction and parsing of SQL:2011 temporal clauses
//! before passing SQL to the standard sqlparser-rs, which doesn't natively
//! support temporal syntax.
//!
//! # Approach
//!
//! Since sqlparser-rs doesn't support SQL:2011 temporal extensions, we:
//! 1. Extract temporal clauses using pattern matching
//! 2. Parse them into TemporalClause enums
//! 3. Remove them from the SQL string
//! 4. Pass the cleaned SQL to sqlparser-rs
//! 5. Convert temporal clauses to TemporalContext
//!
//! # Supported Syntax
//!
//! - `FOR SYSTEM_TIME AS OF TIMESTAMP 'value'`
//! - `FOR SYSTEM_TIME BETWEEN TIMESTAMP 'start' AND TIMESTAMP 'end'`
//! - `FOR VALID_TIME AS OF TIMESTAMP 'value'`
//! - `FOR VALID_TIME BETWEEN TIMESTAMP 'start' AND TIMESTAMP 'end'`
//! - Combined bi-temporal queries (both SYSTEM_TIME and VALID_TIME)

use super::error::SqlError;
use super::temporal::TemporalClause;
use crate::core::temporal::Timestamp;
use crate::query::plan::TemporalContext;

/// Extracted temporal information from SQL.
#[derive(Debug, Clone)]
pub struct ExtractedTemporal {
    /// The SQL string with temporal clauses removed
    pub cleaned_sql: String,
    /// Extracted system time clause (if any)
    pub system_time: Option<TemporalClause>,
    /// Extracted valid time clause (if any)
    pub valid_time: Option<TemporalClause>,
}

impl ExtractedTemporal {
    /// Convert extracted temporal clauses to TemporalContext.
    pub fn to_temporal_context(&self) -> Option<TemporalContext> {
        match (&self.system_time, &self.valid_time) {
            (None, None) => None,

            // System time only (use epoch for valid time since it's not specified)
            (Some(TemporalClause::SystemTimeAsOf(ts)), None) => {
                Some(TemporalContext::as_of(Timestamp::from(0), *ts))
            }
            (Some(TemporalClause::SystemTimeBetween(range)), None) => {
                Some(TemporalContext::between(*range))
            }

            // Valid time only (use i64::MAX for transaction time to represent "current")
            (None, Some(TemporalClause::ValidTimeAsOf(ts))) => {
                Some(TemporalContext::as_of(*ts, Timestamp::from(i64::MAX)))
            }
            (None, Some(TemporalClause::ValidTimeBetween(range))) => {
                Some(TemporalContext::between(*range))
            }

            // Bi-temporal: both system and valid time
            (
                Some(TemporalClause::SystemTimeAsOf(tx_ts)),
                Some(TemporalClause::ValidTimeAsOf(vt_ts)),
            ) => Some(TemporalContext::as_of(*vt_ts, *tx_ts)),
            (
                Some(TemporalClause::ValidTimeAsOf(vt_ts)),
                Some(TemporalClause::SystemTimeAsOf(tx_ts)),
            ) => {
                // Support either order
                Some(TemporalContext::as_of(*vt_ts, *tx_ts))
            }

            // If both are BETWEEN, use system time range (valid time ranges not yet supported in TemporalContext)
            (Some(TemporalClause::SystemTimeBetween(range)), Some(_)) => {
                Some(TemporalContext::between(*range))
            }
            (Some(_), Some(TemporalClause::ValidTimeBetween(range))) => {
                Some(TemporalContext::between(*range))
            }

            // Other combinations not yet supported
            _ => None,
        }
    }
}

/// Extract a timestamp value from a TIMESTAMP 'value' pattern.
///
/// Expects input like "TIMESTAMP '1000000'" and returns "1000000".
fn extract_timestamp_value(s: &str) -> Option<&str> {
    let s = s.trim();
    let upper = s.to_uppercase();

    if !upper.starts_with("TIMESTAMP") {
        return None;
    }

    // Find the quoted value after TIMESTAMP
    let after_timestamp = s[9..].trim(); // Skip "TIMESTAMP"

    // Find opening quote
    let start = after_timestamp.find('\'')?;
    // Find closing quote
    let end = after_timestamp[start + 1..].find('\'')?;

    Some(&after_timestamp[start + 1..start + 1 + end])
}

/// Find the position of a pattern in SQL (case-insensitive).
fn find_pattern_ignore_case(sql: &str, pattern: &str) -> Option<usize> {
    let sql_upper = sql.to_uppercase();
    let pattern_upper = pattern.to_uppercase();
    sql_upper.find(&pattern_upper)
}

/// Extract temporal clause from SQL string.
///
/// Returns (clause, remaining_sql) if found, None otherwise.
fn extract_temporal_clause(
    sql: &str,
    time_type: &str, // "SYSTEM_TIME" or "VALID_TIME"
) -> Result<Option<(TemporalClause, String)>, SqlError> {
    let for_pattern = format!("FOR {} ", time_type);

    // Find "FOR SYSTEM_TIME" or "FOR VALID_TIME"
    let start_pos = match find_pattern_ignore_case(sql, &for_pattern) {
        Some(pos) => pos,
        None => return Ok(None),
    };

    let after_for = &sql[start_pos + for_pattern.len()..];

    // Check if it's AS OF or BETWEEN
    if find_pattern_ignore_case(after_for, "AS OF TIMESTAMP") == Some(0) {
        // Extract AS OF clause
        let after_as_of = &after_for[15..]; // Skip "AS OF TIMESTAMP"

        // Find the timestamp value
        let temp_str = format!("TIMESTAMP {}", after_as_of);
        let timestamp_str = extract_timestamp_value(&temp_str).ok_or_else(|| {
            SqlError::InvalidTemporalClause("Invalid timestamp format".to_string())
        })?;

        let ts = TemporalClause::parse_timestamp(timestamp_str)?;

        // Find the end of this clause (the closing quote)
        let quote_pos = after_as_of
            .find('\'')
            .ok_or_else(|| SqlError::InvalidTemporalClause("Missing opening quote".to_string()))?;
        let end_quote_pos = after_as_of[quote_pos + 1..]
            .find('\'')
            .ok_or_else(|| SqlError::InvalidTemporalClause("Missing closing quote".to_string()))?;
        let clause_end = start_pos + for_pattern.len() + 15 + quote_pos + 1 + end_quote_pos + 1;

        // Create the clause
        let clause = if time_type == "SYSTEM_TIME" {
            TemporalClause::SystemTimeAsOf(ts)
        } else {
            TemporalClause::ValidTimeAsOf(ts)
        };

        // Remove the clause from SQL
        let cleaned = format!("{} {}", &sql[..start_pos], &sql[clause_end..])
            .trim()
            .to_string();

        Ok(Some((clause, cleaned)))
    } else if find_pattern_ignore_case(after_for, "BETWEEN TIMESTAMP") == Some(0) {
        // Extract BETWEEN clause
        let after_between = &after_for[17..]; // Skip "BETWEEN TIMESTAMP"

        // Find first timestamp
        let temp_str = format!("TIMESTAMP {}", after_between);
        let start_timestamp_str = extract_timestamp_value(&temp_str).ok_or_else(|| {
            SqlError::InvalidTemporalClause("Invalid start timestamp format".to_string())
        })?;

        let start_ts = TemporalClause::parse_timestamp(start_timestamp_str)?;

        // Find "AND TIMESTAMP"
        let and_pos =
            find_pattern_ignore_case(after_between, "AND TIMESTAMP").ok_or_else(|| {
                SqlError::InvalidTemporalClause("BETWEEN requires AND TIMESTAMP".to_string())
            })?;

        let after_and = &after_between[and_pos + 13..]; // Skip "AND TIMESTAMP"

        // Find second timestamp
        let temp_str2 = format!("TIMESTAMP {}", after_and);
        let end_timestamp_str = extract_timestamp_value(&temp_str2).ok_or_else(|| {
            SqlError::InvalidTemporalClause("Invalid end timestamp format".to_string())
        })?;

        let end_ts = TemporalClause::parse_timestamp(end_timestamp_str)?;

        // Find the end of this clause (the closing quote of second timestamp)
        let quote_pos = after_and.find('\'').ok_or_else(|| {
            SqlError::InvalidTemporalClause("Missing opening quote in end timestamp".to_string())
        })?;
        let end_quote_pos = after_and[quote_pos + 1..].find('\'').ok_or_else(|| {
            SqlError::InvalidTemporalClause("Missing closing quote in end timestamp".to_string())
        })?;
        let clause_end =
            start_pos + for_pattern.len() + 17 + and_pos + 13 + quote_pos + 1 + end_quote_pos + 1;

        // Create the clause
        let clause = if time_type == "SYSTEM_TIME" {
            TemporalClause::system_time_between(start_ts, end_ts)?
        } else {
            TemporalClause::valid_time_between(start_ts, end_ts)?
        };

        // Remove the clause from SQL
        let cleaned = format!("{} {}", &sql[..start_pos], &sql[clause_end..])
            .trim()
            .to_string();

        Ok(Some((clause, cleaned)))
    } else {
        Err(SqlError::InvalidTemporalClause(
            "Expected AS OF or BETWEEN after FOR TIME_TYPE".to_string(),
        ))
    }
}

/// Parse and extract temporal clauses from SQL.
///
/// This function:
/// 1. Extracts temporal clauses from the SQL string
/// 2. Returns the cleaned SQL and parsed temporal clauses
///
/// # Example
///
/// ```rust,ignore
/// let result = extract_temporal_clauses(
///     "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '1000000' WHERE age > 21"
/// )?;
/// assert_eq!(result.cleaned_sql, "SELECT * FROM nodes WHERE age > 21");
/// assert!(result.system_time.is_some());
/// ```
pub fn extract_temporal_clauses(sql: &str) -> Result<ExtractedTemporal, SqlError> {
    let mut cleaned = sql.to_string();
    let mut system_time: Option<TemporalClause> = None;
    let mut valid_time: Option<TemporalClause> = None;

    // Extract SYSTEM_TIME clause
    if let Some((clause, new_sql)) = extract_temporal_clause(&cleaned, "SYSTEM_TIME")? {
        system_time = Some(clause);
        cleaned = new_sql;
    }

    // Extract VALID_TIME clause
    if let Some((clause, new_sql)) = extract_temporal_clause(&cleaned, "VALID_TIME")? {
        valid_time = Some(clause);
        cleaned = new_sql;
    }

    // Clean up extra whitespace
    cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");

    Ok(ExtractedTemporal {
        cleaned_sql: cleaned,
        system_time,
        valid_time,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_timestamp_value() {
        assert_eq!(
            extract_timestamp_value("TIMESTAMP '1000000'"),
            Some("1000000")
        );
        assert_eq!(extract_timestamp_value("timestamp '123'"), Some("123"));
        assert_eq!(extract_timestamp_value("TIMESTAMP  '456'"), Some("456"));
    }

    #[test]
    fn test_extract_system_time_as_of() {
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '1000000' WHERE age > 21";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes WHERE age > 21");
        assert!(result.system_time.is_some());
        assert!(matches!(
            result.system_time,
            Some(TemporalClause::SystemTimeAsOf(_))
        ));
    }

    #[test]
    fn test_extract_system_time_between() {
        let sql =
            "SELECT * FROM nodes FOR SYSTEM_TIME BETWEEN TIMESTAMP '1000' AND TIMESTAMP '2000'";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes");
        assert!(result.system_time.is_some());
        assert!(matches!(
            result.system_time,
            Some(TemporalClause::SystemTimeBetween(_))
        ));
    }

    #[test]
    fn test_extract_valid_time_as_of() {
        let sql = "SELECT * FROM nodes FOR VALID_TIME AS OF TIMESTAMP '1000000'";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes");
        assert!(result.valid_time.is_some());
        assert!(matches!(
            result.valid_time,
            Some(TemporalClause::ValidTimeAsOf(_))
        ));
    }

    #[test]
    fn test_extract_bitemporal() {
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '2000' FOR VALID_TIME AS OF TIMESTAMP '1500'";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes");
        assert!(result.system_time.is_some());
        assert!(result.valid_time.is_some());
    }

    #[test]
    fn test_to_temporal_context_system_time_only() {
        let extracted = ExtractedTemporal {
            cleaned_sql: "SELECT * FROM nodes".to_string(),
            system_time: Some(TemporalClause::SystemTimeAsOf(Timestamp::from(1000))),
            valid_time: None,
        };

        let ctx = extracted.to_temporal_context();
        assert!(ctx.is_some());
        let ctx = ctx.unwrap();
        assert!(ctx.as_of.is_some());
    }

    #[test]
    fn test_to_temporal_context_bitemporal() {
        let extracted = ExtractedTemporal {
            cleaned_sql: "SELECT * FROM nodes".to_string(),
            system_time: Some(TemporalClause::SystemTimeAsOf(Timestamp::from(2000))),
            valid_time: Some(TemporalClause::ValidTimeAsOf(Timestamp::from(1500))),
        };

        let ctx = extracted.to_temporal_context();
        assert!(ctx.is_some());
        let ctx = ctx.unwrap();
        assert!(ctx.as_of.is_some());

        let (vt, tt) = ctx.as_of.unwrap();
        assert_eq!(vt.wallclock(), 1500);
        assert_eq!(tt.wallclock(), 2000);
    }
}
