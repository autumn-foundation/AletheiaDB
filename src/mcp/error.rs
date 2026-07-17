//! Structured MCP error codes with a retriable flag (Issue #3234).
//!
//! Every MCP tool error response is a JSON object of the shape:
//!
//! ```json
//! {
//!   "error": {
//!     "code": "NOT_FOUND",
//!     "message": "Storage error: Node not found: Node(123)",
//!     "retriable": false,
//!     "details": { "...": "optional, structured, per-code metadata" }
//!   }
//! }
//! ```
//!
//! - `code` is drawn from the small, stable [`McpErrorCode`] enum, modeled on
//!   gRPC's canonical codes. Clients branch on `code`, never on `message`.
//! - `message` preserves the human-readable free text previously returned as
//!   the whole `error` value (additive change — no information lost).
//! - `retriable` is an explicit, advisory server-side classification: `true`
//!   only for transient failure classes (timeouts, clock skew, serialization
//!   conflicts), `false` for caller-fault classes (not-found,
//!   invalid-argument, constraint violations). The client owns the retry
//!   loop; the server never retries on its behalf.
//! - `details` carries optional structured metadata (e.g. the DETACH-delete
//!   refusal's `connected_edges`, a unique violation's `existing_node_id`).
//!
//! The classification from AletheiaDB's internal [`Error`] taxonomy to MCP
//! codes happens only at this boundary — the library's `thiserror` enums are
//! unchanged.

use crate::core::error::{
    ConstraintError, Error, QueryError, StorageError, TemporalError, TransactionError, VectorError,
};

/// Stable, machine-readable error codes for the MCP surface.
///
/// The wire representation ([`McpErrorCode::as_str`]) is SCREAMING_SNAKE_CASE
/// and is a **stability contract**: codes may be added, but existing codes
/// never change meaning or spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpErrorCode {
    /// The referenced entity (node, edge, version, tool, ...) does not exist,
    /// or did not exist at the requested bi-temporal coordinate.
    NotFound,
    /// The request itself is malformed: bad JSON arguments, an out-of-range
    /// ID, an unparseable timestamp, inconsistent parameter combinations, or
    /// an invalid query statement. Fix the arguments before retrying.
    InvalidArgument,
    /// A declared uniqueness constraint rejected the write.
    ConstraintViolation,
    /// The operation is valid but the system is not in the required state:
    /// e.g. a vector index is not enabled, a node still has connected edges
    /// and `detach` was not passed, or a required build feature is not
    /// compiled in. Change the state (or the call), then retry.
    FailedPrecondition,
    /// A concurrency conflict: serialization failure, write-write conflict,
    /// aborted transaction, or duplicate ID race. Usually retriable.
    Conflict,
    /// A transient condition: query timeout, clock skew, or other
    /// should-heal-itself failure. Retriable.
    Unavailable,
    /// An unexpected internal failure (I/O, corruption, poisoned lock,
    /// serialization of a response, ...). Not retriable; report it.
    Internal,
    /// The caller is not authenticated (Issue #3350): the surface requires a
    /// credential and none was supplied, or the supplied credential is
    /// unknown or revoked. Deliberately uniform across all of those cases —
    /// the error never reveals whether a presented key exists and never
    /// echoes credential material. Not retriable as-is: obtain a valid
    /// credential, then retry.
    Unauthenticated,
    /// The caller is authenticated but their role does not permit the
    /// operation's access class (Issue #3350). `details` carries
    /// `{required_class, principal_role}`. Not retriable: the same call
    /// under the same principal can never succeed.
    PermissionDenied,
    /// A per-query resource limit was exceeded (Issue #3368): the wall-clock
    /// timeout elapsed, or the result exceeded the response-byte cap.
    /// `details` carries `{dimension, limit, consumed?}`. Retriability is
    /// case-specific and set explicitly (a read-only wall-clock timeout is
    /// retriable; a byte-cap breach is not), so the default is the
    /// conservative non-retriable — treat an unqualified `RESOURCE_EXHAUSTED`
    /// as non-retriable.
    ResourceExhausted,
}

