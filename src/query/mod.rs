//! Hybrid Query Planner for GallifreyDB
//!
//! This module implements Phase 4 of SUPERRAG: a query planner that enables
//! unified queries combining **graph traversal**, **vector search**, and
//! **bi-temporal queries**.
//!
//! # Architecture
//!
//! ```text
//! Query → LogicalPlan → Optimization → PhysicalPlan → Execution
//! ```
//!
//! # Example Usage
//!
//! ```rust,ignore
//! // Graph + Vector: "Who does Alice know that's similar to Bob?"
//! let results = db.query()
//!     .start(alice_id)
//!     .traverse("KNOWS")
//!     .rank_by_similarity(&bob_embedding, 10)
//!     .execute()?;
//!
//! // Temporal + Vector: "What was similar to this in 2023?"
//! let results = db.query()
//!     .as_of(timestamp_2023, tx_time)
//!     .find_similar(&embedding, 10)
//!     .execute()?;
//! ```

pub mod ast;
pub mod builder;
pub mod converter;
pub mod executor;
pub mod hybrid;
pub mod ir;
pub mod lexer;
pub mod parser;
pub mod plan;
pub mod planner;

// Re-export commonly used types
pub use ast::QueryAst;
pub use builder::{Query, QueryBuilder};
pub use converter::{AstConverter, ParameterValue, parse_query, parse_query_with_params};
pub use executor::{QueryExecutor, QueryResults, QueryRow};
pub use hybrid::traverse_and_rank;
pub use ir::{Direction, Predicate, QueryOp, SortKey, TraversalDepth};
pub use lexer::{Lexer, LexerError, Token};
pub use parser::{ParseError, Parser};
pub use plan::{LogicalOp, LogicalPlan};
pub use planner::{PhysicalPlan, QueryPlanner};
