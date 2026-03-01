//! SQL-specific error types.

use thiserror::Error;

/// Errors that can occur during SQL parsing and conversion.
#[derive(Debug, Clone, Error)]
pub enum SqlError {
    /// Error from the SQL parser (sqlparser-rs).
    #[error("SQL parse error: {0}")]
    ParseError(String),

    /// Unsupported SQL feature or syntax.
    #[error("Unsupported SQL feature: {0}")]
    UnsupportedFeature(String),

    /// Invalid table reference (not 'nodes' or 'edges').
    #[error("Invalid table '{0}': expected 'nodes' or 'edges'")]
    InvalidTable(String),

    /// Invalid column reference.
    #[error("Invalid column reference: {0}")]
    InvalidColumn(String),

    /// Invalid temporal clause.
    #[error("Invalid temporal clause: {0}")]
    InvalidTemporalClause(String),

    /// Invalid timestamp format.
    #[error("Invalid timestamp: {0}")]
    InvalidTimestamp(String),

    /// Missing required clause.
    #[error("Missing required clause: {0}")]
    MissingClause(String),

    /// Type conversion error.
    #[error("Type error: {0}")]
    TypeError(String),

    /// Parameter binding error.
    #[error("Parameter error: {0}")]
    ParameterError(String),
}

impl From<sqlparser::parser::ParserError> for SqlError {
    fn from(err: sqlparser::parser::ParserError) -> Self {
        SqlError::ParseError(err.to_string())
    }
}

impl From<SqlError> for Error {
    fn from(e: SqlError) -> Self {
        #[cfg(feature = "observability")]
        crate::observability::METRICS
            .error_query_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Error::Query(QueryError::SyntaxError {
            message: e.to_string(),
        })
    }
}
