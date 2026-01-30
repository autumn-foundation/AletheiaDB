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
//! - Combined bi-temporal AS OF queries (both SYSTEM_TIME and VALID_TIME)
//!
//! # Architecture: Token-Based Parsing
//!
//! ## Design Rationale
//!
//! The temporal parser uses a **token-based approach** rather than regex or string
//! slicing for the following critical reasons:
//!
//! ### 1. UTF-8 Safety
//!
//! Hard-coded byte offsets (e.g., `&sql[15..]`) can panic on UTF-8 character boundaries.
//! Using `char_indices()` ensures we always slice at valid UTF-8 boundaries.
//!
//! ```rust,ignore
//! // UNSAFE - panics on multi-byte chars:
//! let after_for = &sql[start_pos + for_pattern.len()..];
//!
//! // SAFE - respects UTF-8 boundaries:
//! let tokens: Vec<(usize, Token)> = tokenize_temporal_keywords(sql);
//! ```
//!
//! ### 2. String Literal Handling
//!
//! Pattern matching on raw SQL text can incorrectly match temporal keywords
//! inside string literals:
//!
//! ```sql
//! -- This should NOT extract temporal clause:
//! SELECT * FROM nodes WHERE description = 'FOR SYSTEM_TIME AS OF TIMESTAMP'
//! ```
//!
//! The tokenizer skips string literals during keyword matching, ensuring
//! only SQL code is parsed.
//!
//! ### 3. Whitespace Tolerance
//!
//! Token-based parsing naturally handles any whitespace variation:
//!
//! ```sql
//! -- All of these work identically:
//! FOR SYSTEM_TIME AS OF TIMESTAMP '1000'
//! FOR   SYSTEM_TIME   AS OF   TIMESTAMP   '1000'
//! FOR
//!   SYSTEM_TIME
//!   AS OF TIMESTAMP '1000'
//! ```
//!
//! ## Algorithm Overview
//!
//! ```text
//! SQL String
//!     │
//!     ├─> [tokenize_temporal_keywords]
//!     │       │
//!     │       ├─> Skip string literals ('...')
//!     │       ├─> Recognize keywords (FOR, SYSTEM_TIME, AS OF, etc.)
//!     │       ├─> Extract quoted timestamp values
//!     │       └─> Return Vec<(byte_offset, Token)>
//!     │
//!     ├─> [extract_temporal_clause_from_tokens]
//!     │       │
//!     │       ├─> Pattern match token sequences
//!     │       │   - FOR SYSTEM_TIME AS OF TIMESTAMP <value>
//!     │       │   - FOR SYSTEM_TIME BETWEEN TIMESTAMP <v1> AND TIMESTAMP <v2>
//!     │       ├─> Parse timestamp strings
//!     │       └─> Return (TemporalClause, start_byte, end_byte)
//!     │
//!     ├─> [extract_temporal_clauses]
//!     │       │
//!     │       ├─> Extract SYSTEM_TIME clause
//!     │       ├─> Extract VALID_TIME clause
//!     │       ├─> Remove clauses from SQL using byte ranges
//!     │       └─> Return ExtractedTemporal
//!     │
//!     └─> [to_temporal_context]
//!             │
//!             ├─> Match on (system_time, valid_time) combinations
//!             ├─> Validate supported combinations
//!             ├─> Fill missing dimensions with time::now()
//!             └─> Return Result<Option<TemporalContext>>
//! ```
//!
//! ## Token Types
//!
//! The parser recognizes these token types:
//!
//! - **Keywords**: `FOR`, `SYSTEM_TIME`, `VALID_TIME`, `AS OF`, `BETWEEN`, `AND`, `TIMESTAMP`
//! - **Quoted Values**: `'1000000'` → `Token::QuotedValue("1000000")`
//! - **Other**: Any unrecognized token (ignored during temporal parsing)
//!
//! ## Error Handling
//!
//! The parser returns explicit errors for:
//!
//! - **Unsupported combinations**: Mixed AS OF + BETWEEN queries
//! - **Invalid timestamps**: Non-numeric or malformed timestamp strings
//! - **Invalid ranges**: BETWEEN with end < start
//!
//! ## Performance Characteristics
//!
//! - **Time Complexity**: O(n) where n = SQL string length
//! - **Space Complexity**: O(k) where k = number of tokens (typically << n)
//! - **Overhead**: ~10-20µs for typical temporal queries (negligible vs query execution)
//!
//! ## Future Enhancements
//!
//! Potential improvements tracked in roadmap:
//!
//! - [ ] Support SQL comments (skip `--` and `/* */`)
//! - [ ] Support ISO 8601 timestamp format (`2024-01-15T10:00:00Z`)
//! - [ ] Support both BETWEEN clauses simultaneously
//! - [ ] Optimize token allocation with arena/bump allocator

