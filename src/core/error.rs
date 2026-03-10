//! Error types for AletheiaDB.
//!
//! This module defines all error types that can occur during database operations.
//! Errors are organized by category for clarity.

use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::temporal::Timestamp;
use std::io;
use thiserror::Error;

/// Result type alias using AletheiaDB's Error type.
pub type Result<T> = std::result::Result<T, Error>;

/// Extension methods for [`Result`] values using AletheiaDB's error type.
pub trait ResultExt<T> {
    /// Record an error metric (if this is `Err`) and return the original result.
    fn record_error_metric(self) -> Self;
}

impl<T> ResultExt<T> for Result<T> {
    #[inline]
    fn record_error_metric(self) -> Self {
        if let Err(ref err) = self {
            err.record_metric();
        }
        self
    }
}

/// Main error type for all AletheiaDB operations.
#[derive(Debug, Error)]
pub enum Error {
    /// Storage-related errors.
    #[error("Storage error: {0}")]
    Storage(StorageError),
    /// Temporal constraint violations.
    #[error("Temporal error: {0}")]
    Temporal(TemporalError),
    /// Query-related errors.
    #[error("Query error: {0}")]
    Query(QueryError),
    /// Transaction-related errors.
    #[error("Transaction error: {0}")]
    Transaction(TransactionError),
    /// Vector-related errors.
    #[error("Vector error: {0}")]
    Vector(VectorError),
    /// I/O errors.
    #[error("I/O error: {0}")]
    Io(io::Error),
    /// Feature not yet implemented.
    #[error("Feature not implemented: {feature} ({reason})")]
    NotImplemented {
        /// The feature that is not implemented
        feature: String,
        /// Why it's not implemented (e.g., "Phase 4 feature")
        reason: String,
    },
    /// Other errors.
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Record an error metric for this error's category.
    ///
    /// This is intentionally explicit and side-effect free for constructors and
    /// conversions. Callers should record at API boundaries when returning errors.
    #[inline]
    pub fn record_metric(&self) {
        #[cfg(feature = "observability")]
        {
            let counter = match self {
                Error::Storage(_) => &crate::observability::METRICS.error_storage_total,
                Error::Temporal(_) => &crate::observability::METRICS.error_temporal_total,
                Error::Query(_) => &crate::observability::METRICS.error_query_total,
                Error::Transaction(_) => &crate::observability::METRICS.error_transaction_total,
                Error::Vector(_) => &crate::observability::METRICS.error_vector_total,
                Error::Io(_) => &crate::observability::METRICS.error_io_total,
                Error::NotImplemented { .. } | Error::Other(_) => {
                    &crate::observability::METRICS.error_other_total
                }
            };
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Create a new error from a message.
    pub fn other<S: Into<String>>(msg: S) -> Self {
        Error::Other(msg.into())
    }

    /// Create a new NotImplemented error.
    pub fn not_implemented<S: Into<String>, R: Into<String>>(feature: S, reason: R) -> Self {
        Error::NotImplemented {
            feature: feature.into(),
            reason: reason.into(),
        }
    }
}

// Manual From impls are explicit to keep category conversions easy to audit.

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

impl From<crate::config::ConfigError> for Error {
    fn from(e: crate::config::ConfigError) -> Self {
        Error::Other(e.to_string())
    }
}

#[cfg(feature = "sql")]
impl From<crate::sql::SqlError> for Error {
    fn from(e: crate::sql::SqlError) -> Self {
        let query_error = match e {
            crate::sql::SqlError::ParseError(message) => QueryError::SyntaxError { message },
            crate::sql::SqlError::UnsupportedFeature(feature) => {
                QueryError::UnsupportedFeature { feature }
            }
            crate::sql::SqlError::InvalidTable(table) => QueryError::InvalidParameter {
                parameter: "table".to_string(),
                reason: format!("invalid table '{}': expected 'nodes' or 'edges'", table),
            },
            crate::sql::SqlError::InvalidColumn(column) => QueryError::InvalidParameter {
                parameter: "column".to_string(),
                reason: format!("invalid column reference: {}", column),
            },
            crate::sql::SqlError::InvalidTemporalClause(clause) => QueryError::InvalidParameter {
                parameter: "temporal_clause".to_string(),
                reason: format!("invalid temporal clause: {}", clause),
            },
            crate::sql::SqlError::InvalidTimestamp(timestamp) => QueryError::InvalidParameter {
                parameter: "timestamp".to_string(),
                reason: format!("invalid timestamp: {}", timestamp),
            },
            crate::sql::SqlError::MissingClause(clause) => QueryError::InvalidParameter {
                parameter: "clause".to_string(),
                reason: format!("missing required clause: {}", clause),
            },
            crate::sql::SqlError::TypeError(actual) => QueryError::TypeMismatch {
                expected: "compatible SQL type".to_string(),
                actual,
            },
            crate::sql::SqlError::ParameterError(reason) => QueryError::InvalidParameter {
                parameter: "parameter".to_string(),
                reason,
            },
        };

        Error::Query(query_error)
    }
}

/// Structured categories for persistence failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceErrorKind {
    /// Corrupted index data.
    Corrupted,
    /// String interner mismatch while restoring persisted state.
    InternerMismatch,
    /// Unsupported persisted manifest/index version.
    UnsupportedVersion,
    /// Required persisted index file is missing.
    MissingIndex,
    /// Invalid file magic bytes.
    InvalidMagic,
    /// Size-limit violation (DoS protection).
    SizeLimitExceeded,
    /// I/O failure while reading/writing persisted data.
    Io,
    /// Serialization/deserialization failure.
    Serialization,
    /// Unknown or unclassified persistence failure.
    Unknown,
}

impl std::fmt::Display for PersistenceErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            PersistenceErrorKind::Corrupted => "corrupted",
            PersistenceErrorKind::InternerMismatch => "interner_mismatch",
            PersistenceErrorKind::UnsupportedVersion => "unsupported_version",
            PersistenceErrorKind::MissingIndex => "missing_index",
            PersistenceErrorKind::InvalidMagic => "invalid_magic",
            PersistenceErrorKind::SizeLimitExceeded => "size_limit_exceeded",
            PersistenceErrorKind::Io => "io",
            PersistenceErrorKind::Serialization => "serialization",
            PersistenceErrorKind::Unknown => "unknown",
        };
        write!(f, "{}", kind)
    }
}

