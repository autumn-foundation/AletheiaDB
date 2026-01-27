//! SQL:2011 Temporal Syntax Support for GallifreyDB
//!
//! This module provides SQL query language support with SQL:2011 temporal extensions,
//! enabling developers to query the database using familiar SQL syntax while maintaining
//! full bi-temporal capabilities.
//!
//! # Feature Flag
//!
//! This module is only available when the `sql` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! gallifreydb = { version = "0.1", features = ["sql"] }
//! ```
//!
//! # Architecture
//!
//! ```text
//! SQL String → [sqlparser] → SQL AST → [SqlConverter] → Query → [Planner] → Results
//! ```
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use gallifreydb::sql::parse_sql;
//!
//! // Basic query
//! let query = parse_sql("SELECT * FROM nodes WHERE label = 'Person' LIMIT 10")?;
//!
//! // Temporal query (SQL:2011)
//! let query = parse_sql(
//!     "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '2024-01-15 10:00:00'"
//! )?;
//! ```
//!
//! # Supported Syntax
//!
//! ## Phase 1: Basic SQL
//! - `SELECT` with column projection
//! - `FROM nodes` / `FROM edges` table references
//! - `WHERE` with comparison and logical operators
//! - `ORDER BY` with ASC/DESC
//! - `LIMIT` and `OFFSET`
//!
//! ## Phase 2: Temporal Extensions (SQL:2011)
//! - `FOR SYSTEM_TIME AS OF timestamp` - Point-in-time query
//! - `FOR SYSTEM_TIME BETWEEN t1 AND t2` - Time range query
//! - `FOR VALID_TIME AS OF timestamp` - Valid time query
//!
//! ## Phase 3: Graph Extensions
//! - `MATCH (source)-[:EDGE_TYPE*1..3]->(target)` - Path traversal
//!
//! ## Phase 4: Vector Extensions
//! - `ORDER BY embedding <=> '[0.1, 0.2, ...]'::vector LIMIT 10` - k-NN search

mod converter;
mod error;
mod parser;
mod temporal;
mod temporal_parser;

pub use converter::{SqlConverter, parse_sql, parse_sql_with_params};
pub use error::SqlError;
pub use parser::SqlParser;
pub use temporal::TemporalClause;

#[cfg(test)]
mod tests;
