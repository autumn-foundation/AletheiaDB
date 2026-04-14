//! SQL Parser wrapper for sqlparser-rs.
//!
//! This module provides a thin wrapper around sqlparser-rs that handles
//! AletheiaDB-specific SQL dialect configuration.

use sqlparser::ast::Statement;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use super::error::SqlError;

/// SQL Parser for AletheiaDB.
///
/// Wraps sqlparser-rs with configuration appropriate for AletheiaDB's
/// SQL:2011 temporal extensions.
pub struct SqlParser;

impl SqlParser {
    /// Parse a SQL query string into an AST.
    ///
    /// # Arguments
    ///
    /// * `sql` - The SQL query string to parse
    ///
    /// # Returns
    ///
    /// * `Ok(Statement)` - The parsed SQL AST
    /// * `Err(SqlError)` - Parse error
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aletheiadb::sql::SqlParser;
    ///
    /// let stmt = SqlParser::parse("SELECT * FROM nodes WHERE label = 'Person'")?;
    /// ```
    pub fn parse(sql: &str) -> Result<Statement, SqlError> {
        let dialect = GenericDialect {};
        let mut statements = Parser::parse_sql(&dialect, sql)?;

        if statements.is_empty() {
            return Err(SqlError::ParseError("No SQL statement found".to_string()));
        }

        if statements.len() > 1 {
            return Err(SqlError::UnsupportedFeature(
                "Multiple statements not supported".to_string(),
            ));
        }

        Ok(statements.remove(0))
    }

    /// Parse a SQL query and return all statements.
    ///
    /// For advanced use cases where multiple statements may be needed.
    pub fn parse_multiple(sql: &str) -> Result<Vec<Statement>, SqlError> {
        let dialect = GenericDialect {};
        let statements = Parser::parse_sql(&dialect, sql)?;
        Ok(statements)
    }
}
