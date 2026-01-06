//! Utility modules for GallifreyDB.

pub mod error;
pub mod lock;

// Re-export commonly used types
pub use error::{
    Error, QueryError, Result, StorageError, TemporalError, TransactionError, VectorError,
};
pub use lock::{MutexExt, RwLockExt};
