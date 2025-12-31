//! Error types for GallifreyDB.
//!
//! This module defines all error types that can occur during database operations.
//! Errors are organized by category for clarity.

use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::temporal::Timestamp;
use std::fmt;
use std::io;

/// Result type alias using GallifreyDB's Error type.
pub type Result<T> = std::result::Result<T, Error>;

/// Main error type for all GallifreyDB operations.
#[derive(Debug)]
pub enum Error {
    /// Storage-related errors.
    Storage(StorageError),
    /// Temporal constraint violations.
    Temporal(TemporalError),
    /// Query-related errors.
    Query(QueryError),
    /// I/O errors.
    Io(io::Error),
    /// Other errors.
    Other(String),
}

impl Error {
    /// Create a new error from a message.
    pub fn other<S: Into<String>>(msg: S) -> Self {
        Error::Other(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Storage(e) => write!(f, "Storage error: {}", e),
            Error::Temporal(e) => write!(f, "Temporal error: {}", e),
            Error::Query(e) => write!(f, "Query error: {}", e),
            Error::Io(e) => write!(f, "I/O error: {}", e),
            Error::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

// Conversions from specific error types to main Error type
impl From<StorageError> for Error {
    fn from(e: StorageError) -> Self {
        Error::Storage(e)
    }
}

impl From<TemporalError> for Error {
    fn from(e: TemporalError) -> Self {
        Error::Temporal(e)
    }
}

impl From<QueryError> for Error {
    fn from(e: QueryError) -> Self {
        Error::Query(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Errors related to storage operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// Node with the given ID was not found.
    NodeNotFound(NodeId),
    /// Edge with the given ID was not found.
    EdgeNotFound(EdgeId),
    /// Version with the given ID was not found.
    VersionNotFound(VersionId),
    /// Attempted to create a node/edge with an ID that already exists.
    DuplicateId { id: String, kind: String },
    /// Invalid property value or type.
    InvalidProperty { key: String, reason: String },
    /// Database is in an inconsistent state.
    InconsistentState { reason: String },
    /// Write-ahead log error.
    WalError { reason: String },
    /// Checkpoint error.
    CheckpointError { reason: String },
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::NodeNotFound(id) => write!(f, "Node not found: {}", id),
            StorageError::EdgeNotFound(id) => write!(f, "Edge not found: {}", id),
            StorageError::VersionNotFound(id) => write!(f, "Version not found: {}", id),
            StorageError::DuplicateId { id, kind } => {
                write!(f, "Duplicate {} ID: {}", kind, id)
            }
            StorageError::InvalidProperty { key, reason } => {
                write!(f, "Invalid property '{}': {}", key, reason)
            }
            StorageError::InconsistentState { reason } => {
                write!(f, "Inconsistent database state: {}", reason)
            }
            StorageError::WalError { reason } => {
                write!(f, "Write-ahead log error: {}", reason)
            }
            StorageError::CheckpointError { reason } => {
                write!(f, "Checkpoint error: {}", reason)
            }
        }
    }
}

impl std::error::Error for StorageError {}

/// Errors related to temporal constraints and bi-temporal operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemporalError {
    /// Transaction time is not monotonically increasing.
    NonMonotonicTransactionTime {
        previous: Timestamp,
        attempted: Timestamp,
    },
    /// Invalid time range (start > end).
    InvalidTimeRange { start: Timestamp, end: Timestamp },
    /// Temporal paradox detected (e.g., deleting before creating).
    TemporalParadox { reason: String },
    /// Valid time precedes creation.
    ValidTimeBeforeCreation {
        valid_time: Timestamp,
        creation_time: Timestamp,
    },
    /// Attempted to modify closed version.
    VersionAlreadyClosed { version_id: VersionId },
    /// Version chain is corrupted.
    CorruptedVersionChain { entity_id: String, reason: String },
    /// Anchor not found in version chain.
    MissingAnchor { entity_id: String },
}

impl fmt::Display for TemporalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemporalError::NonMonotonicTransactionTime {
                previous,
                attempted,
            } => {
                write!(
                    f,
                    "Transaction time must be monotonic: previous={}, attempted={}",
                    previous, attempted
                )
            }
            TemporalError::InvalidTimeRange { start, end } => {
                write!(f, "Invalid time range: start={} > end={}", start, end)
            }
            TemporalError::TemporalParadox { reason } => {
                write!(f, "Temporal paradox: {}", reason)
            }
            TemporalError::ValidTimeBeforeCreation {
                valid_time,
                creation_time,
            } => {
                write!(
                    f,
                    "Valid time ({}) precedes creation time ({})",
                    valid_time, creation_time
                )
            }
            TemporalError::VersionAlreadyClosed { version_id } => {
                write!(f, "Version {} is already closed", version_id)
            }
            TemporalError::CorruptedVersionChain { entity_id, reason } => {
                write!(f, "Corrupted version chain for {}: {}", entity_id, reason)
            }
            TemporalError::MissingAnchor { entity_id } => {
                write!(f, "Missing anchor in version chain for {}", entity_id)
            }
        }
    }
}

