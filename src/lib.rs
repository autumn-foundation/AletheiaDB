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
/// Encryption at rest (ADR-0028).
pub mod encryption;
pub mod index;
/// Tamper-evident provenance hash chain (Issue #3351): canonical version
/// encoder, append-only sidecar store, and offline verification.
pub mod provenance_chain;
pub mod query;
pub mod storage;
// Semantic search cohort (graduated from "Nova" in 0.1).
#[cfg(feature = "semantic-search")]
pub mod semantic_search;
// Experimental features ("Nova")
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
// Optional Cypher query language support
#[cfg(feature = "cypher")]
pub mod cypher;
// Optional HTTP server module
#[cfg(feature = "http-server")]
pub mod http;
// Authentication + RBAC core (Issue #3350, Phase 1). Shared by the HTTP
// server today and the MCP server in Phase 2, hence gated on either feature.
#[cfg(any(feature = "http-server", feature = "mcp-server"))]
pub mod auth;
// Signed audit export of entity history for compliance (Issue #3358).
// Offline-verifiable Ed25519-signed evidence artifacts built on history reads.
#[cfg(feature = "audit-export")]
pub mod audit;
// Test utilities: available in unit tests and when the simulation feature is enabled
// (integration tests under `--features simulation` need create_test_db).
#[cfg(any(test, feature = "simulation"))]
pub mod test_utils;

// Deterministic Simulation Testing framework (issue #154).
// Also available in unit tests without the feature flag so in-crate tests can
// inject a `SimulatedClock` into `time::now()` (Issue #3391).
#[cfg(any(test, feature = "simulation"))]
pub mod simulation;

// Internal cargo-fuzz hooks and Arbitrary implementations (issue #155).
#[cfg(any(fuzzing, feature = "fuzzing"))]
pub mod fuzzing;

/// Prelude for convenient imports of common types and traits.
pub mod prelude;

// Re-export commonly used types at the crate root
pub use config::{
    AletheiaDBConfig, AletheiaDBConfigBuilder, ConfigError, HistoricalConfig,
    HistoricalConfigBuilder, VectorIndexConfig, VectorIndexConfigBuilder, WalConfig,
    WalConfigBuilder,
};
pub use core::temporal::time;
pub use core::{
    BiTemporalInterval, ChangeFeedPage, ChangeFeedQuery, ChangeFilter, ChangeRecord, ChangeType,
    ChangefeedBroadcaster, ChangefeedConfig, Edge, EdgeId, EntityId, EntityKind, GLOBAL_INTERNER,
    InternedString, NAMESPACE_KEY, Namespace, NamespaceError, Node, NodeHeader, NodeId,
    PropertyKey, PropertyMap, PropertyMapBuilder, PropertyValue, Provenance, RecvError,
    StringInterner, Subscription, TimeRange, Timestamp, VersionId,
};

pub use api::{ReadOps, ReadTransaction, TxId, TxState, WriteOps, WriteTransaction};
pub use core::error::{
    ConstraintError, Error, QueryError, Result, StorageError, TemporalError, TransactionError,
};
pub use db::{
    AletheiaDB, BackupSummary, ColdStorageDetails, ColdStorageTierStats, CurrentStateStats,
    DatabaseStats, EdgeTypeSchema, FactStatus, GraphSchema, HistoricalDepthStats, LabelExtent,
    LabelSchema, LineageView, LineageViewEntry, NamespaceInfo, PitrCoord, PitrPlan, PitrTarget,
    SchemaInstant, SimilarityQuery, SimilaritySource, TemporalExtent, TierAccessStats, TimeBounds,
    UniqueConstraintBuilder, VectorIndexBuilder, WalStateStats,
};
pub use index::{
    AdjacencyIndex, CurrentIndexes, TemporalIndexes,
    vector::{DistanceMetric, HnswConfig, TemporalVectorConfig},
};
pub use storage::CurrentStorage;
pub use storage::backup::BackupError;
pub use storage::index_persistence::PersistenceConfig;
pub use storage::wal::{DurabilityMode, WriteOptions};

// Query planner re-exports (VS-060)
pub use query::{
    Query, QueryBuilder, QueryExecutor, QueryPlanner, QueryResults, QueryRow,
    ir::{Direction, Predicate, QueryOp, TraversalDepth},
    plan::{LogicalOp, LogicalPlan},
    planner::{Cost, CostModel, PhysicalOp, PhysicalPlan, Statistics},
};
