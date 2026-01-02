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
    /// Transaction-related errors.
    Transaction(TransactionError),
    /// Vector-related errors.
    Vector(VectorError),
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
            Error::Transaction(e) => write!(f, "Transaction error: {}", e),
            Error::Vector(e) => write!(f, "Vector error: {}", e),
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

impl From<TransactionError> for Error {
    fn from(e: TransactionError) -> Self {
        Error::Transaction(e)
    }
}

impl From<VectorError> for Error {
    fn from(e: VectorError) -> Self {
        Error::Vector(e)
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
    DuplicateId {
        /// The duplicate ID
        id: String,
        /// The kind of entity (node/edge)
        kind: String,
    },
    /// Invalid property value or type.
    InvalidProperty {
        /// The property key
        key: String,
        /// Why the property is invalid
        reason: String,
    },
    /// Database is in an inconsistent state.
    InconsistentState {
        /// Why the state is inconsistent
        reason: String,
    },
    /// Write-ahead log error.
    WalError {
        /// The error reason
        reason: String,
    },
    /// Checkpoint error.
    CheckpointError {
        /// The error reason
        reason: String,
    },
    /// I/O error during persistence operations.
    IoError(String),
    /// Corrupted data detected.
    CorruptedData(String),
    /// A lock was poisoned by a panicking thread.
    ///
    /// This occurs when a thread panics while holding a lock, causing subsequent
    /// lock acquisitions to fail. This prevents cascade panics by returning an
    /// error instead of panicking.
    LockPoisoned {
        /// The type of lock that was poisoned (e.g., "Mutex", "RwLock")
        lock_type: &'static str,
    },
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
            StorageError::IoError(msg) => write!(f, "I/O error: {}", msg),
            StorageError::CorruptedData(msg) => write!(f, "Corrupted data: {}", msg),
            StorageError::LockPoisoned { lock_type } => {
                write!(
                    f,
                    "{} lock poisoned: a thread panicked while holding this lock",
                    lock_type
                )
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
        /// The previous transaction time
        previous: Timestamp,
        /// The attempted transaction time
        attempted: Timestamp,
    },
    /// Invalid time range (start > end).
    InvalidTimeRange {
        /// The start timestamp
        start: Timestamp,
        /// The end timestamp
        end: Timestamp,
    },
    /// Temporal paradox detected (e.g., deleting before creating).
    TemporalParadox {
        /// Description of the paradox
        reason: String,
    },
    /// Valid time precedes creation.
    ValidTimeBeforeCreation {
        /// The valid time timestamp
        valid_time: Timestamp,
        /// The creation time timestamp
        creation_time: Timestamp,
    },
    /// Attempted to modify closed version.
    VersionAlreadyClosed {
        /// The version ID
        version_id: VersionId,
    },
    /// Version chain is corrupted.
    CorruptedVersionChain {
        /// The entity ID
        entity_id: String,
        /// Why the chain is corrupted
        reason: String,
    },
    /// Anchor not found in version chain.
    MissingAnchor {
        /// The entity ID
        entity_id: String,
    },
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
    SyntaxError {
        /// The error message
        message: String,
    },
    /// Invalid query parameter.
    InvalidParameter {
        /// The parameter name
        parameter: String,
        /// Why it's invalid
        reason: String,
    },
    /// Query timeout.
    Timeout {
        /// Duration in milliseconds
        duration_ms: u64,
    },
    /// Query result limit exceeded.
    LimitExceeded {
        /// The limit that was exceeded
        limit: usize,
    },
    /// Invalid traversal (e.g., edge doesn't connect specified nodes).
    InvalidTraversal {
        /// Why the traversal is invalid
        reason: String,
    },
    /// Type mismatch in query.
    TypeMismatch {
        /// The expected type
        expected: String,
        /// The actual type
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

/// Errors related to transaction operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionError {
    /// Transaction is not in the correct state for this operation.
    InvalidState {
        /// The current state
        current: String,
        /// The expected state
        expected: String,
    },
    /// Transaction has already been committed.
    AlreadyCommitted {
        /// The transaction ID
        tx_id: u64,
    },
    /// Transaction has been aborted.
    Aborted {
        /// The transaction ID
        tx_id: u64,
    },
    /// Write conflict detected.
    WriteConflict {
        /// The entity ID involved in the conflict
        entity_id: String,
        /// Why there's a conflict
        reason: String,
    },
    /// Snapshot Isolation serialization failure (write-write conflict).
    ///
    /// This occurs when two concurrent transactions try to modify the same entity
    /// and one commits after the other's snapshot was taken.
    SerializationFailure {
        /// The entity involved in the conflict
        entity: String,
        /// Why serialization failed
        reason: String,
    },
    /// Validation failed before commit.
    ValidationFailed {
        /// Why validation failed
        reason: String,
    },
    /// Commit failed.
    CommitFailed {
        /// Why commit failed
        reason: String,
    },
    /// Rollback failed.
    RollbackFailed {
        /// Why rollback failed
        reason: String,
    },
}

impl fmt::Display for TransactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransactionError::InvalidState { current, expected } => {
                write!(
                    f,
                    "Transaction in invalid state: expected {}, got {}",
                    expected, current
                )
            }
            TransactionError::AlreadyCommitted { tx_id } => {
                write!(f, "Transaction {} has already been committed", tx_id)
            }
            TransactionError::Aborted { tx_id } => {
                write!(f, "Transaction {} has been aborted", tx_id)
            }
            TransactionError::WriteConflict { entity_id, reason } => {
                write!(f, "Write conflict on {}: {}", entity_id, reason)
            }
            TransactionError::SerializationFailure { entity, reason } => {
                write!(f, "Serialization failure on {}: {}", entity, reason)
            }
            TransactionError::ValidationFailed { reason } => {
                write!(f, "Transaction validation failed: {}", reason)
            }
            TransactionError::CommitFailed { reason } => {
                write!(f, "Transaction commit failed: {}", reason)
            }
            TransactionError::RollbackFailed { reason } => {
                write!(f, "Transaction rollback failed: {}", reason)
            }
        }
    }
}

