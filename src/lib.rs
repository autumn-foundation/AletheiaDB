//! AletheiaDB - A high-performance bi-temporal graph database.
//!
//! AletheiaDB tracks both **valid time** (when facts were true in reality) and
//! **transaction time** (when facts were recorded in the database). This enables
//! powerful time-traveling queries and historical analysis.
//!
//! **📊 [View Performance Benchmarks](../dev/bench/index.html)**
//!
//! # Architecture
//!
//! - **Hybrid Storage**: Separate current state (fast path) from historical data (temporal path)
//! - **Anchor+Delta Compression**: Reduces storage overhead by 5-6X
//! - **Copy-on-Write Properties**: Efficient property sharing across versions
//! - **String Interning**: Memory-efficient label and property key storage
//!
//! # Primary Use Case
//!
//! Designed for LLM integration, enabling reasoning systems to:
//! - Query historical knowledge states
//! - Track how facts evolved over time
//! - Detect contradictions through provenance
//! - Reason about temporal causality
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::{AletheiaDB, properties, WriteOps};
//! use aletheiadb::core::temporal::time;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//!
//! // Create a node
//! let alice = db.create_node("Person", properties! {
//!     "name" => "Alice",
//!     "age" => 30,
//! })?;
//!
//! // Later, update a property
//! db.write(|tx| tx.update_node(alice, properties! {
//!     "age" => 31,
//! }))?;
//!
//! // Query current state
//! let current = db.get_node(alice)?;
//!
//! // Time-travel to see historical state
//! // Use current time as a placeholder for the point in time we want to query
//! let now = time::now();
//! let historical = db.get_node_at_time(alice, now, now)?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod api;
pub mod config;
pub mod core;
pub mod db;
pub mod index;
pub mod query;
pub mod storage;
// Experimental features ("Nova")
#[cfg(any(feature = "nova", test))] // Allow in tests for verification
pub mod experimental;
// Optional embedding generation module
#[cfg(feature = "embeddings")]
pub mod embeddings;
// Optional observability infrastructure
#[cfg(feature = "observability")]
pub mod observability;
// Optional MCP server module
#[cfg(feature = "mcp-server")]
pub mod mcp;
// Optional SQL:2011 temporal syntax support
#[cfg(feature = "sql")]
pub mod sql;
// Optional HTTP server module
#[cfg(feature = "http-server")]
pub mod http;
// Test utilities (only available in tests)
#[cfg(test)]
pub mod test_utils;

// Re-export commonly used types at the crate root
pub use config::{
    AletheiaDBConfig, AletheiaDBConfigBuilder, ConfigError, HistoricalConfig,
    HistoricalConfigBuilder, VectorIndexConfig, VectorIndexConfigBuilder, WalConfig,
    WalConfigBuilder,
};
pub use core::temporal::time;
pub use core::{
    BiTemporalInterval, Edge, EdgeId, EntityId, GLOBAL_INTERNER, InternedString, Node, NodeId,
    PropertyKey, PropertyMap, PropertyMapBuilder, PropertyValue, StringInterner, TimeRange,
    Timestamp, VersionId,
};

pub use api::{ReadOps, ReadTransaction, TxId, TxState, WriteOps, WriteTransaction};
pub use core::error::{Error, QueryError, Result, StorageError, TemporalError, TransactionError};
pub use db::{AletheiaDB, VectorIndexBuilder};
pub use index::{
    AdjacencyIndex, CurrentIndexes, TemporalIndexes,
    vector::{DistanceMetric, HnswConfig},
};
pub use storage::CurrentStorage;
pub use storage::wal::{DurabilityMode, WriteOptions};

// Query planner re-exports (VS-060)
pub use query::{
    Query, QueryBuilder, QueryExecutor, QueryPlanner, QueryResults, QueryRow,
    ir::{Direction, Predicate, QueryOp, TraversalDepth},
    plan::{LogicalOp, LogicalPlan},
    planner::{Cost, CostModel, PhysicalOp, PhysicalPlan, Statistics},
};