impl From<&crate::storage::index_persistence::IndexPersistenceError> for PersistenceErrorKind {
    fn from(e: &crate::storage::index_persistence::IndexPersistenceError) -> Self {
        match e {
            crate::storage::index_persistence::IndexPersistenceError::Corrupted { .. } => {
                PersistenceErrorKind::Corrupted
            }
            crate::storage::index_persistence::IndexPersistenceError::InternerMismatch {
                ..
            } => PersistenceErrorKind::InternerMismatch,
            crate::storage::index_persistence::IndexPersistenceError::UnsupportedVersion {
                ..
            } => PersistenceErrorKind::UnsupportedVersion,
            crate::storage::index_persistence::IndexPersistenceError::MissingIndex { .. } => {
                PersistenceErrorKind::MissingIndex
            }
            crate::storage::index_persistence::IndexPersistenceError::InvalidMagic { .. } => {
                PersistenceErrorKind::InvalidMagic
            }
            crate::storage::index_persistence::IndexPersistenceError::SizeLimitExceeded {
                ..
            } => PersistenceErrorKind::SizeLimitExceeded,
            crate::storage::index_persistence::IndexPersistenceError::Io(_) => {
                PersistenceErrorKind::Io
            }
            crate::storage::index_persistence::IndexPersistenceError::Serialization(_) => {
                PersistenceErrorKind::Serialization
            }
        }
    }
}

/// Errors related to storage operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StorageError {
    /// Node with the given ID was not found.
    #[error("Node not found: {0}")]
    NodeNotFound(NodeId),
    /// Edge with the given ID was not found.
    #[error("Edge not found: {0}")]
    EdgeNotFound(EdgeId),
    /// Version with the given ID was not found.
    #[error("Version not found: {0}")]
    VersionNotFound(VersionId),
    /// Attempted to create a node/edge with an ID that already exists.
    #[error("Duplicate {kind} ID: {id}")]
    DuplicateId {
        /// The duplicate ID
        id: String,
        /// The kind of entity (node/edge)
        kind: String,
    },
    /// Invalid property value or type.
    #[error("Invalid property '{key}': {reason}")]
    InvalidProperty {
        /// The property key
        key: String,
        /// Why the property is invalid
        reason: String,
    },
    /// Database is in an inconsistent state.
    #[error("Inconsistent database state: {reason}")]
    InconsistentState {
        /// Why the state is inconsistent
        reason: String,
    },
    /// Write-ahead log error.
    #[error("Write-ahead log error: {reason}")]
    WalError {
        /// The error reason
        reason: String,
    },
    /// Checkpoint error.
    #[error("Checkpoint error: {reason}")]
    CheckpointError {
        /// The error reason
        reason: String,
    },
    /// I/O error during persistence operations.
    #[error("I/O error: {0}")]
    IoError(String),
    /// Corrupted data detected.
    #[error("Corrupted data: {0}")]
    CorruptedData(String),
    /// Index persistence error with structured category information.
    #[error("Persistence error [{kind}]: {message}")]
    PersistenceErrorWithKind {
        /// Persistence error category.
        kind: PersistenceErrorKind,
        /// Human-readable error message.
        message: String,
    },
    /// Legacy index persistence error message.
    ///
    /// This variant stores only a formatted message and does not retain structured
    /// persistence error category information.
    #[error("Persistence error: {0}")]
    PersistenceError(String),
    /// Property with the given key was not found.
    #[error("Property not found: {0}")]
    PropertyNotFound(String),
    /// Invalid ID value (out of range or reserved).
    #[error(
        "Invalid {id_type} ID {id}: exceeds maximum allowed value {max} (reserved range for internal use)",
        max = crate::core::id::MAX_VALID_ID
    )]
    InvalidId {
        /// The invalid ID value
        id: u64,
        /// The type of ID (node/edge/version)
        id_type: &'static str,
    },
    /// Capacity limit exceeded (DoS protection).
    #[error("Capacity exceeded for {resource}: current={current}, limit={limit} (DoS protection)")]
    CapacityExceeded {
        /// The resource that exceeded capacity
        resource: String,
        /// Current count
        current: usize,
        /// Maximum allowed
        limit: usize,
    },
    /// A Mutex lock was poisoned (a thread panicked while holding the lock).
    ///
    /// This indicates severe internal corruption. The affected resource cannot be
    /// safely accessed.
    #[error("Lock poisoned for {resource}: a thread panicked while holding the lock")]
    LockPoisoned {
        /// The resource whose lock was poisoned
        resource: String,
    },
}

impl StorageError {
    /// Create an I/O error with a message.
    pub fn io_error<S: Into<String>>(msg: S) -> Self {
        StorageError::IoError(msg.into())
    }

    /// Create a corruption error with a message.
    pub fn corruption<S: Into<String>>(msg: S) -> Self {
        StorageError::CorruptedData(msg.into())
    }

