//! Utility modules for GallifreyDB.

pub mod error;

// Re-export commonly used types
pub use error::{Error, QueryError, Result, StorageError, TemporalError, TransactionError};