impl McpErrorCode {
    /// The stable wire representation of this code.
    pub fn as_str(self) -> &'static str {
        match self {
            McpErrorCode::NotFound => "NOT_FOUND",
            McpErrorCode::InvalidArgument => "INVALID_ARGUMENT",
            McpErrorCode::ConstraintViolation => "CONSTRAINT_VIOLATION",
            McpErrorCode::FailedPrecondition => "FAILED_PRECONDITION",
            McpErrorCode::Conflict => "CONFLICT",
            McpErrorCode::Unavailable => "UNAVAILABLE",
            McpErrorCode::Internal => "INTERNAL",
            McpErrorCode::Unauthenticated => "UNAUTHENTICATED",
            McpErrorCode::PermissionDenied => "PERMISSION_DENIED",
            McpErrorCode::ResourceExhausted => "RESOURCE_EXHAUSTED",
        }
    }

    /// Default retriable classification for this code.
    ///
    /// `Conflict` and `Unavailable` are transient by default; every
    /// caller-fault and internal class defaults to non-retriable. Individual
    /// errors may override (e.g. a `DuplicateId` maps to `Conflict` but is
    /// *not* retriable, because retrying the same ID cannot succeed).
    pub fn default_retriable(self) -> bool {
        matches!(self, McpErrorCode::Conflict | McpErrorCode::Unavailable)
    }
}

impl std::fmt::Display for McpErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A structured MCP error payload: code + message + retriable (+ details).
#[derive(Debug, Clone)]
pub struct McpError {
    code: McpErrorCode,
    message: String,
    retriable: bool,
    details: Option<serde_json::Value>,
}

impl McpError {
    /// Build an error with the code's default retriable classification.
    pub fn new(code: McpErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retriable: code.default_retriable(),
            details: None,
        }
    }

    /// Override the retriable flag (e.g. non-retriable `Conflict`).
    pub fn retriable(mut self, retriable: bool) -> Self {
        self.retriable = retriable;
        self
    }

    /// Attach structured, per-code metadata under `error.details`.
    pub fn details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// Replace the message (e.g. to add call-site context around the
    /// classified error's own text) without changing the classification.
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    /// The code assigned to this error.
    pub fn code(&self) -> McpErrorCode {
        self.code
    }

    /// Whether this error is advisorily retriable.
    pub fn is_retriable(&self) -> bool {
        self.retriable
    }

    /// The human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Serialize the inner error object (the value of the `"error"` key).
    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("code".to_string(), self.code.as_str().into());
        obj.insert("message".to_string(), self.message.clone().into());
        obj.insert("retriable".to_string(), self.retriable.into());
        if let Some(details) = &self.details {
            obj.insert("details".to_string(), details.clone());
        }
        serde_json::Value::Object(obj)
    }

    /// Classify an internal database [`Error`] into an MCP error.
    ///
    /// This is the single boundary mapping from the library's `thiserror`
    /// taxonomy to the stable MCP codes; the message is always
    /// `e.to_string()`, preserving the pre-#3234 free text verbatim.
    pub fn from_db_error(e: &Error) -> Self {
        let (code, retriable) = classify_db_error(e);
        Self {
            code,
            message: e.to_string(),
            retriable,
            details: None,
        }
    }
}

/// Map an internal error to `(code, retriable)`.
///
/// `retriable` is `true` **only** for transient classes: concurrency
/// conflicts that a fresh attempt can win (serialization failures, write
/// conflicts, aborted transactions) and self-healing conditions (timeouts,
/// clock skew). Caller-fault classes (not-found, invalid-argument,
/// constraint, failed-precondition) and persistent internal failures
/// (corruption, poisoned locks, I/O) are never retriable.
fn classify_db_error(e: &Error) -> (McpErrorCode, bool) {
    match e {
        Error::Storage(se) => classify_storage_error(se),
        Error::Temporal(te) => classify_temporal_error(te),
        Error::Query(qe) => classify_query_error(qe),
        Error::Transaction(te) => classify_transaction_error(te),
        Error::Vector(ve) => classify_vector_error(ve),
        Error::Constraint(ce) => classify_constraint_error(ce),
        // A provenance bundle failing validation is a caller fault.
        Error::Provenance(_) => (McpErrorCode::InvalidArgument, false),
        Error::Lineage(le) => classify_lineage_error(le),
        Error::Namespace(ne) => classify_namespace_error(ne),
        // A PITR (#3374) target outside the achievable window, or a window that
        // crosses a post-backup vocabulary change, is a caller-fault precondition
        // failure; each error's Display explains the remediation. All other
        // backup errors remain Internal.
        Error::Backup(
            crate::storage::backup::BackupError::TargetOutsideWindow { .. }
            | crate::storage::backup::BackupError::WindowCrossesVocabularyChange { .. },
        ) => (McpErrorCode::FailedPrecondition, false),
        Error::Io(_) | Error::Backup(_) => (McpErrorCode::Internal, false),
        // The feature exists but this build/deployment doesn't provide it.
        Error::NotImplemented { .. } => (McpErrorCode::FailedPrecondition, false),
        // An opt-in feature is disabled (e.g. the provenance hash chain).
        Error::FailedPrecondition(_) => (McpErrorCode::FailedPrecondition, false),
        Error::Other(_) => (McpErrorCode::Internal, false),
    }
}

