//! Utility modules for AletheiaDB.

pub mod error;

// Re-export commonly used types
pub use error::{
    Error, QueryError, Result, StorageError, TemporalError, TransactionError, VectorError,
};
