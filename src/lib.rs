//! GallifreyDB - A high-performance bi-temporal graph database.
//!
//! GallifreyDB tracks both **valid time** (when facts were true in reality) and
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
//! ```
//! use gallifreydb::{GallifreyDB, properties, WriteOps};
//!
//! # fn main() -> gallifreydb::utils::Result<()> {
//! let db = GallifreyDB::new()?;
//!
//! // Create a node
//! let alice = db.create_node("Person", properties! {
//!     "name" => "Alice",
//!     "age" => 30,
//! })?;
//!
//! // Later, update a property
//! db.write(|tx| {
//!     tx.update_node(alice, properties! {
//!         "age" => 31,
//!     })?;
//!     Ok(())
//! })?;
//!
//! // Query current state
//! let current = db.get_node(alice)?;
//!
//! // Time-travel to see historical state
//! // use gallifreydb::core::temporal::Timestamp;
//! // let valid_time = Timestamp::now();
//! // let tx_time = Timestamp::now();
//! // let historical = db.get_node_at_time(alice, valid_time, tx_time)?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

/// Core API types including [`ReadTransaction`], [`WriteTransaction`] and ID types.
pub mod api;
/// Configuration structs for [`GallifreyDB`], WAL, and storage.
pub mod config;
/// Fundamental primitives like [`Node`], [`Edge`], [`Timestamp`] and [`PropertyMap`].
pub mod core;
/// Main database interface [`GallifreyDB`].
pub mod db;
/// Index implementations including [`VectorIndex`](index::VectorIndex) and [`AdjacencyIndex`].
pub mod index;
/// Query engine and planner.
pub mod query;
/// Storage engine internals (current, historical, WAL).
pub mod storage;
/// Utility types including [`Error`] and [`Result`].
pub mod utils;
// Optional embedding generation module
#[cfg(feature = "embeddings")]
pub mod embeddings;
// Optional observability infrastructure
#[cfg(feature = "observability")]
pub mod observability;
// Optional MCP server module
#[cfg(feature = "mcp-server")]
pub mod mcp;
// Custom Honeycomb client module (replaces libhoney-rust git dependency)
#[cfg(feature = "honeycomb-client")]
pub mod honeycomb;
// Optional SQL:2011 temporal syntax support
#[cfg(feature = "sql")]
pub mod sql;
// Optional HTTP server module
#[cfg(feature = "http-server")]
pub mod http;

// Re-export commonly used types at the crate root
pub use config::{
    ConfigError, GallifreyDBConfig, GallifreyDBConfigBuilder, HistoricalConfig,
    HistoricalConfigBuilder, VectorIndexConfig, VectorIndexConfigBuilder, WalConfig,
    WalConfigBuilder,
};
pub use core::{
    BiTemporalInterval, Edge, EdgeId, EntityId, GLOBAL_INTERNER, InternedString, Node, NodeId,
    PropertyKey, PropertyMap, PropertyMapBuilder, PropertyValue, StringInterner, TimeRange,
    Timestamp, VersionId,
};

pub use api::{ReadOps, ReadTransaction, TxId, TxState, WriteOps, WriteTransaction};
pub use db::GallifreyDB;
pub use index::{
    AdjacencyIndex, CurrentIndexes, TemporalIndexes,
    vector::{DistanceMetric, HnswConfig},
};
pub use storage::CurrentStorage;
pub use storage::wal::{DurabilityMode, WriteOptions};
pub use utils::{Error, QueryError, Result, StorageError, TemporalError, TransactionError};

// Query planner re-exports (VS-060)
pub use query::{
    Query, QueryBuilder, QueryExecutor, QueryPlanner, QueryResults, QueryRow,
    ir::{Direction, Predicate, QueryOp, TraversalDepth},
    plan::{LogicalOp, LogicalPlan},
    planner::{Cost, CostModel, PhysicalOp, PhysicalPlan, Statistics},
};
