//! Cypher Query Language Support for AletheiaDB
//!
//! This module provides Cypher query language support with bi-temporal extensions,
//! enabling developers to query the graph database using the industry-standard
//! Cypher syntax while maintaining full temporal capabilities.
//!
//! # Feature Flag
//!
//! This module is only available when the `cypher` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! aletheiadb = { version = "0.1", features = ["cypher"] }
//! ```
//!
//! # Architecture
//!
//! ```text
//! Cypher String -> [Lexer] -> Tokens -> [Parser] -> Cypher AST -> [Planner] -> Query IR -> Results
//! ```
//!
//! The pipeline is built from scratch (no external parser dependency) to support
//! AletheiaDB's temporal and vector extensions natively.
//!
//! # Quick Start
//!
//! ```rust
//! use aletheiadb::cypher::CypherError;
//!
//! // Errors carry position information for diagnostics
//! let err = CypherError::ParseError {
//!     position: 42,
//!     message: "expected RETURN clause".to_string(),
//! };
//! assert_eq!(err.to_string(), "Cypher parse error at position 42: expected RETURN clause");
//! ```
//!
//! # Planned Syntax
//!
//! ## Phase 1: Core Cypher
//! - `MATCH (n:Label)-[:REL]->(m)` -- graph pattern matching
//! - `WHERE`, `RETURN`, `ORDER BY`, `LIMIT`, `SKIP`
//!
//! ## Phase 2: Temporal Extensions
//! - `AT TIME timestamp` -- point-in-time queries
//! - `BETWEEN t1 AND t2` -- time-range queries
//!
//! ## Phase 3: Vector Extensions
//! - `SIMILAR TO $embedding LIMIT k` -- k-NN search
//! - `RANK BY SIMILARITY` -- hybrid graph + vector queries

pub mod ast;
pub mod converter;
mod error;
pub mod exec;
pub mod lexer;
pub mod parser;

pub use ast::*;
pub use converter::{
    CypherConverter, CypherParameterValue, parse_cypher, parse_cypher_with_params,
};
pub use error::CypherError;
pub use exec::{CypherExecution, plan_cypher, plan_cypher_with_params};
pub use lexer::{CypherLexer, Token, TokenKind};
pub use parser::CypherParser;

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "cypher"))]
mod compat;