use super::error::SqlError;
use super::temporal::TemporalClause;
use crate::core::temporal::time;
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
    ///
    /// # Errors
    ///
    /// Returns `SqlError::UnsupportedFeature` for unsupported bi-temporal combinations
    /// like mixing AS OF with BETWEEN clauses.
    pub fn to_temporal_context(&self) -> Result<Option<TemporalContext>, SqlError> {
        match (&self.system_time, &self.valid_time) {
            (None, None) => Ok(None),

            // System time only: Query transaction time with current valid time
            (Some(TemporalClause::SystemTimeAsOf(ts)), None) => {
                Ok(Some(TemporalContext::as_of(time::now(), *ts)))
            }
            (Some(TemporalClause::SystemTimeBetween(range)), None) => {
                Ok(Some(TemporalContext::transaction_time_between(*range)))
            }

            // Valid time only: Query valid time with current transaction time
            (None, Some(TemporalClause::ValidTimeAsOf(ts))) => {
                Ok(Some(TemporalContext::as_of(*ts, time::now())))
            }
            (None, Some(TemporalClause::ValidTimeBetween(range))) => {
                Ok(Some(TemporalContext::valid_time_between(*range)))
            }

            // Bi-temporal: both system and valid time AS OF (fully supported)
            (
                Some(TemporalClause::SystemTimeAsOf(tx_ts)),
                Some(TemporalClause::ValidTimeAsOf(vt_ts)),
            ) => Ok(Some(TemporalContext::as_of(*vt_ts, *tx_ts))),
            (
                Some(TemporalClause::ValidTimeAsOf(vt_ts)),
                Some(TemporalClause::SystemTimeAsOf(tx_ts)),
            ) => {
                // Support either order
                Ok(Some(TemporalContext::as_of(*vt_ts, *tx_ts)))
            }

            // Unsupported: Mixing AS OF with BETWEEN or both BETWEEN
            (
                Some(TemporalClause::SystemTimeBetween(_)),
                Some(TemporalClause::ValidTimeAsOf(_)),
            )
            | (
                Some(TemporalClause::SystemTimeAsOf(_)),
                Some(TemporalClause::ValidTimeBetween(_)),
            )
            | (
                Some(TemporalClause::ValidTimeBetween(_)),
                Some(TemporalClause::SystemTimeAsOf(_)),
            )
            | (
                Some(TemporalClause::ValidTimeAsOf(_)),
                Some(TemporalClause::SystemTimeBetween(_)),
            )
            | (
                Some(TemporalClause::SystemTimeBetween(_)),
                Some(TemporalClause::ValidTimeBetween(_)),
            )
            | (
                Some(TemporalClause::ValidTimeBetween(_)),
                Some(TemporalClause::SystemTimeBetween(_)),
            ) => Err(SqlError::UnsupportedFeature(
                "Bi-temporal queries mixing AS OF and BETWEEN clauses are not yet supported"
                    .to_string(),
            )),

            // BiTemporal variant (not produced by current parser, but included for exhaustiveness)
            (Some(TemporalClause::BiTemporal { .. }), _)
            | (_, Some(TemporalClause::BiTemporal { .. })) => Err(SqlError::UnsupportedFeature(
                "BiTemporal clause variant is not supported in this context".to_string(),
            )),

            // Catch-all for any invalid or impossible combinations
            // (e.g., SYSTEM_TIME clauses in valid_time field)
            _ => Err(SqlError::InvalidTemporalClause(
                "Invalid temporal clause combination".to_string(),
            )),
        }
    }
}