/// Map a derivation-lineage error (Issue #3371). All lineage errors are
/// caller faults and never retriable: a dangling source reference is
/// `NOT_FOUND`, a self/cyclic declaration is `INVALID_ARGUMENT`, and
/// re-declaring lineage for an already-recorded version is a
/// `FAILED_PRECONDITION`.
fn classify_lineage_error(e: &crate::core::lineage::LineageError) -> (McpErrorCode, bool) {
    use crate::core::lineage::LineageError;
    match e {
        LineageError::SourceNotFound { .. } => (McpErrorCode::NotFound, false),
        LineageError::SelfDerivation(_) | LineageError::CycleDetected { .. } => {
            (McpErrorCode::InvalidArgument, false)
        }
        LineageError::AlreadyRecorded(_) => (McpErrorCode::FailedPrecondition, false),
    }
}

/// Map a namespace error (Issue #3349). All are caller faults and never
/// retriable: an invalid name or a forged engine-reserved property key is
/// `INVALID_ARGUMENT`, an unknown namespace is `NOT_FOUND`, and a duplicate
/// `create_namespace` is `CONFLICT`.
fn classify_namespace_error(e: &crate::core::namespace::NamespaceError) -> (McpErrorCode, bool) {
    use crate::core::namespace::NamespaceError;
    match e {
        NamespaceError::InvalidName { .. }
        | NamespaceError::ReservedPropertyKey { .. }
        | NamespaceError::Immutable => (McpErrorCode::InvalidArgument, false),
        NamespaceError::NotFound { .. } => (McpErrorCode::NotFound, false),
        NamespaceError::AlreadyExists { .. } => (McpErrorCode::Conflict, false),
    }
}

fn classify_storage_error(e: &StorageError) -> (McpErrorCode, bool) {
    match e {
        StorageError::NodeNotFound(_)
        | StorageError::EdgeNotFound(_)
        | StorageError::VersionNotFound(_)
        | StorageError::PropertyNotFound(_) => (McpErrorCode::NotFound, false),
        StorageError::InvalidId { .. } | StorageError::InvalidProperty { .. } => {
            (McpErrorCode::InvalidArgument, false)
        }
        // A duplicate ID is a conflict, but retrying the identical request
        // cannot succeed — override the code's default retriability.
        StorageError::DuplicateId { .. } => (McpErrorCode::Conflict, false),
        // Resource limits (DoS protection): the request is well-formed but
        // the system refuses in its current state.
        StorageError::CapacityExceeded { .. } => (McpErrorCode::FailedPrecondition, false),
        StorageError::InconsistentState { .. }
        | StorageError::WalError { .. }
        | StorageError::CheckpointError { .. }
        | StorageError::IoError(_)
        | StorageError::CorruptedData(_)
        | StorageError::PersistenceErrorWithKind { .. }
        | StorageError::PersistenceError(_)
        | StorageError::LockPoisoned { .. }
        | StorageError::Encryption(_)
        | StorageError::KeyProvider(_) => (McpErrorCode::Internal, false),
    }
}