    /// Create a persistence error with a message.
    pub fn persistence<S: Into<String>>(msg: S) -> Self {
        StorageError::PersistenceError(msg.into())
    }

    /// Create a persistence error with explicit category information.
    pub fn persistence_with_kind<S: Into<String>>(kind: PersistenceErrorKind, msg: S) -> Self {
        StorageError::PersistenceErrorWithKind {
            kind,
            message: msg.into(),
        }
    }
}

impl From<crate::storage::index_persistence::IndexPersistenceError> for StorageError {
    fn from(e: crate::storage::index_persistence::IndexPersistenceError) -> Self {
        let kind = PersistenceErrorKind::from(&e);
        StorageError::PersistenceErrorWithKind {
            kind,
            message: e.to_string(),
        }
    }
}

/// Errors related to temporal constraints and bi-temporal operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TemporalError {
    /// Transaction time is not monotonically increasing.
    #[error("Transaction time must be monotonic: previous={previous}, attempted={attempted}")]
    NonMonotonicTransactionTime {
        /// The previous transaction time
        previous: Timestamp,
        /// The attempted transaction time
        attempted: Timestamp,
    },
    /// Invalid time range (start > end).
    #[error("Invalid time range: start={start} > end={end}")]
    InvalidTimeRange {
        /// The start timestamp
        start: Timestamp,
        /// The end timestamp
        end: Timestamp,
    },
    /// Invalid timestamp (exceeds valid range).
    #[error("Invalid timestamp {timestamp}: {reason}")]
    InvalidTimestamp {
        /// The invalid timestamp
        timestamp: Timestamp,
        /// Why the timestamp is invalid
        reason: String,
    },
    /// Temporal paradox detected (e.g., deleting before creating).
    #[error("Temporal paradox: {reason}")]
    TemporalParadox {
        /// Description of the paradox
        reason: String,
    },
    /// Valid time precedes creation.
    #[error("Valid time ({valid_time}) precedes creation time ({creation_time})")]
    ValidTimeBeforeCreation {
        /// The valid time timestamp
        valid_time: Timestamp,
        /// The creation time timestamp
        creation_time: Timestamp,
    },
    /// Attempted to modify closed version.
    #[error("Version {version_id} is already closed")]
    VersionAlreadyClosed {
        /// The version ID
        version_id: VersionId,
    },
    /// Version chain is corrupted.
    #[error("Corrupted version chain for {entity_id}: {reason}")]
    CorruptedVersionChain {
        /// The entity ID
        entity_id: String,
        /// Why the chain is corrupted
        reason: String,
    },
    /// Anchor not found in version chain.
    #[error("Missing anchor in version chain for {entity_id}")]
    MissingAnchor {
        /// The entity ID
        entity_id: String,
    },
    /// Maximum recursion depth exceeded during version reconstruction.
    ///
    /// This error indicates either a corrupted version chain (possible cycle)
    /// or an unusually long delta chain that exceeds the safety limit.
    #[error(
        "Maximum recursion depth ({max_depth}) exceeded while reconstructing {entity_id}: possible corrupted version chain or cycle"
    )]
    MaxDepthExceeded {
        /// The maximum depth that was exceeded
        max_depth: usize,
        /// The entity ID being reconstructed
        entity_id: String,
    },
    /// Node did not exist at the specified point in bi-temporal time.
    #[error(
        "Node {node_id} did not exist at valid_time={valid_time}, transaction_time={transaction_time}"
    )]
    NodeNotFoundAtTime {
        /// The node ID that was queried
        node_id: NodeId,
        /// The valid time timestamp
        valid_time: Timestamp,
        /// The transaction time timestamp
        transaction_time: Timestamp,
    },
    /// Version was not found.
    #[error("Version {0} not found")]
    VersionNotFound(VersionId),
    /// HLC logical counter overflow.
    ///
    /// This occurs when the logical counter would exceed u32::MAX, which theoretically
    /// requires 4+ billion events at the same microsecond wallclock. In practice, this
    /// indicates severe clock drift or a pathological workload.
    #[error(
        "HLC logical counter overflow at wallclock={wallclock}: current_logical={current_logical} would exceed u32::MAX"
    )]
    LogicalCounterOverflow {
        /// The wallclock at which overflow occurred
        wallclock: i64,
        /// The current logical counter value (u32::MAX)
        current_logical: u32,
    },
    /// Valid time is too far in the future.
    ///
    /// This prevents users from forward-dating facts decades or centuries into the future,
    /// which could be either an accident or a DoS attack attempting to fill the database
    /// with far-future timestamps.
    #[error(
        "valid_from {valid_from} is too far in future (current: {current_time}, max offset: {max_future_offset_us}\u{b5}s)"
    )]
    ValidTimeTooFarInFuture {
        /// The attempted valid_from timestamp
        valid_from: Timestamp,
        /// The current system time
        current_time: Timestamp,
        /// Maximum allowed future offset in microseconds
        max_future_offset_us: i64,
    },
    /// Valid time precedes entity creation.
    ///
    /// This prevents backdating a fact's valid_time to before the entity existed.
    /// For example, you cannot update a node with valid_from=Jan 1 if the node
    /// was created on Feb 1.
    #[error(
        "valid_from {valid_from} is before entity creation time {entity_creation_time} for {entity_id}"
    )]
    ValidTimeBeforeEntityCreation {
        /// The attempted valid_from timestamp
        valid_from: Timestamp,
        /// When the entity was created
        entity_creation_time: Timestamp,
        /// The entity ID (for error context)
        entity_id: String,
    },
}