/// Token types for safer SQL parsing
#[derive(Debug, PartialEq)]
enum Token {
    For,
    SystemTime,
    ValidTime,
    AsOf,
    Between,
    And,
    Timestamp,
    QuotedValue(String),
    Other(String),
}

/// Simple tokenizer for temporal clause extraction.
///
/// This skips string literals and only tokenizes SQL keywords and quoted values.
fn tokenize_temporal_keywords(sql: &str) -> Vec<(usize, Token)> {
    let mut tokens = Vec::new();
    let chars: Vec<(usize, char)> = sql.char_indices().collect();
    let mut i = 0;

    while i < chars.len() {
        let (byte_idx, ch) = chars[i];

        // Skip whitespace
        if ch.is_whitespace() {
            i += 1;
            continue;
        }

        // Handle single-quoted strings (SQL string literals)
        if ch == '\'' {
            i += 1;
            let mut value = String::new();
            while i < chars.len() {
                let (_, current_ch) = chars[i];
                if current_ch == '\'' {
                    // Check for escaped quote ''
                    if i + 1 < chars.len() && chars[i + 1].1 == '\'' {
                        value.push('\'');
                        i += 2;
                    } else {
                        // End of string
                        break;
                    }
                } else {
                    value.push(current_ch);
                    i += 1;
                }
            }
            tokens.push((byte_idx, Token::QuotedValue(value)));
            i += 1; // Skip closing quote
            continue;
        }

        // Collect alphanumeric/underscore sequences
        if ch.is_alphanumeric() || ch == '_' {
            let start = i;
            while i < chars.len() && (chars[i].1.is_alphanumeric() || chars[i].1 == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().map(|(_, c)| c).collect();
            let token = match word.to_uppercase().as_str() {
                "FOR" => Token::For,
                "SYSTEM_TIME" => Token::SystemTime,
                "VALID_TIME" => Token::ValidTime,
                "AS" => {
                    // Check if next token is "OF"
                    let mut j = i;
                    while j < chars.len() && chars[j].1.is_whitespace() {
                        j += 1;
                    }
                    if j < chars.len() {
                        let mut k = j;
                        while k < chars.len() && (chars[k].1.is_alphanumeric() || chars[k].1 == '_')
                        {
                            k += 1;
                        }
                        let next_word: String = chars[j..k].iter().map(|(_, c)| c).collect();
                        if next_word.to_uppercase() == "OF" {
                            i = k; // Skip "OF"
                            Token::AsOf
                        } else {
                            Token::Other(word)
                        }
                    } else {
                        Token::Other(word)
                    }
                }
                "BETWEEN" => Token::Between,
                "AND" => Token::And,
                "TIMESTAMP" => Token::Timestamp,
                _ => Token::Other(word),
            };
            tokens.push((byte_idx, token));
            continue;
        }

        // Skip other characters
        i += 1;
    }

    tokens
}

/// Extract temporal clause from tokenized SQL.
///
/// Returns (clause, start_byte, end_byte) if found, None otherwise.
///
/// # Errors
///
/// Returns explicit errors for malformed temporal clauses:
/// - Missing TIMESTAMP keyword or quoted value
/// - Incomplete BETWEEN clause (missing AND or second timestamp)
fn extract_temporal_clause_from_tokens(
    tokens: &[(usize, Token)],
    time_type_token: Token,
) -> Result<Option<(TemporalClause, usize, usize)>, SqlError> {
    // Find "FOR SYSTEM_TIME" or "FOR VALID_TIME"
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].1 == Token::For && i + 1 < tokens.len() && tokens[i + 1].1 == time_type_token {
            let start_byte = tokens[i].0;
            let time_name = match time_type_token {
                Token::SystemTime => "SYSTEM_TIME",
                Token::ValidTime => "VALID_TIME",
                _ => unreachable!(),
            };

            // Check for AS OF or BETWEEN
            if i + 2 < tokens.len() && tokens[i + 2].1 == Token::AsOf {
                // AS OF case - require TIMESTAMP keyword
                if i + 3 >= tokens.len() {
                    return Err(SqlError::InvalidTemporalClause(format!(
                        "FOR {} AS OF requires TIMESTAMP keyword",
                        time_name
                    )));
                }

                if tokens[i + 3].1 != Token::Timestamp {
                    return Err(SqlError::InvalidTemporalClause(format!(
                        "FOR {} AS OF requires TIMESTAMP keyword, found {:?}",
                        time_name,
                        tokens[i + 3].1
                    )));
                }

                // Require quoted timestamp value
                if i + 4 >= tokens.len() {
                    return Err(SqlError::InvalidTemporalClause(format!(
                        "FOR {} AS OF TIMESTAMP requires a quoted timestamp value",
                        time_name
                    )));
                }

                if let Token::QuotedValue(ref ts_str) = tokens[i + 4].1 {
                    let ts = TemporalClause::parse_timestamp(ts_str)?;

                    // Calculate end byte: look for next token after this temporal clause
                    // to include trailing whitespace in the removal range
                    let end_byte = if i + 5 < tokens.len() {
                        // Use next token's start position
                        tokens[i + 5].0
                    } else {
                        // No more tokens, use position after closing quote
                        tokens[i + 4].0 + ts_str.len() + 2 // +2 for quotes
                    };

                    let clause = match time_type_token {
                        Token::SystemTime => TemporalClause::SystemTimeAsOf(ts),
                        Token::ValidTime => TemporalClause::ValidTimeAsOf(ts),
                        _ => unreachable!(),
                    };

                    return Ok(Some((clause, start_byte, end_byte)));
                } else {
                    return Err(SqlError::InvalidTemporalClause(format!(
                        "FOR {} AS OF TIMESTAMP requires a quoted timestamp value, found {:?}",
                        time_name,
                        tokens[i + 4].1
                    )));
                }
            } else if i + 2 < tokens.len() && tokens[i + 2].1 == Token::Between {
                // BETWEEN case - require first TIMESTAMP
                if i + 3 >= tokens.len() {
                    return Err(SqlError::InvalidTemporalClause(format!(
                        "FOR {} BETWEEN requires TIMESTAMP keyword",
                        time_name
                    )));
                }

                if tokens[i + 3].1 != Token::Timestamp {
                    return Err(SqlError::InvalidTemporalClause(format!(
                        "FOR {} BETWEEN requires TIMESTAMP keyword, found {:?}",
                        time_name,
                        tokens[i + 3].1
                    )));
                }

                // Require first timestamp value
                if i + 4 >= tokens.len() {
                    return Err(SqlError::InvalidTemporalClause(format!(
                        "FOR {} BETWEEN TIMESTAMP requires a quoted timestamp value",
                        time_name
                    )));
                }

                if let Token::QuotedValue(ref start_str) = tokens[i + 4].1 {
                    // Require AND keyword
                    if i + 5 >= tokens.len() {
                        return Err(SqlError::InvalidTemporalClause(format!(
                            "FOR {} BETWEEN requires AND keyword",
                            time_name
                        )));
                    }

                    if tokens[i + 5].1 != Token::And {
                        return Err(SqlError::InvalidTemporalClause(format!(
                            "FOR {} BETWEEN requires AND keyword, found {:?}",
                            time_name,
                            tokens[i + 5].1
                        )));
                    }

                    // Require second TIMESTAMP keyword
                    if i + 6 >= tokens.len() {
                        return Err(SqlError::InvalidTemporalClause(format!(
                            "FOR {} BETWEEN ... AND requires TIMESTAMP keyword",
                            time_name
                        )));
                    }

                    if tokens[i + 6].1 != Token::Timestamp {
                        return Err(SqlError::InvalidTemporalClause(format!(
                            "FOR {} BETWEEN ... AND requires TIMESTAMP keyword, found {:?}",
                            time_name,
                            tokens[i + 6].1
                        )));
                    }

                    // Require second timestamp value
                    if i + 7 >= tokens.len() {
                        return Err(SqlError::InvalidTemporalClause(format!(
                            "FOR {} BETWEEN ... AND TIMESTAMP requires a quoted timestamp value",
                            time_name
                        )));
                    }

                    if let Token::QuotedValue(ref end_str) = tokens[i + 7].1 {
                        let start_ts = TemporalClause::parse_timestamp(start_str)?;
                        let end_ts = TemporalClause::parse_timestamp(end_str)?;

                        // Calculate end byte: use next token's position or end of last token
                        let end_byte = if i + 8 < tokens.len() {
                            tokens[i + 8].0
                        } else {
                            tokens[i + 7].0 + end_str.len() + 2 // +2 for quotes
                        };

                        let clause = match time_type_token {
                            Token::SystemTime => {
                                TemporalClause::system_time_between(start_ts, end_ts)?
                            }
                            Token::ValidTime => {
                                TemporalClause::valid_time_between(start_ts, end_ts)?
                            }
                            _ => unreachable!(),
                        };

                        return Ok(Some((clause, start_byte, end_byte)));
                    } else {
                        return Err(SqlError::InvalidTemporalClause(format!(
                            "FOR {} BETWEEN ... AND TIMESTAMP requires a quoted timestamp value, found {:?}",
                            time_name,
                            tokens[i + 7].1
                        )));
                    }
                } else {
                    return Err(SqlError::InvalidTemporalClause(format!(
                        "FOR {} BETWEEN TIMESTAMP requires a quoted timestamp value, found {:?}",
                        time_name,
                        tokens[i + 4].1
                    )));
                }
            } else if i + 2 < tokens.len() {
                // Found FOR SYSTEM_TIME/VALID_TIME but not followed by AS OF or BETWEEN
                return Err(SqlError::InvalidTemporalClause(format!(
                    "FOR {} must be followed by AS OF or BETWEEN, found {:?}",
                    time_name,
                    tokens[i + 2].1
                )));
            } else {
                // Found FOR SYSTEM_TIME/VALID_TIME at end of tokens
                return Err(SqlError::InvalidTemporalClause(format!(
                    "FOR {} must be followed by AS OF or BETWEEN",
                    time_name
                )));
            }
        }
        i += 1;
    }

    Ok(None)
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
    let tokens = tokenize_temporal_keywords(sql);
    let mut cleaned = sql.to_string();
    let mut system_time: Option<TemporalClause> = None;
    let mut valid_time: Option<TemporalClause> = None;

    // Track byte ranges to remove (in reverse order to avoid offset issues)
    let mut removals: Vec<(usize, usize)> = Vec::new();

    // Extract SYSTEM_TIME clause
    if let Some((clause, start, end)) =
        extract_temporal_clause_from_tokens(&tokens, Token::SystemTime)?
    {
        system_time = Some(clause);
        removals.push((start, end));
    }

    // Extract VALID_TIME clause
    if let Some((clause, start, end)) =
        extract_temporal_clause_from_tokens(&tokens, Token::ValidTime)?
    {
        valid_time = Some(clause);
        removals.push((start, end));
    }

    // Sort removals by start position (descending) to remove from end to start
    removals.sort_by(|a, b| b.0.cmp(&a.0));

    // Remove temporal clauses from SQL
    for (start, end) in removals {
        cleaned.replace_range(start..end, "");
    }

    // Clean up extra whitespace
    let new_cleaned = {
        let mut parts = cleaned.split_whitespace();
        if let Some(first) = parts.next() {
            // Reserve approximate capacity to avoid reallocations
            let mut s = String::with_capacity(cleaned.len());
            s.push_str(first);
            for part in parts {
                s.push(' ');
                s.push_str(part);
            }
            s
        } else {
            String::new()
        }
    };
    cleaned = new_cleaned;

    Ok(ExtractedTemporal {
        cleaned_sql: cleaned,
        system_time,
        valid_time,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::temporal::Timestamp;

    #[test]
    fn test_tokenize_keywords() {
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '1000'";
        let tokens = tokenize_temporal_keywords(sql);

        assert!(tokens.iter().any(|(_, t)| t == &Token::For));
        assert!(tokens.iter().any(|(_, t)| t == &Token::SystemTime));
        assert!(tokens.iter().any(|(_, t)| t == &Token::AsOf));
        assert!(tokens.iter().any(|(_, t)| t == &Token::Timestamp));
        assert!(
            tokens
                .iter()
                .any(|(_, t)| matches!(t, Token::QuotedValue(v) if v == "1000"))
        );
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
    fn test_extract_with_extra_whitespace() {
        let sql = "SELECT * FROM nodes   FOR   SYSTEM_TIME   AS OF   TIMESTAMP  '1000'";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes");
        assert!(result.system_time.is_some());
    }

    #[test]
    fn test_to_temporal_context_system_time_only() {
        let extracted = ExtractedTemporal {
            cleaned_sql: "SELECT * FROM nodes".to_string(),
            system_time: Some(TemporalClause::SystemTimeAsOf(Timestamp::from(1000))),
            valid_time: None,
        };

        let ctx = extracted.to_temporal_context().unwrap();
        assert!(ctx.is_some());
        let ctx = ctx.unwrap();
        assert!(ctx.as_of_tuple().is_some());
    }

    #[test]
    fn test_to_temporal_context_bitemporal() {
        let extracted = ExtractedTemporal {
            cleaned_sql: "SELECT * FROM nodes".to_string(),
            system_time: Some(TemporalClause::SystemTimeAsOf(Timestamp::from(2000))),
            valid_time: Some(TemporalClause::ValidTimeAsOf(Timestamp::from(1500))),
        };

        let ctx = extracted.to_temporal_context().unwrap();
        assert!(ctx.is_some());
        let ctx = ctx.unwrap();
        assert!(ctx.as_of_tuple().is_some());

        let (vt, tt) = ctx.as_of_tuple().unwrap();
        assert_eq!(vt.wallclock(), 1500);
        assert_eq!(tt.wallclock(), 2000);
    }

    #[test]
    fn test_unsupported_mixed_between_and_as_of() {
        let extracted = ExtractedTemporal {
            cleaned_sql: "SELECT * FROM nodes".to_string(),
            system_time: Some(TemporalClause::SystemTimeBetween(
                crate::core::temporal::TimeRange::new(Timestamp::from(1000), Timestamp::from(2000))
                    .unwrap(),
            )),
            valid_time: Some(TemporalClause::ValidTimeAsOf(Timestamp::from(1500))),
        };

        let result = extracted.to_temporal_context();
        assert!(result.is_err());
        assert!(matches!(result, Err(SqlError::UnsupportedFeature(_))));
    }

    // ========================================================================
    // Edge Case Tests
    // ========================================================================

    #[test]
    fn test_string_literal_with_temporal_keywords() {
        // Temporal keywords inside string literals should NOT be extracted
        let sql = "SELECT * FROM nodes WHERE description = 'FOR SYSTEM_TIME AS OF TIMESTAMP'";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(
            result.cleaned_sql,
            "SELECT * FROM nodes WHERE description = 'FOR SYSTEM_TIME AS OF TIMESTAMP'"
        );
        assert!(result.system_time.is_none());
        assert!(result.valid_time.is_none());
    }

    #[test]
    fn test_escaped_quotes_in_string_literals() {
        // SQL with escaped quotes should be handled correctly
        let sql =
            "SELECT * FROM nodes WHERE name = 'O''Brien' FOR SYSTEM_TIME AS OF TIMESTAMP '1000'";
        let result = extract_temporal_clauses(sql).unwrap();

        // Should extract temporal clause but preserve escaped quotes
        assert!(result.cleaned_sql.contains("O''Brien"));
        assert!(result.system_time.is_some());
    }

    #[test]
    fn test_multiple_spaces_and_newlines() {
        // Parser should handle various whitespace patterns
        let sql = "SELECT   *   FROM   nodes\n  FOR\n  SYSTEM_TIME\n  AS OF\n  TIMESTAMP\n  '1000'\n  WHERE age > 21";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes WHERE age > 21");
        assert!(result.system_time.is_some());
    }

    #[test]
    fn test_tabs_in_temporal_clause() {
        // Should handle tabs as whitespace
        let sql = "SELECT * FROM nodes FOR\tSYSTEM_TIME\tAS OF\tTIMESTAMP\t'1000'";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes");
        assert!(result.system_time.is_some());
    }

    #[test]
    fn test_temporal_clause_at_end_of_query() {
        // Temporal clause at the very end (no trailing WHERE/ORDER BY)
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '1000'";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes");
        assert!(result.system_time.is_some());
    }

    #[test]
    fn test_both_between_clauses_unsupported() {
        // Both SYSTEM_TIME BETWEEN and VALID_TIME BETWEEN should error
        let extracted = ExtractedTemporal {
            cleaned_sql: "SELECT * FROM nodes".to_string(),
            system_time: Some(TemporalClause::SystemTimeBetween(
                crate::core::temporal::TimeRange::new(Timestamp::from(1000), Timestamp::from(2000))
                    .unwrap(),
            )),
            valid_time: Some(TemporalClause::ValidTimeBetween(
                crate::core::temporal::TimeRange::new(Timestamp::from(1500), Timestamp::from(2500))
                    .unwrap(),
            )),
        };

        let result = extracted.to_temporal_context();
        assert!(result.is_err());
        assert!(matches!(result, Err(SqlError::UnsupportedFeature(_))));
    }

    #[test]
    fn test_timestamp_with_leading_zeros() {
        // Timestamps with leading zeros should parse correctly
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '0000001000'";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes");
        assert!(result.system_time.is_some());
        if let Some(TemporalClause::SystemTimeAsOf(ts)) = result.system_time {
            assert_eq!(ts.wallclock(), 1000);
        } else {
            panic!("Expected SystemTimeAsOf");
        }
    }

    #[test]
    fn test_empty_query_string() {
        // Empty string should return empty result
        let result = extract_temporal_clauses("").unwrap();

        assert_eq!(result.cleaned_sql, "");
        assert!(result.system_time.is_none());
        assert!(result.valid_time.is_none());
    }

    #[test]
    fn test_only_temporal_clause() {
        // Query with only temporal clause (no SELECT)
        let sql = "FOR SYSTEM_TIME AS OF TIMESTAMP '1000'";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "");
        assert!(result.system_time.is_some());
    }

    #[test]
    fn test_case_insensitive_keywords() {
        // Keywords in different cases should work
        let sql = "SELECT * FROM nodes for system_time as of timestamp '1000'";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes");
        assert!(result.system_time.is_some());
    }

    #[test]
    fn test_mixed_case_keywords() {
        // Mixed case keywords
        let sql = "SELECT * FROM nodes FoR SyStEm_TiMe As Of TiMeStAmP '1000'";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes");
        assert!(result.system_time.is_some());
    }

    #[test]
    fn test_temporal_context_with_now() {
        // Test that time::now() is used correctly
        let extracted = ExtractedTemporal {
            cleaned_sql: "SELECT * FROM nodes".to_string(),
            system_time: Some(TemporalClause::SystemTimeAsOf(Timestamp::from(1000))),
            valid_time: None,
        };

        let ctx = extracted.to_temporal_context().unwrap();
        assert!(ctx.is_some());

        // Valid time should be set to "now" (non-zero)
        let ctx = ctx.unwrap();
        let (vt, tt) = ctx.as_of_tuple().unwrap();
        assert_eq!(tt.wallclock(), 1000); // System time from clause
        assert!(vt.wallclock() > 0); // Valid time is "now"
    }

    // ========================================================================
    // Malformed Input Tests (Error Handling)
    // ========================================================================

    #[test]
    fn test_error_missing_timestamp_keyword() {
        // FOR SYSTEM_TIME AS OF without TIMESTAMP keyword
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME AS OF '1000'";
        let result = extract_temporal_clauses(sql);
        assert!(result.is_err());
        assert!(matches!(result, Err(SqlError::InvalidTemporalClause(_))));
    }

    #[test]
    fn test_error_missing_timestamp_value() {
        // FOR SYSTEM_TIME AS OF TIMESTAMP without quoted value
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP";
        let result = extract_temporal_clauses(sql);
        assert!(result.is_err());
        assert!(matches!(result, Err(SqlError::InvalidTemporalClause(_))));
        if let Err(SqlError::InvalidTemporalClause(msg)) = result {
            assert!(msg.contains("requires a quoted timestamp value"));
        }
    }

    #[test]
    fn test_error_between_missing_and() {
        // FOR SYSTEM_TIME BETWEEN without AND keyword
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME BETWEEN TIMESTAMP '1000'";
        let result = extract_temporal_clauses(sql);
        assert!(result.is_err());
        assert!(matches!(result, Err(SqlError::InvalidTemporalClause(_))));
        if let Err(SqlError::InvalidTemporalClause(msg)) = result {
            assert!(msg.contains("requires AND keyword"));
        }
    }

    #[test]
    fn test_error_between_missing_second_timestamp() {
        // FOR SYSTEM_TIME BETWEEN ... AND without second TIMESTAMP keyword
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME BETWEEN TIMESTAMP '1000' AND";
        let result = extract_temporal_clauses(sql);
        assert!(result.is_err());
        assert!(matches!(result, Err(SqlError::InvalidTemporalClause(_))));
        if let Err(SqlError::InvalidTemporalClause(msg)) = result {
            assert!(msg.contains("requires TIMESTAMP keyword"));
        }
    }

    #[test]
    fn test_error_between_missing_second_value() {
        // FOR SYSTEM_TIME BETWEEN ... AND TIMESTAMP without second quoted value
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME BETWEEN TIMESTAMP '1000' AND TIMESTAMP";
        let result = extract_temporal_clauses(sql);
        assert!(result.is_err());
        assert!(matches!(result, Err(SqlError::InvalidTemporalClause(_))));
        if let Err(SqlError::InvalidTemporalClause(msg)) = result {
            assert!(msg.contains("requires a quoted timestamp value"));
        }
    }

    #[test]
    fn test_error_for_system_time_without_as_of_or_between() {
        // FOR SYSTEM_TIME followed by unexpected token
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME WHERE age > 21";
        let result = extract_temporal_clauses(sql);
        assert!(result.is_err());
        assert!(matches!(result, Err(SqlError::InvalidTemporalClause(_))));
        if let Err(SqlError::InvalidTemporalClause(msg)) = result {
            assert!(msg.contains("must be followed by AS OF or BETWEEN"));
        }
    }

    #[test]
    fn test_error_for_valid_time_incomplete() {
        // FOR VALID_TIME at end of SQL without AS OF or BETWEEN
        let sql = "SELECT * FROM nodes FOR VALID_TIME";
        let result = extract_temporal_clauses(sql);
        assert!(result.is_err());
        assert!(matches!(result, Err(SqlError::InvalidTemporalClause(_))));
        if let Err(SqlError::InvalidTemporalClause(msg)) = result {
            assert!(msg.contains("must be followed by AS OF or BETWEEN"));
        }
    }

    #[test]
    fn test_whitespace_preserved_after_temporal_removal() {
        // Test that whitespace is correctly handled when removing temporal clauses
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '1000' WHERE age > 21";
        let result = extract_temporal_clauses(sql).unwrap();

        // Should have proper spacing (not "nodesWHERE")
        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes WHERE age > 21");
        assert!(!result.cleaned_sql.contains("nodesWHERE"));
    }

    #[test]
    fn test_multiple_temporal_clauses_whitespace() {
        // Test whitespace handling with both SYSTEM_TIME and VALID_TIME
        let sql = "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '2000' FOR VALID_TIME AS OF TIMESTAMP '1500' WHERE age > 21";
        let result = extract_temporal_clauses(sql).unwrap();

        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes WHERE age > 21");
        assert!(!result.cleaned_sql.contains("nodesWHERE"));
        assert!(result.system_time.is_some());
        assert!(result.valid_time.is_some());
    }
}