fn classify_temporal_error(e: &TemporalError) -> (McpErrorCode, bool) {
    match e {
        TemporalError::NodeNotFoundAtTime { .. } | TemporalError::VersionNotFound(_) => {
            (McpErrorCode::NotFound, false)
        }
        TemporalError::InvalidTimeRange { .. }
        | TemporalError::InvalidTimestamp { .. }
        | TemporalError::ValidTimeBeforeCreation { .. }
        | TemporalError::ValidTimeTooFarInFuture { .. }
        | TemporalError::ValidTimeBeforeEntityCreation { .. } => {
            (McpErrorCode::InvalidArgument, false)
        }
        TemporalError::VersionAlreadyClosed { .. } => (McpErrorCode::FailedPrecondition, false),
        // Clock-adjacent hiccups heal on their own; a fresh attempt at a
        // later wallclock/logical tick can succeed.
        TemporalError::NonMonotonicTransactionTime { .. }
        | TemporalError::LogicalCounterOverflow { .. } => (McpErrorCode::Unavailable, true),
        // Despite its caller-fault-sounding name, `TemporalParadox` is only
        // raised when the system clock reads before the Unix epoch — a
        // server-environment fault, not something the caller can repair.
        TemporalError::TemporalParadox { .. }
        | TemporalError::CorruptedVersionChain { .. }
        | TemporalError::MissingAnchor { .. }
        | TemporalError::MaxDepthExceeded { .. } => (McpErrorCode::Internal, false),
    }
}

/// Classify a [`ConstraintError`] per variant.
///
/// Only a `UniqueViolation` is a true `CONSTRAINT_VIOLATION` (a declared
/// constraint rejecting *this* write). `UnsupportedKeyType` is a caller
/// fault in the enable request itself, and `DuplicateOnEnable` is a state
/// problem — duplicates already exist in the graph, so the caller must fix
/// the data (or drop the request) before the enable can succeed.
fn classify_constraint_error(e: &ConstraintError) -> (McpErrorCode, bool) {
    match e {
        ConstraintError::UniqueViolation { .. } => (McpErrorCode::ConstraintViolation, false),
        ConstraintError::UnsupportedKeyType { .. } => (McpErrorCode::InvalidArgument, false),
        ConstraintError::DuplicateOnEnable { .. } => (McpErrorCode::FailedPrecondition, false),
        // Schema constraints (Issue #3378). A type/required-key violation on a
        // write is a declared constraint rejecting *this* write
        // (CONSTRAINT_VIOLATION); non-conformance on enable is a state problem
        // the caller must fix first (FAILED_PRECONDITION). All non-retriable.
        ConstraintError::TypeViolation { .. } => (McpErrorCode::ConstraintViolation, false),
        ConstraintError::MissingRequiredKey { .. } => (McpErrorCode::ConstraintViolation, false),
        ConstraintError::NonConformingOnEnable { .. } => (McpErrorCode::FailedPrecondition, false),
    }
}

fn classify_query_error(e: &QueryError) -> (McpErrorCode, bool) {
    match e {
        QueryError::SyntaxError { .. }
        | QueryError::UnsupportedFeature { .. }
        | QueryError::InvalidParameter { .. }
        | QueryError::LimitExceeded { .. }
        | QueryError::InvalidTraversal { .. }
        | QueryError::TypeMismatch { .. } => (McpErrorCode::InvalidArgument, false),
        QueryError::Timeout { .. } => (McpErrorCode::Unavailable, true),
        QueryError::IndexNotFound { .. } => (McpErrorCode::FailedPrecondition, false),
        QueryError::ExecutionError { .. } => (McpErrorCode::Internal, false),
    }
}

fn classify_transaction_error(e: &TransactionError) -> (McpErrorCode, bool) {
    match e {
        TransactionError::SerializationFailure { .. }
        | TransactionError::WriteConflict { .. }
        | TransactionError::Aborted { .. } => (McpErrorCode::Conflict, true),
        TransactionError::InvalidState { .. }
        | TransactionError::AlreadyCommitted { .. }
        | TransactionError::ValidationFailed { .. }
        // A lost compare-and-set (Issue #3577) is a caller-fault precondition
        // failure: retrying the same call with the same stale `expected` version
        // can never succeed, so it is NON-retriable (unlike SerializationFailure).
        | TransactionError::CasMismatch { .. } => (McpErrorCode::FailedPrecondition, false),
        TransactionError::ClockSkew { .. } => (McpErrorCode::Unavailable, true),
        TransactionError::CommitFailed { .. }
        | TransactionError::RollbackFailed { .. }
        | TransactionError::LockPoisoned { .. } => (McpErrorCode::Internal, false),
    }
}