impl std::error::Error for TemporalError {}

/// Errors related to query operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    /// Query syntax error.
    SyntaxError { message: String },
    /// Invalid query parameter.
    InvalidParameter { parameter: String, reason: String },
    /// Query timeout.
    Timeout { duration_ms: u64 },
    /// Query result limit exceeded.
    LimitExceeded { limit: usize },
    /// Invalid traversal (e.g., edge doesn't connect specified nodes).
    InvalidTraversal { reason: String },
    /// Type mismatch in query.
    TypeMismatch {
        expected: String,
        actual: String,
    },
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryError::SyntaxError { message } => {
                write!(f, "Query syntax error: {}", message)
            }
            QueryError::InvalidParameter { parameter, reason } => {
                write!(f, "Invalid query parameter '{}': {}", parameter, reason)
            }
            QueryError::Timeout { duration_ms } => {
                write!(f, "Query timeout after {} ms", duration_ms)
            }
            QueryError::LimitExceeded { limit } => {
                write!(f, "Query result limit ({}) exceeded", limit)
            }
            QueryError::InvalidTraversal { reason } => {
                write!(f, "Invalid graph traversal: {}", reason)
            }
            QueryError::TypeMismatch { expected, actual } => {
                write!(f, "Type mismatch: expected {}, got {}", expected, actual)
            }
        }
    }
}

impl std::error::Error for QueryError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_error_display() {
        let err = StorageError::NodeNotFound(NodeId::new(42));
        assert_eq!(format!("{}", err), "Node not found: Node(42)");

        let err = StorageError::InvalidProperty {
            key: "age".to_string(),
            reason: "must be positive".to_string(),
        };
        assert!(format!("{}", err).contains("age"));
        assert!(format!("{}", err).contains("must be positive"));
    }

    #[test]
    fn test_temporal_error_display() {
        let err = TemporalError::NonMonotonicTransactionTime {
            previous: 100,
            attempted: 50,
        };
        assert!(format!("{}", err).contains("monotonic"));
        assert!(format!("{}", err).contains("100"));
        assert!(format!("{}", err).contains("50"));

        let err = TemporalError::TemporalParadox {
            reason: "deleted before created".to_string(),
        };
        assert!(format!("{}", err).contains("paradox"));
    }

    #[test]
    fn test_query_error_display() {
        let err = QueryError::Timeout { duration_ms: 5000 };
        assert_eq!(format!("{}", err), "Query timeout after 5000 ms");

        let err = QueryError::TypeMismatch {
            expected: "int".to_string(),
            actual: "string".to_string(),
        };
        assert!(format!("{}", err).contains("int"));
        assert!(format!("{}", err).contains("string"));
    }

    #[test]
    fn test_error_conversions() {
        let storage_err = StorageError::NodeNotFound(NodeId::new(1));
        let err: Error = storage_err.clone().into();
        assert!(matches!(err, Error::Storage(_)));

        let temporal_err = TemporalError::TemporalParadox {
            reason: "test".to_string(),
        };
        let err: Error = temporal_err.clone().into();
        assert!(matches!(err, Error::Temporal(_)));

        let query_err = QueryError::Timeout { duration_ms: 1000 };
        let err: Error = query_err.clone().into();
        assert!(matches!(err, Error::Query(_)));
    }

    #[test]
    fn test_error_display() {
        let err = Error::Storage(StorageError::NodeNotFound(NodeId::new(42)));
        assert!(format!("{}", err).contains("Storage error"));
        assert!(format!("{}", err).contains("Node not found"));

        let err = Error::Other("custom error".to_string());
        assert_eq!(format!("{}", err), "custom error");
    }

    #[test]
    fn test_result_type() {
        fn returns_result() -> Result<i32> {
            Ok(42)
        }

        fn returns_error() -> Result<i32> {
            Err(StorageError::NodeNotFound(NodeId::new(1)).into())
        }

        assert_eq!(returns_result().unwrap(), 42);
        assert!(returns_error().is_err());
    }
}