/// Errors related to query operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum QueryError {
    /// Query syntax error.
    #[error("Query syntax error: {message}")]
    SyntaxError {
        /// The error message
        message: String,
    },
    /// Query uses a feature that is not currently supported.
    #[error("Unsupported query feature: {feature}")]
    UnsupportedFeature {
        /// The unsupported feature description.
        feature: String,
    },
    /// Invalid query parameter.
    #[error("Invalid query parameter '{parameter}': {reason}")]
    InvalidParameter {
        /// The parameter name
        parameter: String,
        /// Why it's invalid
        reason: String,
    },
    /// Query timeout.
    #[error("Query timeout after {duration_ms} ms")]
    Timeout {
        /// Duration in milliseconds
        duration_ms: u64,
    },
    /// Query result limit exceeded.
    #[error("Query result limit ({limit}) exceeded")]
    LimitExceeded {
        /// The limit that was exceeded
        limit: usize,
    },
    /// Invalid traversal (e.g., edge doesn't connect specified nodes).
    #[error("Invalid graph traversal: {reason}")]
    InvalidTraversal {
        /// Why the traversal is invalid
        reason: String,
    },
    /// Type mismatch in query.
    #[error("Type mismatch: expected {expected}, got {actual}")]
    TypeMismatch {
        /// The expected type
        expected: String,
        /// The actual type
        actual: String,
    },
    /// Query execution error (runtime failure).
    #[error("Query execution error: {message}")]
    ExecutionError {
        /// The error message
        message: String,
    },
    /// Required index not found during query planning.
    #[error("{}", format_index_not_found(index_type, property_name, hint))]
    IndexNotFound {
        /// The type of index required (e.g., "vector")
        index_type: String,
        /// The property name being indexed
        property_name: String,
        /// Optional hint on how to fix the issue
        hint: Option<String>,
    },
}

/// Format the `IndexNotFound` display message with optional hint.
fn format_index_not_found(index_type: &str, property_name: &str, hint: &Option<String>) -> String {
    let mut msg = format!(
        "Query requires {} index on '{}' property which is not enabled",
        index_type, property_name
    );
    if let Some(hint_msg) = hint {
        msg.push_str(&format!(". Hint: {}", hint_msg));
    }
    msg
}

/// Errors related to transaction operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransactionError {
    /// Transaction is not in the correct state for this operation.
    #[error("Transaction in invalid state: expected {expected}, got {current}")]
    InvalidState {
        /// The current state
        current: String,
        /// The expected state
        expected: String,
    },
    /// Transaction has already been committed.
    #[error("Transaction {tx_id} has already been committed")]
    AlreadyCommitted {
        /// The transaction ID
        tx_id: u64,
    },
    /// Transaction has been aborted.
    #[error("Transaction {tx_id} has been aborted")]
    Aborted {
        /// The transaction ID
        tx_id: u64,
    },
    /// Write conflict detected.
    #[error("Write conflict on {entity_id}: {reason}")]
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
    #[error("Serialization failure on {entity}: {reason}")]
    SerializationFailure {
        /// The entity involved in the conflict
        entity: String,
        /// Why serialization failed
        reason: String,
    },
    /// Validation failed before commit.
    #[error("Transaction validation failed: {reason}")]
    ValidationFailed {
        /// Why validation failed
        reason: String,
    },
    /// Commit failed.
    #[error("Transaction commit failed: {reason}")]
    CommitFailed {
        /// Why commit failed
        reason: String,
    },
    /// Rollback failed.
    #[error("Transaction rollback failed: {reason}")]
    RollbackFailed {
        /// Why rollback failed
        reason: String,
    },
    /// A Mutex lock was poisoned (a thread panicked while holding the lock).
    ///
    /// This indicates severe internal corruption. The affected resource cannot be
    /// safely accessed, and the transaction must be aborted.
    #[error("Lock poisoned for {resource}: a thread panicked while holding the lock")]
    LockPoisoned {
        /// The resource whose lock was poisoned
        resource: String,
    },
    /// Clock skew exceeds acceptable bounds.
    ///
    /// This occurs when the system clock jumps forward or backward by more than
    /// the configured thresholds (MAX_BACKWARD_DRIFT_US or MAX_FORWARD_JUMP_US).
    /// Such large jumps would break temporal query semantics by creating timestamps
    /// that are far in the future or violate causality.
    #[error("{}", format_clock_skew(*wallclock, *previous, *drift_us, *max_allowed))]
    ClockSkew {
        /// Current wallclock timestamp
        wallclock: i64,
        /// Previous commit timestamp
        previous: i64,
        /// Drift in microseconds (negative = backward, positive = forward)
        drift_us: i64,
        /// Maximum allowed drift for this direction
        max_allowed: i64,
    },
}

/// Format the `ClockSkew` display message with computed direction.
fn format_clock_skew(wallclock: i64, previous: i64, drift_us: i64, max_allowed: i64) -> String {
    let direction = if drift_us < 0 { "backward" } else { "forward" };
    format!(
        "Clock skew too large: system clock jumped {} by {} \u{b5}s (max allowed: {} \u{b5}s). \
         Wallclock: {}, Previous: {}. This may indicate NTP adjustment or manual clock change.",
        direction,
        drift_us.abs(),
        max_allowed.abs(),
        wallclock,
        previous
    )
}

