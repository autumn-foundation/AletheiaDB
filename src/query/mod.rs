//! Hybrid Query Engine for AletheiaDB
//!
//! This module implements the query engine that powers AletheiaDB's unified
//! graph, vector, and temporal querying capabilities.
//!
//! # Architecture
//!
//! The query engine follows a classic database architecture pipeline:
//!
//! ```text
//! ┌───────────────┐      ┌───────────────┐      ┌───────────────┐
//! │  Query Input  │      │  Logical Plan │      │ Physical Plan │
//! │ (Builder/AQL) ├─────►│     (Tree)    ├─────►│  (Executable) ├─────► Results
//! └───────────────┘      └───────┬───────┘      └───────▲───────┘
//!                                │                      │
//!                        ┌───────▼───────┐      ┌───────┴───────┐
//!                        │  Optimizer    │      │   Executor    │
//!                        │ (Rules/Cost)  │      │  (Iterators)  │
//!                        └───────────────┘      └───────────────┘
//! ```
//!
//! 1. **Input**: Queries are constructed via the fluent [`QueryBuilder`] or parsed from AQL strings.
//! 2. **Logical Plan**: A high-level tree of operations ([`LogicalPlan`]) representing *what* to do.
//! 3. **Optimization**: The [`QueryPlanner`] applies heuristic rules and cost-based optimization.
//! 4. **Physical Plan**: A low-level executable plan ([`PhysicalPlan`]) representing *how* to do it.
//! 5. **Execution**: The [`QueryExecutor`] runs the plan using a pull-based iterator model.
//!
//! # Key Features
//!
//! - **Hybrid Queries**: Seamlessly mix graph traversals, vector similarity search, and filtering.
//! - **Bi-Temporal**: All queries can be executed `AS OF` a specific valid time and transaction time.
//! - **Vector Indexing**: Integrated HNSW index for fast approximate nearest neighbor search.
//! - **Cost-Based Optimization**: Uses statistics to reorder operations (e.g., filter before traverse).
//!
//! # Examples
//!
//! ## 1. Hybrid Graph + Vector Query
//!
//! "Find documents similar to 'Rust' that are written by people Alice knows."
//!
//! ```rust,no_run
//! # use aletheiadb::AletheiaDB;
//! # use aletheiadb::core::NodeId;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let db = AletheiaDB::new()?;
//! # let alice_id = NodeId::new(1)?;
//! # let rust_embedding = vec![0.1, 0.2, 0.3];
//! let results = db.query()
//!     .start(alice_id)
//!     .traverse("KNOWS")              // Find people Alice knows
//!     .traverse("WROTE")              // Find documents they wrote
//!     .rank_by_similarity(&rust_embedding, 10) // Rank by content similarity
//!     .execute(&db)?;
//! # Ok(())
//! # }
//! ```
//!
//! ## 2. Temporal Query
//!
//! "What did the graph look like on Jan 1st, 2023?"
//!
//! ```rust,no_run
//! # use aletheiadb::AletheiaDB;
//! # use aletheiadb::core::NodeId;
//! # use aletheiadb::core::temporal::Timestamp;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let db = AletheiaDB::new()?;
//! # let node_id = NodeId::new(1)?;
//! # let time_2023 = Timestamp::new(1672531200000000, 0)?;
//! # let tx_time = aletheiadb::core::temporal::time::now();
//! let results = db.query()
//!     .as_of(time_2023, tx_time)      // Set temporal context
//!     .start(node_id)
//!     .traverse("KNOWS")              // Traversal respects history
//!     .execute(&db)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Submodules
//!
//! - [`builder`]: Fluent API for constructing queries programmatically.
//! - [`parser`]: Recursive descent parser for AQL query strings.
//! - [`planner`]: Logical-to-physical transformation and cost-based optimization.
//! - [`executor`]: Iterator-based execution engine.
//! - [`ir`]: Intermediate Representation types ([`QueryOp`], [`Predicate`]).
//! - [`ast`]: Abstract Syntax Tree for parsed AQL queries.

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
pub mod result;
pub mod semantic_pathfinding;

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
pub use result::{EntityHistory, VersionDiff, VersionInfo, VersionSummary};
pub use semantic_pathfinding::SemanticPathfinder;
