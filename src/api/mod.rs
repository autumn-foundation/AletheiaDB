//! Public API for GallifreyDB
//!
//! This module provides high-level APIs for interacting with the database,
//! including transaction support for ACID guarantees.

pub mod transaction;
/// Builder pattern for configuring vector indexes.
pub mod vector_builder;

// Re-export commonly used types
pub use transaction::{ReadOps, ReadTransaction, TxId, TxState, WriteOps, WriteTransaction};
pub use vector_builder::VectorIndexBuilder;
