//! GallifreyDB - A high-performance bi-temporal graph database.
//!
//! GallifreyDB tracks both **valid time** (when facts were true in reality) and
//! **transaction time** (when facts were recorded in the database). This enables
//! powerful time-traveling queries and historical analysis.
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
//! ```ignore
//! use gallifreydb::{GallifreyDB, properties};
//!
//! let db = GallifreyDB::new();
//!
//! // Create a node
//! let alice = db.create_node("Person", properties! {
//!     "name" => "Alice",
//!     "age" => 30,
//! })?;
//!
//! // Later, update a property
//! db.update_node(alice, properties! {
//!     "age" => 31,
//! })?;
//!
//! // Query current state
//! let current = db.get_node(alice)?;
//!
//! // Time-travel to see historical state
//! let historical = db.as_of(timestamp).get_node(alice)?;
//! ```

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod api;
pub mod core;
pub mod db;
pub mod index;
pub mod storage;
pub mod utils;

// Re-export commonly used types at the crate root
pub use core::{
    BiTemporalInterval, Edge, EdgeId, EntityId, GLOBAL_INTERNER, InternedString, Node, NodeId,
    PropertyKey, PropertyMap, PropertyMapBuilder, PropertyValue, StringInterner, TimeRange,
    Timestamp, VersionId,
};

pub use api::{ReadOps, ReadTransaction, TxId, TxState, WriteOps, WriteTransaction};
pub use db::GallifreyDB;
pub use index::{AdjacencyIndex, CurrentIndexes, TemporalIndexes};
pub use storage::CurrentStorage;
pub use utils::{Error, QueryError, Result, StorageError, TemporalError, TransactionError};