impl std::error::Error for TransactionError {}

/// Errors related to vector operations and validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VectorError {
    /// Vector dimensions do not match.
    DimensionMismatch {
        /// The expected dimension (from the first vector)
        expected: usize,
        /// The actual dimension (from the second vector)
        actual: usize,
    },
    /// Vector contains NaN (Not a Number) values.
    ContainsNaN {
        /// Number of NaN values found
        count: usize,
    },
    /// Vector contains infinity values.
    ContainsInfinity {
        /// Number of infinity values found
        count: usize,
    },
    /// Vector dimension exceeds maximum allowed.
    DimensionTooLarge {
        /// The actual dimension
        dimension: usize,
        /// The maximum allowed dimension
        max_allowed: usize,
    },
}

impl fmt::Display for VectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VectorError::DimensionMismatch { expected, actual } => {
                write!(
                    f,
                    "Vector dimension mismatch: expected {}, got {}",
                    expected, actual
                )
            }
            VectorError::ContainsNaN { count } => {
                write!(f, "Vector contains {} NaN value(s)", count)
            }
            VectorError::ContainsInfinity { count } => {
                write!(f, "Vector contains {} infinity value(s)", count)
            }
            VectorError::DimensionTooLarge {
                dimension,
                max_allowed,
            } => {
                write!(
                    f,
                    "Vector dimension {} exceeds maximum allowed {}",
                    dimension, max_allowed
                )
            }
        }
    }
}

