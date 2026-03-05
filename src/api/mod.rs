//! Public API for AletheiaDB.
//!
//! This module contains the primary interfaces for interacting with the database,
//! organizing the "Hero's Journey" for developers building applications.
//!
//! # Overview
//!
//! AletheiaDB's API is designed around three core pillars:
//!
//! 1.  **Transactions** ([`transaction`]): All interactions (read or write) happen within
//!     ACID-compliant transactions.
//! 2.  **Hybrid Querying** ([`crate::query`]): A unified engine for Graph, Vector, and
//!     Temporal queries.
//! 3.  **Vector Search** ([`crate::db::vector_builder`]): Fluent configuration for embedding-based
//!     similarity search.
//!
//! # Quick Start
//!
//! ## 1. Initialize the Database
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//!
//! // Create a new database (defaults to in-memory for testing)
//! let db = AletheiaDB::new().expect("Failed to initialize database");
//! ```
//!
//! ## 2. Write Data (ACID Transaction)
//!
//! ```rust,no_run
//! # use aletheiadb::{AletheiaDB, PropertyMapBuilder};
//! # let db = AletheiaDB::new().unwrap();
//! use aletheiadb::api::transaction::WriteOps;
//!
//! // Write transaction (auto-commits on Ok)
//! let alice_id = db.write(|tx| {
//!     tx.create_node(
//!         "Person",
//!         PropertyMapBuilder::new()
//!             .insert("name", "Alice")
//!             .insert("role", "Engineer")
//!             .build()
//!     )
//! }).expect("Transaction failed");
//! ```
//!
//! ## 3. Query Data (Hybrid)
//!
//! ```rust,no_run
//! # use aletheiadb::{AletheiaDB, PropertyMapBuilder};
//! # use aletheiadb::api::transaction::WriteOps;
//! # let db = AletheiaDB::new().unwrap();
//! # let alice_id = db.write(|tx| tx.create_node("Person", PropertyMapBuilder::new().build())).unwrap();
//! // Find nodes connected to Alice
//! let results = db.query()
//!     .start(alice_id)
//!     .traverse("KNOWS")
//!     .execute(&db)
//!     .expect("Query failed");
//! ```
//!
//! # Key Concepts
//!
//! ## Bi-Temporality
//! Every fact in AletheiaDB tracks two dimensions of time:
//! - **Valid Time**: When the fact was true in the real world.
//! - **Transaction Time**: When the fact was recorded in the database.
//!
//! Use [`QueryBuilder::as_of`](crate::query::QueryBuilder::as_of) to travel through time.
//!
//! ## Vector Search
//! Attach embeddings to any node property and index them for similarity search.
//!
//! ```rust,no_run
//! # use aletheiadb::AletheiaDB;
//! # use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
//! # let db = AletheiaDB::new().unwrap();
//! // Enable vector index on "embedding" property
//! db.vector_index("embedding")
//!     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
//!     .enable()
//!     .expect("Failed to enable index");
//! ```
//!
//! # Module Map
//!
//! - **[`transaction`]**: Core ACID transaction types ([`ReadTransaction`], [`WriteTransaction`]).
//! - **[`crate::db::vector_builder`]**: Fluent builder for configuring vector indexes.
//! - **[`crate::db`]**: The main [`AletheiaDB`](crate::AletheiaDB) struct and entry point.
//! - **[`crate::query`]**: The fluent query builder API.

pub mod transaction;

// Re-export commonly used types
pub use transaction::{ReadOps, ReadTransaction, TxState, WriteOps, WriteTransaction};