/// Errors related to vector operations and validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VectorError {
    /// Vector dimensions do not match.
    #[error("Vector dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch {
        /// The expected dimension (from the first vector)
        expected: usize,
        /// The actual dimension (from the second vector)
        actual: usize,
    },
    /// Vector contains NaN (Not a Number) values.
    #[error("Vector contains {count} NaN value(s)")]
    ContainsNaN {
        /// Number of NaN values found
        count: usize,
    },
    /// Vector contains infinity values.
    #[error("Vector contains {count} infinity value(s)")]
    ContainsInfinity {
        /// Number of infinity values found
        count: usize,
    },
    /// Vector dimension exceeds maximum allowed.
    #[error("Vector dimension {dimension} exceeds maximum allowed {max_allowed}")]
    DimensionTooLarge {
        /// The actual dimension
        dimension: usize,
        /// The maximum allowed dimension
        max_allowed: usize,
    },
    /// Index is out of bounds for the vector or index structure.
    #[error("Vector index {index} out of bounds for length {len}")]
    IndexOutOfBounds {
        /// The index that was accessed
        index: usize,
        /// The valid range (0..len)
        len: usize,
    },
    /// Vector not found in storage or index.
    #[error("Vector not found: {id}")]
    NotFound {
        /// Identifier or description of the vector that was not found
        id: String,
    },
    /// Vector is invalid for the requested operation.
    #[error("Invalid vector: {reason}")]
    InvalidVector {
        /// Description of why the vector is invalid
        reason: String,
    },
    /// Sparse vector is invalid (e.g., duplicate indices, zero values).
    #[error("Invalid sparse vector: {reason}")]
    InvalidSparseVector {
        /// Description of why the sparse vector is invalid
        reason: String,
    },
    /// Error from underlying vector index implementation.
    #[error("Vector index error: {0}")]
    IndexError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_error_display() {
        let err = StorageError::NodeNotFound(NodeId::new(42).unwrap());
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
            previous: 100.into(),
            attempted: 50.into(),
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
        let storage_err = StorageError::NodeNotFound(NodeId::new(1).unwrap());
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
        let err = Error::Storage(StorageError::NodeNotFound(NodeId::new(42).unwrap()));
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
            Err(StorageError::NodeNotFound(NodeId::new(1).unwrap()).into())
        }

        assert_eq!(returns_result().unwrap(), 42);
        assert!(returns_error().is_err());
    }

    #[test]
    fn test_all_storage_error_variants() {
        // Test EdgeNotFound
        let err = StorageError::EdgeNotFound(EdgeId::new(1).unwrap());
        assert!(format!("{}", err).contains("Edge not found"));

        // Test VersionNotFound
        let err = StorageError::VersionNotFound(VersionId::new(1).unwrap());
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

        // Test PersistenceError
        let err = StorageError::PersistenceError("failed to save index".to_string());
        assert!(format!("{}", err).contains("Persistence error"));
        assert!(format!("{}", err).contains("failed to save index"));

        // Test PersistenceErrorWithKind
        let err = StorageError::PersistenceErrorWithKind {
            kind: PersistenceErrorKind::InvalidMagic,
            message: "bad header".to_string(),
        };
        assert!(format!("{}", err).contains("invalid_magic"));
        assert!(format!("{}", err).contains("bad header"));

        // Test PropertyNotFound
        let err = StorageError::PropertyNotFound("embedding".to_string());
        assert_eq!(format!("{}", err), "Property not found: embedding");
    }

    #[test]
    fn test_all_temporal_error_variants() {
        // Test InvalidTimeRange
        let err = TemporalError::InvalidTimeRange {
            start: 100.into(),
            end: 50.into(),
        };
        assert!(format!("{}", err).contains("Invalid time range"));
        assert!(format!("{}", err).contains("100"));
        assert!(format!("{}", err).contains("50"));

        // Test ValidTimeBeforeCreation
        let err = TemporalError::ValidTimeBeforeCreation {
            valid_time: 50.into(),
            creation_time: 100.into(),
        };
        assert!(format!("{}", err).contains("precedes creation"));

        // Test VersionAlreadyClosed
        let err = TemporalError::VersionAlreadyClosed {
            version_id: VersionId::new(42).unwrap(),
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

        // Test MaxDepthExceeded
        let err = TemporalError::MaxDepthExceeded {
            max_depth: 100,
            entity_id: "node-789".to_string(),
        };
        assert!(format!("{}", err).contains("Maximum recursion depth"));
        assert!(format!("{}", err).contains("100"));
        assert!(format!("{}", err).contains("node-789"));
        assert!(format!("{}", err).contains("corrupted version chain"));
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

        // Test IndexNotFound without hint
        let err = QueryError::IndexNotFound {
            index_type: "vector".to_string(),
            property_name: "embedding".to_string(),
            hint: None,
        };
        let display = format!("{}", err);
        assert!(display.contains("vector index"));
        assert!(display.contains("embedding"));
        assert!(display.contains("not enabled"));

        // Test IndexNotFound with hint
        let err = QueryError::IndexNotFound {
            index_type: "vector".to_string(),
            property_name: "embedding".to_string(),
            hint: Some("Call db.vector_index(\"embedding\").hnsw(...).enable() first".to_string()),
        };
        let display = format!("{}", err);
        assert!(display.contains("vector index"));
        assert!(display.contains("embedding"));
        assert!(display.contains("vector_index(\"embedding\").hnsw"));
    }

    #[test]
    fn test_index_persistence_error_conversion_preserves_kind() {
        let io_err = crate::storage::index_persistence::IndexPersistenceError::Serialization(
            "decode failed".to_string(),
        );
        let converted: StorageError = io_err.into();
        match converted {
            StorageError::PersistenceErrorWithKind { kind, message } => {
                assert_eq!(kind, PersistenceErrorKind::Serialization);
                assert!(message.contains("decode failed"));
            }
            other => panic!("expected PersistenceErrorWithKind, got {other:?}"),
        }
    }

    #[cfg(feature = "sql")]
    #[test]
    fn test_sql_error_conversion_preserves_semantics() {
        let converted: Error = crate::sql::SqlError::UnsupportedFeature("MATCH".to_string()).into();
        assert!(matches!(
            converted,
            Error::Query(QueryError::UnsupportedFeature { feature }) if feature == "MATCH"
        ));

        let converted: Error = crate::sql::SqlError::InvalidColumn("foo".to_string()).into();
        assert!(matches!(
            converted,
            Error::Query(QueryError::InvalidParameter { parameter, .. }) if parameter == "column"
        ));
    }

    #[cfg(feature = "observability")]
    #[test]
    fn test_result_ext_records_storage_metric_once() {
        crate::observability::METRICS.reset();

        let err: Result<()> = Err(StorageError::NodeNotFound(NodeId::new(1).unwrap()).into());
        let snapshot = crate::observability::METRICS.snapshot();
        assert_eq!(snapshot.error_storage_total, 0);

        let _ = err.record_error_metric();
        let snapshot = crate::observability::METRICS.snapshot();
        assert_eq!(snapshot.error_storage_total, 1);
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

        let err = VectorError::IndexOutOfBounds { index: 10, len: 5 };
        assert_eq!(
            format!("{}", err),
            "Vector index 10 out of bounds for length 5"
        );

        let err = VectorError::NotFound {
            id: "vec_12345".to_string(),
        };
        assert_eq!(format!("{}", err), "Vector not found: vec_12345");

        let err = VectorError::InvalidVector {
            reason: "empty vector not allowed".to_string(),
        };
        assert_eq!(
            format!("{}", err),
            "Invalid vector: empty vector not allowed"
        );
    }

    #[test]
    fn test_vector_error_conversions() {
        let err = VectorError::ContainsNaN { count: 1 };
        let converted: Error = err.into();
        assert!(matches!(converted, Error::Vector(_)));

        // Test that Display works on converted error
        assert!(format!("{}", converted).contains("Vector error"));
    }

    // ==================== Error Coverage Tests ====================

    #[test]
    fn test_temporal_error_node_not_found_at_time() {
        let node_id = NodeId::new(42).unwrap();
        let err = TemporalError::NodeNotFoundAtTime {
            node_id,
            valid_time: 1000.into(),
            transaction_time: 2000.into(),
        };

        let display = format!("{}", err);
        assert!(display.contains("Node"));
        assert!(display.contains("42"));
        assert!(display.contains("1000"));
        assert!(display.contains("2000"));
        assert!(display.contains("did not exist"));
    }

    #[test]
    fn test_temporal_error_version_not_found() {
        let version_id = VersionId::new(123).unwrap();
        let err = TemporalError::VersionNotFound(version_id);

        let display = format!("{}", err);
        assert!(display.contains("Version"));
        assert!(display.contains("123"));
        assert!(display.contains("not found"));
    }

    #[test]
    fn test_temporal_error_node_not_found_at_time_conversion() {
        let node_id = NodeId::new(1).unwrap();
        let err = TemporalError::NodeNotFoundAtTime {
            node_id,
            valid_time: 1000.into(),
            transaction_time: 2000.into(),
        };

        let converted: Error = err.into();
        assert!(matches!(converted, Error::Temporal(_)));

        let display = format!("{}", converted);
        assert!(display.contains("Temporal error"));
    }

    #[test]
    fn test_temporal_error_version_not_found_conversion() {
        let version_id = VersionId::new(123).unwrap();
        let err = TemporalError::VersionNotFound(version_id);

        let converted: Error = err.into();
        assert!(matches!(converted, Error::Temporal(_)));

        let display = format!("{}", converted);
        assert!(display.contains("Temporal error"));
    }

    // =========================================================================
    // Phase 2: True Bi-Temporal Validation Error Tests
    // =========================================================================

    #[test]
    fn test_valid_time_too_far_in_future_error_display() {
        use crate::core::hlc::HybridTimestamp;

        let err = TemporalError::ValidTimeTooFarInFuture {
            valid_from: HybridTimestamp::new(2000, 0).unwrap(),
            current_time: HybridTimestamp::new(1000, 0).unwrap(),
            max_future_offset_us: 31_536_000_000_000, // 1 year
        };

        let msg = format!("{}", err);
        assert!(msg.contains("too far in future"));
        assert!(msg.contains("2000"));
        assert!(msg.contains("1000"));
        assert!(msg.contains("31536000000000"));
    }

    #[test]
    fn test_valid_time_before_entity_creation_error_display() {
        use crate::core::hlc::HybridTimestamp;

        let err = TemporalError::ValidTimeBeforeEntityCreation {
            valid_from: HybridTimestamp::new(100, 0).unwrap(),
            entity_creation_time: HybridTimestamp::new(500, 0).unwrap(),
            entity_id: "node:123".to_string(),
        };

        let msg = format!("{}", err);
        assert!(msg.contains("before entity creation"));
        assert!(msg.contains("100"));
        assert!(msg.contains("500"));
        assert!(msg.contains("node:123"));
    }

    #[test]
    fn test_clock_skew_display_negative_drift_is_backward() {
        let err = TransactionError::ClockSkew {
            wallclock: 1_000,
            previous: 1_100,
            drift_us: -100,
            max_allowed: 50,
        };

        let msg = err.to_string();
        assert!(msg.contains("Clock skew too large"));
        assert!(msg.contains("backward"));
        assert!(msg.contains("100"));
        assert!(msg.contains("max allowed: 50"));
        assert!(msg.contains("Wallclock: 1000"));
        assert!(msg.contains("Previous: 1100"));
    }

    #[test]
    fn test_clock_skew_display_zero_drift_is_forward() {
        let err = TransactionError::ClockSkew {
            wallclock: 500,
            previous: 500,
            drift_us: 0,
            max_allowed: 25,
        };

        let msg = err.to_string();
        assert!(msg.contains("Clock skew too large"));
        assert!(msg.contains("forward"));
        assert!(!msg.contains("backward"));
        assert!(msg.contains("by 0"));
    }

    #[test]
    fn test_error_other() {
        let err = Error::other("something went wrong");
        match err {
            Error::Other(msg) => assert_eq!(msg, "something went wrong"),
            _ => panic!("expected Error::Other"),
        }
    }

    #[test]
    fn test_error_not_implemented() {
        let err = Error::not_implemented("feature X", "reason Y");
        match err {
            Error::NotImplemented { feature, reason } => {
                assert_eq!(feature, "feature X");
                assert_eq!(reason, "reason Y");
            }
            _ => panic!("expected Error::NotImplemented"),
        }
    }

    #[test]
    fn test_storage_error_constructors() {
        let err = StorageError::io_error("io err");
        assert_eq!(err, StorageError::IoError("io err".to_string()));

        let err = StorageError::corruption("bad bytes");
        assert_eq!(err, StorageError::CorruptedData("bad bytes".to_string()));

        let err = StorageError::persistence("db dead");
        assert_eq!(err, StorageError::PersistenceError("db dead".to_string()));

        let err = StorageError::persistence_with_kind(PersistenceErrorKind::InvalidMagic, "magic");
        assert_eq!(err, StorageError::PersistenceErrorWithKind {
            kind: PersistenceErrorKind::InvalidMagic,
            message: "magic".to_string(),
        });
    }

    #[test]
    fn test_error_from_conversions_exact() {
        // StorageError
        let se = StorageError::NodeNotFound(NodeId::new(1).unwrap());
        let e: Error = se.clone().into();
        assert!(matches!(e, Error::Storage(s) if s == se));

        // TemporalError
        let te = TemporalError::VersionNotFound(VersionId::new(1).unwrap());
        let e: Error = te.clone().into();
        assert!(matches!(e, Error::Temporal(t) if t == te));

        // QueryError
        let qe = QueryError::Timeout { duration_ms: 10 };
        let e: Error = qe.clone().into();
        assert!(matches!(e, Error::Query(q) if q == qe));

        // TransactionError
        let tre = TransactionError::Aborted { tx_id: 1 };
        let e: Error = tre.clone().into();
        assert!(matches!(e, Error::Transaction(t) if t == tre));

        // VectorError
        let ve = VectorError::ContainsNaN { count: 1 };
        let e: Error = ve.clone().into();
        assert!(matches!(e, Error::Vector(v) if v == ve));

        // io::Error
        let ioe = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let e: Error = ioe.into();
        assert!(matches!(e, Error::Io(i) if i.kind() == std::io::ErrorKind::NotFound));

        // ConfigError
        let ce = crate::config::ConfigError::IoError("bad config".to_string());
        let ce_msg = ce.to_string();
        let e: Error = ce.into();
        assert!(matches!(e, Error::Other(msg) if msg == ce_msg));
    }

    #[test]
    fn test_persistence_error_kind_display_all() {
        assert_eq!(PersistenceErrorKind::Corrupted.to_string(), "corrupted");
        assert_eq!(PersistenceErrorKind::InternerMismatch.to_string(), "interner_mismatch");
        assert_eq!(PersistenceErrorKind::UnsupportedVersion.to_string(), "unsupported_version");
        assert_eq!(PersistenceErrorKind::MissingIndex.to_string(), "missing_index");
        assert_eq!(PersistenceErrorKind::InvalidMagic.to_string(), "invalid_magic");
        assert_eq!(PersistenceErrorKind::SizeLimitExceeded.to_string(), "size_limit_exceeded");
        assert_eq!(PersistenceErrorKind::Io.to_string(), "io");
        assert_eq!(PersistenceErrorKind::Serialization.to_string(), "serialization");
        assert_eq!(PersistenceErrorKind::Unknown.to_string(), "unknown");
    }

    #[test]
    fn test_persistence_error_kind_from_index_persistence_error() {
        use crate::storage::index_persistence::IndexPersistenceError;

        assert_eq!(PersistenceErrorKind::from(&IndexPersistenceError::Corrupted { path: std::path::PathBuf::from(""), source: Box::new(std::io::Error::other("")) }), PersistenceErrorKind::Corrupted);
        assert_eq!(PersistenceErrorKind::from(&IndexPersistenceError::InternerMismatch { expected: 1, got: 2 }), PersistenceErrorKind::InternerMismatch);
        assert_eq!(PersistenceErrorKind::from(&IndexPersistenceError::UnsupportedVersion { found: 1, supported: 2 }), PersistenceErrorKind::UnsupportedVersion);
        assert_eq!(PersistenceErrorKind::from(&IndexPersistenceError::MissingIndex { name: "".into() }), PersistenceErrorKind::MissingIndex);
        assert_eq!(PersistenceErrorKind::from(&IndexPersistenceError::InvalidMagic { path: std::path::PathBuf::from(""), got: [0; 4], expected: [1; 4] }), PersistenceErrorKind::InvalidMagic);
        assert_eq!(PersistenceErrorKind::from(&IndexPersistenceError::SizeLimitExceeded { message: "".into() }), PersistenceErrorKind::SizeLimitExceeded);
        assert_eq!(PersistenceErrorKind::from(&IndexPersistenceError::Io(std::io::Error::other(""))), PersistenceErrorKind::Io);
        assert_eq!(PersistenceErrorKind::from(&IndexPersistenceError::Serialization("".into())), PersistenceErrorKind::Serialization);
    }

    #[test]
    fn test_storage_error_from_index_persistence_error_exact() {
        use crate::storage::index_persistence::IndexPersistenceError;
        let e = IndexPersistenceError::Corrupted { path: std::path::PathBuf::from(""), source: Box::new(std::io::Error::other("")) };
        let e_str = e.to_string();
        let se: StorageError = e.into();
        assert_eq!(se, StorageError::PersistenceErrorWithKind {
            kind: PersistenceErrorKind::Corrupted,
            message: e_str,
        });
    }

    #[cfg(feature = "observability")]
    #[test]
    fn test_result_ext_records_storage_metric_returns_original_error() {
        crate::observability::METRICS.reset();

        let err: Result<()> = Err(StorageError::NodeNotFound(NodeId::new(42).unwrap()).into());
        let res = err.record_error_metric();

        match res {
            Err(Error::Storage(StorageError::NodeNotFound(id))) => assert_eq!(id.as_u64(), 42),
            _ => panic!("Expected the original error back"),
        }

        // test for Ok case
        let ok_res: Result<i32> = Ok(100);
        assert_eq!(ok_res.record_error_metric().unwrap(), 100);
    }

    #[cfg(feature = "observability")]
    #[test]
    fn test_error_record_metric_all_categories() {
        let cases = vec![
            (Error::Storage(StorageError::NodeNotFound(NodeId::new(1).unwrap())), 0),
            (Error::Temporal(TemporalError::VersionNotFound(VersionId::new(1).unwrap())), 1),
            (Error::Query(QueryError::Timeout { duration_ms: 1 }), 2),
            (Error::Transaction(TransactionError::Aborted { tx_id: 1 }), 3),
            (Error::Vector(VectorError::ContainsNaN { count: 1 }), 4),
            (Error::Io(std::io::Error::other("")), 5),
            (Error::not_implemented("a", "b"), 6),
            (Error::other("c"), 6),
        ];

        for (err, idx) in cases {
            crate::observability::METRICS.reset();
            err.record_metric();
            let snap = crate::observability::METRICS.snapshot();
            let counts = [
                snap.error_storage_total,
                snap.error_temporal_total,
                snap.error_query_total,
                snap.error_transaction_total,
                snap.error_vector_total,
                snap.error_io_total,
                snap.error_other_total,
            ];
            for (i, count) in counts.iter().enumerate() {
                if i == idx {
                    assert_eq!(*count, 1, "Expected metric {} to be 1", i);
                } else {
                    assert_eq!(*count, 0, "Expected metric {} to be 0", i);
                }
            }
        }
    }

    #[test]
    fn test_format_index_not_found_exact() {
        let msg = format_index_not_found("vector", "embedding", &None);
        assert_eq!(msg, "Query requires vector index on 'embedding' property which is not enabled");

        let msg = format_index_not_found("vector", "embedding", &Some("Try harder".into()));
        assert_eq!(msg, "Query requires vector index on 'embedding' property which is not enabled. Hint: Try harder");
    }

    #[test]
    fn test_format_clock_skew_exact() {
        let msg = format_clock_skew(100, 200, -50, 20);
        assert_eq!(msg, "Clock skew too large: system clock jumped backward by 50 \u{b5}s (max allowed: 20 \u{b5}s). Wallclock: 100, Previous: 200. This may indicate NTP adjustment or manual clock change.");

        let msg = format_clock_skew(100, 200, 0, 20);
        assert_eq!(msg, "Clock skew too large: system clock jumped forward by 0 \u{b5}s (max allowed: 20 \u{b5}s). Wallclock: 100, Previous: 200. This may indicate NTP adjustment or manual clock change.");

        let msg = format_clock_skew(100, 200, 50, 20);
        assert_eq!(msg, "Clock skew too large: system clock jumped forward by 50 \u{b5}s (max allowed: 20 \u{b5}s). Wallclock: 100, Previous: 200. This may indicate NTP adjustment or manual clock change.");
    }

    #[cfg(feature = "sql")]
    #[test]
    fn test_sql_error_conversion_exhaustive() {
        use crate::sql::SqlError;
        let cases = vec![
            (SqlError::ParseError("bad syntax".into()), QueryError::SyntaxError { message: "bad syntax".into() }),
            (SqlError::UnsupportedFeature("MATCH".into()), QueryError::UnsupportedFeature { feature: "MATCH".into() }),
            (SqlError::InvalidTable("foo".into()), QueryError::InvalidParameter { parameter: "table".into(), reason: "invalid table 'foo': expected 'nodes' or 'edges'".into() }),
            (SqlError::InvalidColumn("bar".into()), QueryError::InvalidParameter { parameter: "column".into(), reason: "invalid column reference: bar".into() }),
            (SqlError::InvalidTemporalClause("AS OF".into()), QueryError::InvalidParameter { parameter: "temporal_clause".into(), reason: "invalid temporal clause: AS OF".into() }),
            (SqlError::InvalidTimestamp("yesterday".into()), QueryError::InvalidParameter { parameter: "timestamp".into(), reason: "invalid timestamp: yesterday".into() }),
            (SqlError::MissingClause("FROM".into()), QueryError::InvalidParameter { parameter: "clause".into(), reason: "missing required clause: FROM".into() }),
            (SqlError::TypeError("float".into()), QueryError::TypeMismatch { expected: "compatible SQL type".into(), actual: "float".into() }),
            (SqlError::ParameterError("bad param".into()), QueryError::InvalidParameter { parameter: "parameter".into(), reason: "bad param".into() }),
        ];

        for (sql_err, expected_query_err) in cases {
            let e: Error = sql_err.into();
            assert!(matches!(e, Error::Query(q) if q == expected_query_err), "failed for {:?}", expected_query_err);
        }
    }
}
