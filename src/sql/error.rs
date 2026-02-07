//! SQL-specific error types.

use std::fmt;

/// Errors that can occur during SQL parsing and conversion.
#[derive(Debug, Clone)]
pub enum SqlError {
    /// Error from the SQL parser (sqlparser-rs).
    ParseError(String),

    /// Unsupported SQL feature or syntax.
    UnsupportedFeature(String),

    /// Invalid table reference (not 'nodes' or 'edges').
    InvalidTable(String),

    /// Invalid column reference.
    InvalidColumn(String),

    /// Invalid temporal clause.
    InvalidTemporalClause(String),

    /// Invalid timestamp format.
    InvalidTimestamp(String),

    /// Missing required clause.
    MissingClause(String),

    /// Type conversion error.
    TypeError(String),

    /// Parameter binding error.
    ParameterError(String),
}

impl fmt::Display for SqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SqlError::ParseError(msg) => write!(f, "SQL parse error: {}", msg),
            SqlError::UnsupportedFeature(msg) => write!(f, "Unsupported SQL feature: {}", msg),
            SqlError::InvalidTable(name) => {
                write!(f, "Invalid table '{}': expected 'nodes' or 'edges'", name)
            }
            SqlError::InvalidColumn(name) => write!(f, "Invalid column reference: {}", name),
            SqlError::InvalidTemporalClause(msg) => write!(f, "Invalid temporal clause: {}", msg),
            SqlError::InvalidTimestamp(msg) => write!(f, "Invalid timestamp: {}", msg),
            SqlError::MissingClause(msg) => write!(f, "Missing required clause: {}", msg),
            SqlError::TypeError(msg) => write!(f, "Type error: {}", msg),
            SqlError::ParameterError(msg) => write!(f, "Parameter error: {}", msg),
        }
    }
}

impl std::error::Error for SqlError {}

impl From<sqlparser::parser::ParserError> for SqlError {
    fn from(err: sqlparser::parser::ParserError) -> Self {
        SqlError::ParseError(err.to_string())
    }
}
