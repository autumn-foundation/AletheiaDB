//! GallifreyDB - A high-performance bi-temporal graph database.
//!
//! GallifreyDB tracks both **valid time** (when facts were true in reality) and
//! **transaction time** (when facts were recorded in the database). This enables
//! powerful time-traveling queries and historical analysis.
//!
//! **📊 [View Performance Benchmarks](../dev/bench/index.html)**
//!
//! # Features & Modules
//!
//! GallifreyDB is organized into several key modules:
//!
//! - **[`core`]**: Fundamental types (Node, Edge, PropertyValue, Timestamp).
//! - **[`query`]**: Fluent Query Builder and hybrid execution engine.
//! - **[`index`]**: Vector indexing (HNSW) and temporal indexes.
//! - **[`storage`]**: Pluggable storage backends (Current, Historical, Tiered).
//! - **[`experimental`]**: Cutting-edge features (Narrative, Fishing) - *requires `nova` feature*.
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
//! use gallifreydb::{GallifreyDB, properties, WriteOps};
//! use gallifreydb::core::temporal::time;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = GallifreyDB::new()?;
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
//!
//! ## Fluent Query API
//!
//! Combine graph traversal, vector search, and temporal queries:
//!
//! ```rust,no_run
//! # use gallifreydb::{GallifreyDB, query::QueryBuilder};
//! # use gallifreydb::core::NodeId;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let db = GallifreyDB::new()?;
//! # let alice_id = NodeId::new(1)?;
//! # let query_embedding = vec![0.1, 0.2, 0.3];
//! // "Who did Alice know in 2023 that is semantically similar to this embedding?"
//! let results = db.query()
//!     .as_of_valid_time(gallifreydb::core::temporal::Timestamp::from(1672531200000000)) // 2023-01-01
//!     .start(alice_id)
//!     .traverse("KNOWS")
//!     .rank_by_similarity(&query_embedding, 10)
//!     .execute(&db)?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Vector Search
//!
//! Enable and use vector indexes for semantic similarity:
//!
//! ```rust,no_run
//! # use gallifreydb::{GallifreyDB, PropertyMapBuilder};
//! # use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = GallifreyDB::new()?;
//!
//! // 1. Enable vector indexing on "embedding" property
//! db.vector_index("embedding")
//!     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
//!     .enable()?;
//!
//! // 2. Add nodes with vectors (automatically indexed)
//! let embedding = vec![0.1f32; 384]; // Your actual embedding
//! db.create_node("Document",
//!     PropertyMapBuilder::new()
//!         .insert("text", "Hello world")
//!         .insert_vector("embedding", &embedding)
//!         .build()
//! )?;
//!
//! // 3. Search
//! let results = db.find_similar_by_embedding(&embedding, 5)?;
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
pub mod utils;
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
// Custom Honeycomb client module (replaces libhoney-rust git dependency)
#[cfg(feature = "honeycomb-client")]
pub mod honeycomb;
// Optional SQL:2011 temporal syntax support
#[cfg(feature = "sql")]
pub mod sql;
// Optional Cypher Query Language support
#[cfg(feature = "cypher")]
pub mod cypher;
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

pub use api::{
    ReadOps, ReadTransaction, TxId, TxState, VectorIndexBuilder, WriteOps, WriteTransaction,
};
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
