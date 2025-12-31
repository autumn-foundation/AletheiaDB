//! Public API for GallifreyDB
//!
//! This module provides high-level APIs for interacting with the database,
//! including transaction support for ACID guarantees.

pub mod transaction;

// Re-export commonly used types
pub use transaction::{ReadOps, ReadTransaction, TxId, TxState, WriteOps, WriteTransaction};
