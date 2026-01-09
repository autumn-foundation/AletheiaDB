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

pub mod builder;
pub mod executor;
pub mod ir;
pub mod plan;
pub mod planner;

// Re-export commonly used types
pub use builder::{Query, QueryBuilder};
pub use executor::{QueryExecutor, QueryResults, QueryRow};
pub use ir::{Direction, Predicate, QueryOp, TraversalDepth};
pub use plan::{LogicalOp, LogicalPlan};
pub use planner::{PhysicalPlan, QueryPlanner};