fn classify_vector_error(e: &VectorError) -> (McpErrorCode, bool) {
    match e {
        VectorError::NotFound { .. } => (McpErrorCode::NotFound, false),
        VectorError::DimensionMismatch { .. }
        | VectorError::ContainsNaN { .. }
        | VectorError::ContainsInfinity { .. }
        | VectorError::DimensionTooLarge { .. }
        | VectorError::IndexOutOfBounds { .. }
        | VectorError::InvalidVector { .. }
        | VectorError::InvalidSparseVector { .. } => (McpErrorCode::InvalidArgument, false),
        VectorError::IndexError(_) => (McpErrorCode::Internal, false),
    }
}

/// Map a `query` tool error `kind` (Issue #3213) to `(code, retriable)`.
///
/// The query tool keeps its own `kind` field verbatim (its published
/// contract); `code`/`retriable` are added additively so the tool also
/// satisfies the uniform #3234 contract. Every kind is a caller-fixable
/// request problem except `language_unavailable` (a build/deployment
/// precondition) and `runtime_error` (engine-side, classified from the
/// underlying error where available).
pub(crate) fn query_kind_classification(kind: &str) -> (McpErrorCode, bool) {
    match kind {
        "invalid_request"
        | "read_only_violation"
        | "parse_error"
        | "unsupported_construct"
        | "invalid_params" => (McpErrorCode::InvalidArgument, false),
        "language_unavailable" => (McpErrorCode::FailedPrecondition, false),
        "runtime_error" => (McpErrorCode::Internal, false),
        // Fail-safe: a future kind added without updating this mapping is
        // reported as INTERNAL (non-retriable) rather than silently blamed
        // on the caller as INVALID_ARGUMENT.
        _ => (McpErrorCode::Internal, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::ConstraintError;
    use crate::core::id::NodeId;

    #[test]
    fn code_wire_representation_is_stable() {
        assert_eq!(McpErrorCode::NotFound.as_str(), "NOT_FOUND");
        assert_eq!(McpErrorCode::InvalidArgument.as_str(), "INVALID_ARGUMENT");
        assert_eq!(
            McpErrorCode::ConstraintViolation.as_str(),
            "CONSTRAINT_VIOLATION"
        );
        assert_eq!(
            McpErrorCode::FailedPrecondition.as_str(),
            "FAILED_PRECONDITION"
        );
        assert_eq!(McpErrorCode::Conflict.as_str(), "CONFLICT");
        assert_eq!(McpErrorCode::Unavailable.as_str(), "UNAVAILABLE");
        assert_eq!(McpErrorCode::Internal.as_str(), "INTERNAL");
        assert_eq!(McpErrorCode::Unauthenticated.as_str(), "UNAUTHENTICATED");
        assert_eq!(McpErrorCode::PermissionDenied.as_str(), "PERMISSION_DENIED");
        assert_eq!(
            McpErrorCode::ResourceExhausted.as_str(),
            "RESOURCE_EXHAUSTED"
        );
    }

    #[test]
    fn resource_exhausted_defaults_non_retriable() {
        // The default is the conservative non-retriable; the read-only
        // wall-clock-timeout case opts into retriable at its call site (see
        // `server::wall_clock_timeout_error`), the byte-cap case does not.
        assert!(!McpErrorCode::ResourceExhausted.default_retriable());
    }

    #[test]
    fn auth_codes_are_never_retriable_by_default() {
        assert!(!McpErrorCode::Unauthenticated.default_retriable());
        assert!(!McpErrorCode::PermissionDenied.default_retriable());
    }

    #[test]
    fn to_json_carries_code_message_retriable_and_optional_details() {
        let plain = McpError::new(McpErrorCode::NotFound, "Node not found: Node(1)");
        let json = plain.to_json();
        assert_eq!(json["code"], "NOT_FOUND");
        assert_eq!(json["message"], "Node not found: Node(1)");
        assert_eq!(json["retriable"], false);
        assert!(json.get("details").is_none(), "details omitted when unset");

        let with_details = McpError::new(McpErrorCode::FailedPrecondition, "refused")
            .details(serde_json::json!({"connected_edges": 3}));
        let json = with_details.to_json();
        assert_eq!(json["details"]["connected_edges"], 3);
    }

    #[test]
    fn retriable_true_only_for_transient_classes() {
        // Transient: serialization failure, write conflict, timeout, clock skew.
        let transient: Vec<Error> = vec![
            TransactionError::SerializationFailure {
                entity: "node-1".into(),
                reason: "concurrent commit".into(),
            }
            .into(),
            TransactionError::WriteConflict {
                entity_id: "node-1".into(),
                reason: "concurrent modification".into(),
            }
            .into(),
            TransactionError::Aborted { tx_id: 7 }.into(),
            TransactionError::ClockSkew {
                wallclock: 1_000,
                previous: 2_000,
                drift_us: -1_000,
                max_allowed: 500,
            }
            .into(),
            QueryError::Timeout { duration_ms: 5000 }.into(),
        ];
        for e in &transient {
            let err = McpError::from_db_error(e);
            assert!(err.is_retriable(), "expected retriable for {e}");
        }

        // Caller-fault / persistent: never retriable.
        let not_retriable: Vec<Error> = vec![
            StorageError::NodeNotFound(NodeId::new(1).unwrap()).into(),
            StorageError::InvalidId {
                id: u64::MAX,
                id_type: "node",
            }
            .into(),
            StorageError::DuplicateId {
                id: "1".into(),
                kind: "node".into(),
            }
            .into(),
            StorageError::LockPoisoned {
                resource: "wal".into(),
            }
            .into(),
            ConstraintError::UniqueViolation {
                label: "Person".into(),
                property: "email".into(),
                value: "a".into(),
                existing_node_id: NodeId::new(1).unwrap(),
            }
            .into(),
            Error::not_implemented("feature", "reason"),
            Error::Io(std::io::Error::other("disk on fire")),
        ];
        for e in &not_retriable {
            let err = McpError::from_db_error(e);
            assert!(!err.is_retriable(), "expected non-retriable for {e}");
        }
    }

    #[test]
    fn classification_covers_expected_codes() {
        let cases: Vec<(Error, McpErrorCode)> = vec![
            (
                StorageError::NodeNotFound(NodeId::new(1).unwrap()).into(),
                McpErrorCode::NotFound,
            ),
            (
                TemporalError::NodeNotFoundAtTime {
                    node_id: NodeId::new(1).unwrap(),
                    valid_time: 1_000.into(),
                    transaction_time: 2_000.into(),
                }
                .into(),
                McpErrorCode::NotFound,
            ),
            (
                StorageError::InvalidId {
                    id: u64::MAX,
                    id_type: "node",
                }
                .into(),
                McpErrorCode::InvalidArgument,
            ),
            (
                TemporalError::ValidTimeTooFarInFuture {
                    valid_from: 2_000.into(),
                    current_time: 1_000.into(),
                    max_future_offset_us: 1,
                }
                .into(),
                McpErrorCode::InvalidArgument,
            ),
            (
                ConstraintError::UniqueViolation {
                    label: "Person".into(),
                    property: "email".into(),
                    value: "a".into(),
                    existing_node_id: NodeId::new(1).unwrap(),
                }
                .into(),
                McpErrorCode::ConstraintViolation,
            ),
            (
                QueryError::IndexNotFound {
                    index_type: "vector".into(),
                    property_name: "embedding".into(),
                    hint: None,
                }
                .into(),
                McpErrorCode::FailedPrecondition,
            ),
            (
                TransactionError::SerializationFailure {
                    entity: "n".into(),
                    reason: "r".into(),
                }
                .into(),
                McpErrorCode::Conflict,
            ),
            (
                QueryError::Timeout { duration_ms: 1 }.into(),
                McpErrorCode::Unavailable,
            ),
            (
                StorageError::CorruptedData("bad checksum".into()).into(),
                McpErrorCode::Internal,
            ),
            // TemporalParadox's only raise site is a system-clock-before-epoch
            // fault — a server-environment problem, not a caller fault.
            (
                TemporalError::TemporalParadox {
                    reason: "System clock is before Unix epoch".into(),
                }
                .into(),
                McpErrorCode::Internal,
            ),
            // An unsupported constraint key type is a fault in the request.
            (
                ConstraintError::UnsupportedKeyType {
                    label: "Person".into(),
                    property: "avatar".into(),
                    type_name: "Vector".into(),
                }
                .into(),
                McpErrorCode::InvalidArgument,
            ),
            // Enabling over existing duplicates is a state problem: fix the
            // data, then re-issue the enable.
            (
                ConstraintError::DuplicateOnEnable {
                    label: "Person".into(),
                    property: "email".into(),
                    value: "dup".into(),
                    node_ids: vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()],
                }
                .into(),
                McpErrorCode::FailedPrecondition,
            ),
            (
                VectorError::DimensionMismatch {
                    expected: 4,
                    actual: 3,
                }
                .into(),
                McpErrorCode::InvalidArgument,
            ),
        ];
        for (e, expected) in &cases {
            let err = McpError::from_db_error(e);
            assert_eq!(err.code(), *expected, "wrong code for {e}");
            assert_eq!(
                err.message(),
                e.to_string(),
                "free text must be preserved verbatim as message"
            );
        }
    }

    #[test]
    fn query_kind_classification_matches_contract() {
        assert_eq!(
            query_kind_classification("invalid_request"),
            (McpErrorCode::InvalidArgument, false)
        );
        assert_eq!(
            query_kind_classification("read_only_violation"),
            (McpErrorCode::InvalidArgument, false)
        );
        assert_eq!(
            query_kind_classification("parse_error"),
            (McpErrorCode::InvalidArgument, false)
        );
        assert_eq!(
            query_kind_classification("unsupported_construct"),
            (McpErrorCode::InvalidArgument, false)
        );
        assert_eq!(
            query_kind_classification("invalid_params"),
            (McpErrorCode::InvalidArgument, false)
        );
        assert_eq!(
            query_kind_classification("language_unavailable"),
            (McpErrorCode::FailedPrecondition, false)
        );
        assert_eq!(
            query_kind_classification("runtime_error"),
            (McpErrorCode::Internal, false)
        );
    }

    #[test]
    fn schema_constraint_errors_classify_per_contract() {
        // Issue #3378: type/required-key violations are CONSTRAINT_VIOLATION,
        // non-conformance on enable is FAILED_PRECONDITION; all non-retriable.
        let type_violation = ConstraintError::TypeViolation {
            entity_kind: "node".into(),
            label: "Person".into(),
            property: "age".into(),
            expected_type: "int",
            actual_type: "string",
        };
        let (code, retriable) = classify_constraint_error(&type_violation);
        assert_eq!(code, McpErrorCode::ConstraintViolation);
        assert!(!retriable);

        let missing = ConstraintError::MissingRequiredKey {
            entity_kind: "edge".into(),
            label: "KNOWS".into(),
            missing_keys: vec!["since".into()],
        };
        let (code, retriable) = classify_constraint_error(&missing);
        assert_eq!(code, McpErrorCode::ConstraintViolation);
        assert!(!retriable);

        let non_conforming = ConstraintError::NonConformingOnEnable {
            entity_kind: "node".into(),
            label: "Person".into(),
            violations: vec![],
            total_non_conforming: 3,
            sample_ids: vec![1, 2, 3],
        };
        let (code, retriable) = classify_constraint_error(&non_conforming);
        assert_eq!(code, McpErrorCode::FailedPrecondition);
        assert!(!retriable);
    }

    #[test]
    fn query_kind_classification_unknown_kind_is_internal() {
        // Fail-safe: a kind added in the future without updating the mapping
        // must surface as INTERNAL, never be blamed on the caller.
        assert_eq!(
            query_kind_classification("some_future_kind"),
            (McpErrorCode::Internal, false)
        );
    }

    #[test]
    fn retriable_true_serializes_through_to_json() {
        // Transient classes must carry `retriable: true` all the way through
        // the JSON serialization path, not just in the in-memory struct.
        let timeout = McpError::from_db_error(&QueryError::Timeout { duration_ms: 5000 }.into());
        let json = timeout.to_json();
        assert_eq!(json["code"], "UNAVAILABLE");
        assert_eq!(json["retriable"], true);

        let serialization = McpError::from_db_error(
            &TransactionError::SerializationFailure {
                entity: "node-1".into(),
                reason: "concurrent commit".into(),
            }
            .into(),
        );
        let json = serialization.to_json();
        assert_eq!(json["code"], "CONFLICT");
        assert_eq!(json["retriable"], true);
    }
}