impl std::error::Error for VectorError {}

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

    #[test]
    fn test_all_storage_error_variants() {
        // Test EdgeNotFound
        let err = StorageError::EdgeNotFound(EdgeId::new(1));
        assert!(format!("{}", err).contains("Edge not found"));

        // Test VersionNotFound
        let err = StorageError::VersionNotFound(VersionId::new(1));
        assert!(format!("{}", err).contains("Version not found"));

        // Test DuplicateId
        let err = StorageError::DuplicateId {
            id: "42".to_string(),
            kind: "node".to_string(),
        };
        assert!(format!("{}", err).contains("Duplicate"));
        assert!(format!("{}", err).contains("node"));

        // Test InconsistentState
        let err = StorageError::InconsistentState {
            reason: "test".to_string(),
        };
        assert!(format!("{}", err).contains("Inconsistent"));

        // Test WalError
        let err = StorageError::WalError {
            reason: "flush failed".to_string(),
        };
        assert!(format!("{}", err).contains("Write-ahead log"));
        assert!(format!("{}", err).contains("flush failed"));

        // Test CheckpointError
        let err = StorageError::CheckpointError {
            reason: "save failed".to_string(),
        };
        assert!(format!("{}", err).contains("Checkpoint"));
        assert!(format!("{}", err).contains("save failed"));

        // Test IoError
        let err = StorageError::IoError("file not found".to_string());
        assert!(format!("{}", err).contains("I/O error"));
        assert!(format!("{}", err).contains("file not found"));

        // Test CorruptedData
        let err = StorageError::CorruptedData("bad checksum".to_string());
        assert!(format!("{}", err).contains("Corrupted data"));
        assert!(format!("{}", err).contains("bad checksum"));

        // Test LockPoisoned
        let err = StorageError::LockPoisoned { lock_type: "Mutex" };
        assert!(format!("{}", err).contains("Mutex"));
        assert!(format!("{}", err).contains("lock poisoned"));
        assert!(format!("{}", err).contains("panicked"));
    }

    #[test]
    fn test_all_temporal_error_variants() {
        // Test InvalidTimeRange
        let err = TemporalError::InvalidTimeRange {
            start: 100,
            end: 50,
        };
        assert!(format!("{}", err).contains("Invalid time range"));
        assert!(format!("{}", err).contains("100"));
        assert!(format!("{}", err).contains("50"));

        // Test ValidTimeBeforeCreation
        let err = TemporalError::ValidTimeBeforeCreation {
            valid_time: 50,
            creation_time: 100,
        };
        assert!(format!("{}", err).contains("precedes creation"));

        // Test VersionAlreadyClosed
        let err = TemporalError::VersionAlreadyClosed {
            version_id: VersionId::new(42),
        };
        assert!(format!("{}", err).contains("already closed"));
        assert!(format!("{}", err).contains("42"));

        // Test CorruptedVersionChain
        let err = TemporalError::CorruptedVersionChain {
            entity_id: "node-123".to_string(),
            reason: "missing delta".to_string(),
        };
        assert!(format!("{}", err).contains("Corrupted version chain"));
        assert!(format!("{}", err).contains("node-123"));
        assert!(format!("{}", err).contains("missing delta"));

        // Test MissingAnchor
        let err = TemporalError::MissingAnchor {
            entity_id: "edge-456".to_string(),
        };
        assert!(format!("{}", err).contains("Missing anchor"));
        assert!(format!("{}", err).contains("edge-456"));
    }

    #[test]
    fn test_all_query_error_variants() {
        // Test SyntaxError
        let err = QueryError::SyntaxError {
            message: "unexpected token".to_string(),
        };
        assert!(format!("{}", err).contains("syntax error"));
        assert!(format!("{}", err).contains("unexpected token"));

        // Test InvalidParameter
        let err = QueryError::InvalidParameter {
            parameter: "limit".to_string(),
            reason: "must be positive".to_string(),
        };
        assert!(format!("{}", err).contains("Invalid query parameter"));
        assert!(format!("{}", err).contains("limit"));
        assert!(format!("{}", err).contains("must be positive"));

        // Test LimitExceeded
        let err = QueryError::LimitExceeded { limit: 1000 };
        assert!(format!("{}", err).contains("limit"));
        assert!(format!("{}", err).contains("1000"));
        assert!(format!("{}", err).contains("exceeded"));

        // Test InvalidTraversal
        let err = QueryError::InvalidTraversal {
            reason: "edge doesn't connect nodes".to_string(),
        };
        assert!(format!("{}", err).contains("Invalid graph traversal"));
        assert!(format!("{}", err).contains("edge doesn't connect nodes"));
    }

    #[test]
    fn test_all_transaction_error_variants() {
        // Test InvalidState
        let err = TransactionError::InvalidState {
            current: "Committed".to_string(),
            expected: "Active".to_string(),
        };
        assert!(format!("{}", err).contains("invalid state"));
        assert!(format!("{}", err).contains("Committed"));
        assert!(format!("{}", err).contains("Active"));

        // Test AlreadyCommitted
        let err = TransactionError::AlreadyCommitted { tx_id: 123 };
        assert!(format!("{}", err).contains("already been committed"));
        assert!(format!("{}", err).contains("123"));

        // Test Aborted
        let err = TransactionError::Aborted { tx_id: 456 };
        assert!(format!("{}", err).contains("aborted"));
        assert!(format!("{}", err).contains("456"));

        // Test WriteConflict
        let err = TransactionError::WriteConflict {
            entity_id: "node-789".to_string(),
            reason: "concurrent modification".to_string(),
        };
        assert!(format!("{}", err).contains("Write conflict"));
        assert!(format!("{}", err).contains("node-789"));
        assert!(format!("{}", err).contains("concurrent modification"));

        // Test ValidationFailed
        let err = TransactionError::ValidationFailed {
            reason: "referential integrity violated".to_string(),
        };
        assert!(format!("{}", err).contains("validation failed"));
        assert!(format!("{}", err).contains("referential integrity violated"));

        // Test CommitFailed
        let err = TransactionError::CommitFailed {
            reason: "WAL write failed".to_string(),
        };
        assert!(format!("{}", err).contains("commit failed"));
        assert!(format!("{}", err).contains("WAL write failed"));

        // Test RollbackFailed
        let err = TransactionError::RollbackFailed {
            reason: "cleanup failed".to_string(),
        };
        assert!(format!("{}", err).contains("rollback failed"));
        assert!(format!("{}", err).contains("cleanup failed"));
    }

    #[test]
    fn test_transaction_error_conversions() {
        let err = TransactionError::ValidationFailed {
            reason: "test".to_string(),
        };
        let converted: Error = err.into();
        assert!(matches!(converted, Error::Transaction(_)));
    }

    #[test]
    fn test_vector_error_display() {
        let err = VectorError::DimensionMismatch {
            expected: 128,
            actual: 256,
        };
        assert!(format!("{}", err).contains("dimension mismatch"));
        assert!(format!("{}", err).contains("128"));
        assert!(format!("{}", err).contains("256"));

        let err = VectorError::ContainsNaN { count: 3 };
        assert!(format!("{}", err).contains("NaN"));
        assert!(format!("{}", err).contains("3"));

        let err = VectorError::ContainsInfinity { count: 2 };
        assert!(format!("{}", err).contains("infinity"));
        assert!(format!("{}", err).contains("2"));

        let err = VectorError::DimensionTooLarge {
            dimension: 200_000,
            max_allowed: 100_000,
        };
        assert!(format!("{}", err).contains("200000"));
        assert!(format!("{}", err).contains("100000"));
    }

    #[test]
    fn test_vector_error_conversions() {
        let err = VectorError::ContainsNaN { count: 1 };
        let converted: Error = err.into();
        assert!(matches!(converted, Error::Vector(_)));

        // Test that Display works on converted error
        assert!(format!("{}", converted).contains("Vector error"));
    }
}
