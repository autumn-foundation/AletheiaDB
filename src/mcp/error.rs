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

    /// Merge a single `key`/`value` into the existing `error.details` object,
    /// preserving any structured details already present (e.g. the
    /// `{resource, current, limit}` a `from_db_error` capacity error carries).
    ///
    /// If no details object exists yet — or the existing details is not a JSON
    /// object — a fresh single-key object is created. This is the additive
    /// counterpart to [`Self::details`] (which replaces): call sites that need
    /// to enrich a classified error with call-site metadata (e.g. the batch
    /// surface's `failed_op_index`) use this so the classifier's own details are
    /// not clobbered.
    pub fn with_detail(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        match &mut self.details {
            Some(serde_json::Value::Object(map)) => {
                map.insert(key.into(), value);
            }
            _ => {
                let mut map = serde_json::Map::new();
                map.insert(key.into(), value);
                self.details = Some(serde_json::Value::Object(map));
            }
        }
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
    ///
    /// Schema-constraint violations (Issue #3378) additionally carry their
    /// structured `details` here — `TypeViolation`'s `expected_type`/
    /// `actual_type`, `MissingRequiredKey`'s `missing_keys`, and
    /// `NonConformingOnEnable`'s bounded violation report — so a caller can
    /// self-repair without parsing the free-text `message`. This mirrors the
    /// `UniqueViolation` precedent (whose `existing_node_id` details are built
    /// at the `server::db_error` call site, along with its legacy top-level
    /// fields); that path is unchanged because
    /// [`ConstraintError::structured_details`] returns `None` for it.
    pub fn from_db_error(e: &Error) -> Self {
        let (code, retriable) = classify_db_error(e);
        let details = e
            .as_constraint()
            .and_then(|ce| ce.structured_details())
            .or_else(|| principal_quota_details(e))
            .or_else(|| tenant_quota_details(e))
            .or_else(|| namespace_details(e))
            .or_else(|| capacity_exceeded_details(e))
            .or_else(|| read_only_replica_details(e));
        // A string-interner capacity exhaustion (configurable interner cap) gets
        // an actionable message naming the knob and the restart requirement, so a
        // caller/LLM can resolve it without parsing the raw Display text.
        let message = interner_capacity_message(e).unwrap_or_else(|| e.to_string());
        Self {
            code,
            message,
            retriable,
            details,
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
        Error::Tenant(te) => classify_tenant_error(te),
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
        | NamespaceError::Immutable
        | NamespaceError::ScopeConflict => (McpErrorCode::InvalidArgument, false),
        NamespaceError::NotFound { .. } => (McpErrorCode::NotFound, false),
        NamespaceError::AlreadyExists { .. } => (McpErrorCode::Conflict, false),
    }
}

/// Map a tenant error (Issue #3365). All are caller faults and never retriable:
/// a malformed id is `INVALID_ARGUMENT`, an unknown tenant is `NOT_FOUND`, a
/// duplicate create is `CONFLICT`, and a quota breach is `RESOURCE_EXHAUSTED`.
/// A quota is a hard capacity limit — it heals only by freeing data or raising
/// the quota, never by retrying the same call — so it is deliberately
/// non-retriable (unlike the transient per-principal changefeed quota of #3678).
fn classify_tenant_error(e: &crate::core::tenant::TenantError) -> (McpErrorCode, bool) {
    use crate::core::tenant::TenantError;
    match e {
        TenantError::InvalidId { .. } => (McpErrorCode::InvalidArgument, false),
        TenantError::NotFound { .. } => (McpErrorCode::NotFound, false),
        TenantError::AlreadyExists { .. } => (McpErrorCode::Conflict, false),
        TenantError::QuotaExceeded { .. } => (McpErrorCode::ResourceExhausted, false),
    }
}

/// Structured `details` for a tenant quota breach (Issue #3365):
/// `{tenant, dimension, current, limit}`. Returns `None` for every other error.
fn tenant_quota_details(e: &Error) -> Option<serde_json::Value> {
    if let Error::Tenant(crate::core::tenant::TenantError::QuotaExceeded {
        tenant,
        dimension,
        current,
        limit,
    }) = e
    {
        Some(serde_json::json!({
            "tenant": tenant,
            "dimension": dimension.as_str(),
            "current": current,
            "limit": limit,
        }))
    } else {
        None
    }
}

/// Structured `details` for a per-principal changefeed quota breach (Issue
/// #3678): `{principal, current, limit}`. Shared by the MCP and HTTP surfaces so
/// both render byte-identical metadata under `error.details`. Returns `None` for
/// every other error.
fn principal_quota_details(e: &Error) -> Option<serde_json::Value> {
    if let Error::Storage(StorageError::PrincipalQuotaExceeded {
        principal,
        current,
        limit,
    }) = e
    {
        Some(serde_json::json!({
            "principal": principal,
            "current": current,
            "limit": limit,
        }))
    } else {
        None
    }
}

/// Structured `details` for a read-only-replica write rejection (Issue
/// #3355): `{node_role: "replica", reason: "read_only_replica"}`. Shared by
/// the MCP and HTTP surfaces so both render byte-identical metadata under
/// `error.details`. This is the leak-through path (a db error surfacing
/// through a handler that classifies via [`McpError::from_db_error`] instead
/// of the dedicated dispatch-seam check in `dispatch_tool`); it carries the
/// same details either way. Returns `None` for every other error.
fn read_only_replica_details(e: &Error) -> Option<serde_json::Value> {
    if matches!(e, Error::Transaction(TransactionError::ReadOnlyReplica)) {
        Some(serde_json::json!({
            "node_role": "replica",
            "reason": "read_only_replica",
        }))
    } else {
        None
    }
}

/// Structured `details` for a storage capacity exhaustion (configurable
/// interner cap): `{resource, current, limit}`. Shared by the MCP and HTTP
/// surfaces so both render byte-identical metadata under `error.details`.
/// Returns `None` for every other error.
fn capacity_exceeded_details(e: &Error) -> Option<serde_json::Value> {
    if let Error::Storage(StorageError::CapacityExceeded {
        resource,
        current,
        limit,
    }) = e
    {
        Some(serde_json::json!({
            "resource": resource,
            "current": current,
            "limit": limit,
        }))
    } else {
        None
    }
}

/// Actionable replacement message for a **string-interner** capacity exhaustion
/// (configurable interner cap). Names the `persistence.max_interned_strings`
/// knob and the restart requirement. Returns `None` for any other error
/// (including non-interner `CapacityExceeded` resources), preserving their
/// Display text.
fn interner_capacity_message(e: &Error) -> Option<String> {
    if let Error::Storage(StorageError::CapacityExceeded {
        resource,
        current,
        limit,
    }) = e
        && resource.contains("interner")
    {
        return Some(format!(
            "String interner at capacity ({current}/{limit}). Raise \
             `persistence.max_interned_strings` above {limit} and restart to intern more \
             unique strings. No data is lost — the WAL is the source of truth."
        ));
    }
    None
}

/// Structured `details` for a namespace error (Issue #3349). An unknown
/// namespace (`NOT_FOUND`) and a malformed / empty scope (`INVALID_ARGUMENT`)
/// both carry the offending `namespace` value so a caller can self-correct
/// without substring-parsing the message; the `InvalidName` case also carries
/// the validation `reason`. Shared by the MCP and HTTP surfaces so both render
/// byte-identical metadata under `error.details`. Returns `None` for every
/// other error.
fn namespace_details(e: &Error) -> Option<serde_json::Value> {
    use crate::core::namespace::NamespaceError;
    match e {
        Error::Namespace(NamespaceError::NotFound { namespace }) => {
            Some(serde_json::json!({ "namespace": namespace }))
        }
        Error::Namespace(NamespaceError::InvalidName { name, reason }) => {
            Some(serde_json::json!({ "namespace": name, "reason": reason }))
        }
        _ => None,
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
        // Enabling encryption on an already-encrypted WAL is a caller precondition
        // failure, not an internal fault (Issue #3616 PR3) — and retrying cannot
        // succeed, so it is non-retriable.
        StorageError::WalKeyringAlreadyInstalled { .. } => {
            (McpErrorCode::FailedPrecondition, false)
        }
        // Disabling encryption on an already-plaintext WAL is a caller precondition
        // failure, not an internal fault (Issue #3616 PR4) — the mirror of the
        // already-installed rejection above, and likewise non-retriable.
        StorageError::WalKeyringNotInstalled { .. } => (McpErrorCode::FailedPrecondition, false),
        // Enabling encryption on an already-encrypted index tier is a caller
        // precondition failure, not an internal fault (Issue #3708) — the
        // index-tier mirror of the WAL already-installed rejection, likewise
        // non-retriable.
        StorageError::IndexKeyringAlreadyInstalled { .. } => {
            (McpErrorCode::FailedPrecondition, false)
        }
        // Enabling encryption on an already-encrypted cold tier is a caller
        // precondition failure, not an internal fault (Issue #3708) — the
        // cold-tier mirror of the WAL/index already-installed rejections,
        // likewise non-retriable.
        StorageError::ColdKeyringAlreadyInstalled { .. } => {
            (McpErrorCode::FailedPrecondition, false)
        }
        // A pre-v13 (0.1.x) WAL tail refused on open (Issue #3746) is a caller
        // precondition failure — the operator must drain/checkpoint the WAL on
        // the old version before upgrading; retrying the same open cannot
        // succeed, so it is non-retriable.
        StorageError::PreV13WalTailRequiresMigration { .. } => {
            (McpErrorCode::FailedPrecondition, false)
        }
        // A per-principal changefeed quota breach (Issue #3678) is a transient
        // fairness limit: another of this principal's subscriptions may drop, so
        // retrying with backoff can succeed → RESOURCE_EXHAUSTED, retriable.
        StorageError::PrincipalQuotaExceeded { .. } => (McpErrorCode::ResourceExhausted, true),
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
        // Engine-lane per-query resource limit (Issue #3368 engine lane,
        // src/query/limits.rs). `retriable` is already dimension-correct
        // (true only for the wall-clock timeout) so it is threaded straight
        // through rather than re-derived.
        QueryError::ResourceExhausted { retriable, .. } => {
            (McpErrorCode::ResourceExhausted, *retriable)
        }
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
        // A fenced claim rejected for a too-low fence (DBOS Phase 3e) is the same
        // caller-fault, non-retriable precondition class (recompute the fence).
        | TransactionError::CasMismatch { .. }
        | TransactionError::FenceTooLow { .. } => (McpErrorCode::FailedPrecondition, false),
        TransactionError::ClockSkew { .. } => (McpErrorCode::Unavailable, true),
        TransactionError::CommitFailed { .. }
        | TransactionError::RollbackFailed { .. }
        | TransactionError::LockPoisoned { .. } => (McpErrorCode::Internal, false),
        // A write rejected because this node is a read-only replica (Issue
        // #3355): retrying the identical call against this node can never
        // succeed (the caller must redirect to the primary), so this is a
        // non-retriable precondition failure like the CAS/fence classes
        // above. The single MCP dispatch seam (`dispatch_tool`) classifies
        // write/admin-class tools before a handler ever runs, so this arm is
        // a defensive leak-through mapping rather than the primary
        // enforcement point.
        TransactionError::ReadOnlyReplica => (McpErrorCode::FailedPrecondition, false),
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
    fn test_wal_error_classification_is_non_retriable_internal() {
        // Issue #3798 AC11: the group-commit lock-acquisition failures (both the
        // suspected-deadlock timeout and the re-entrancy refusal) reuse
        // StorageError::WalError rather than adding a new StorageError variant,
        // so this classification IS their MCP contract.
        //
        // Why non-retriable, honestly stated. It is NOT one argument but two,
        // and they are not equally strong:
        //
        //   * The group-commit ACQUISITION timeout genuinely leaves durability
        //     UNKNOWN on the commit path — the transaction may still become
        //     durable and replay at recovery — so telling a client to retry
        //     would risk a double-applied transaction. Escalation is correct.
        //   * The bounded APPEND timeout (ring buffer full, #3798) is by
        //     contrast retry-SAFE: a mid-batch failure leaves a WAL prefix with
        //     no CommitTx marker, which recovery discards (#3413 framing), so
        //     the transaction never applied. On the merits it deserves
        //     UNAVAILABLE/retriable.
        //
        // It is classified non-retriable anyway because WalError is a blanket
        // arm covering both, and StorageError is not #[non_exhaustive], so
        // splitting it is a breaking change rather than a local edit. The
        // conservative mapping is deliberate — over-escalating a retriable
        // backpressure error costs a page; under-escalating a
        // durability-unknown one risks duplicate data. The dedicated retriable
        // variant is tracked as Issue #3800; until then any reclassification of
        // WalError has to break this test rather than silently flip the advice.
        let e: Error = crate::core::error::StorageError::WalError {
            reason: "Group commit lock acquisition timed out after 120000ms at \
                     group_commit_state site register_transaction (possible deadlock); \
                     durability status UNKNOWN"
                .to_string(),
        }
        .into();
        let json = McpError::from_db_error(&e).to_json();
        assert_eq!(json["code"], "INTERNAL");
        assert_eq!(json["retriable"], false);
    }

    #[test]
    fn principal_quota_breach_is_resource_exhausted_retriable_with_details() {
        // Issue #3678: a per-principal changefeed quota breach classifies to the
        // RESOURCE_EXHAUSTED envelope with retriable:true and structured
        // details {principal, current, limit}.
        let e: Error = crate::core::error::StorageError::PrincipalQuotaExceeded {
            principal: "alice".to_string(),
            current: 2,
            limit: 2,
        }
        .into();
        let err = McpError::from_db_error(&e);
        let json = err.to_json();
        assert_eq!(json["code"], "RESOURCE_EXHAUSTED");
        assert_eq!(json["retriable"], true);
        assert_eq!(json["details"]["principal"], "alice");
        assert_eq!(json["details"]["current"], 2);
        assert_eq!(json["details"]["limit"], 2);
    }

    #[test]
    fn tenant_errors_map_to_3234_codes() {
        use crate::core::tenant::{QuotaDimension, TenantError};
        // Quota breach → RESOURCE_EXHAUSTED, non-retriable, with structured
        // {tenant, dimension, current, limit} details (Issue #3365).
        let quota: Error = TenantError::QuotaExceeded {
            tenant: "acme".to_string(),
            dimension: QuotaDimension::Nodes,
            current: 100,
            limit: 100,
        }
        .into();
        let json = McpError::from_db_error(&quota).to_json();
        assert_eq!(json["code"], "RESOURCE_EXHAUSTED");
        assert_eq!(json["retriable"], false);
        assert_eq!(json["details"]["tenant"], "acme");
        assert_eq!(json["details"]["dimension"], "nodes");
        assert_eq!(json["details"]["current"], 100);
        assert_eq!(json["details"]["limit"], 100);

        // Lifecycle errors map to their respective codes.
        let not_found: Error = TenantError::NotFound {
            id: "ghost".to_string(),
        }
        .into();
        assert_eq!(
            McpError::from_db_error(&not_found).to_json()["code"],
            "NOT_FOUND"
        );

        let exists: Error = TenantError::AlreadyExists {
            id: "dup".to_string(),
        }
        .into();
        assert_eq!(
            McpError::from_db_error(&exists).to_json()["code"],
            "CONFLICT"
        );

        let invalid: Error = TenantError::InvalidId {
            id: "bad/id".to_string(),
            reason: "x".to_string(),
        }
        .into();
        assert_eq!(
            McpError::from_db_error(&invalid).to_json()["code"],
            "INVALID_ARGUMENT"
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
    fn interner_capacity_exceeded_maps_to_actionable_failed_precondition() {
        // Configurable interner cap: a string-interner CapacityExceeded must
        // surface as a structured FAILED_PRECONDITION (non-retriable) whose
        // message names `persistence.max_interned_strings` and whose details
        // carry {resource, current, limit} — NOT a panic/INTERNAL.
        let e: Error = StorageError::CapacityExceeded {
            resource: "string interner".into(),
            current: 200,
            limit: 200,
        }
        .into();
        let err = McpError::from_db_error(&e);
        assert_eq!(err.code(), McpErrorCode::FailedPrecondition);
        assert!(!err.is_retriable());
        assert!(
            err.message().contains("persistence.max_interned_strings"),
            "message must name the knob, got: {}",
            err.message()
        );
        let json = err.to_json();
        assert_eq!(json["code"], "FAILED_PRECONDITION");
        assert_eq!(json["details"]["resource"], "string interner");
        assert_eq!(json["details"]["current"], 200);
        assert_eq!(json["details"]["limit"], 200);
    }

    #[test]
    fn pre_v13_wal_tail_maps_to_non_retriable_failed_precondition() {
        // Issue #3746: a refused pre-v13 WAL tail is a caller precondition
        // failure (drain/checkpoint on the old version first), never retriable.
        let e = StorageError::PreV13WalTailRequiresMigration {
            reason: "unreplayed pre-v13 tail; see docs/guides/migration-0.1-to-0.2.md".into(),
        };
        assert_eq!(
            classify_storage_error(&e),
            (McpErrorCode::FailedPrecondition, false)
        );
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
    fn keyring_already_installed_errors_classify_as_failed_precondition() {
        // Issue #3708: the index and cold runtime keyring-install seams reject a
        // double-install with a distinguishable `*AlreadyInstalled` variant, each
        // of which must classify to FAILED_PRECONDITION (non-retriable): a second
        // install is a caller-fault precondition breach, never a transient retry.
        let index_err = StorageError::IndexKeyringAlreadyInstalled {
            reason: "None -> Some transition only".to_string(),
        };
        assert_eq!(
            classify_storage_error(&index_err),
            (McpErrorCode::FailedPrecondition, false),
            "IndexKeyringAlreadyInstalled must be FAILED_PRECONDITION, non-retriable"
        );

        let cold_err = StorageError::ColdKeyringAlreadyInstalled {
            reason: "None -> Some transition only".to_string(),
        };
        assert_eq!(
            classify_storage_error(&cold_err),
            (McpErrorCode::FailedPrecondition, false),
            "ColdKeyringAlreadyInstalled must be FAILED_PRECONDITION, non-retriable"
        );
    }

    #[test]
    fn type_violation_carries_expected_and_actual_type_details() {
        // Issue #3378: a TypeViolation surfaces `expected_type`/`actual_type`
        // under `error.details` (CONSTRAINT_VIOLATION, non-retriable), so a
        // caller can self-repair without parsing the free-text message.
        let e: Error = ConstraintError::TypeViolation {
            entity_kind: "node".into(),
            label: "Person".into(),
            property: "age".into(),
            expected_type: "int",
            actual_type: "string",
        }
        .into();
        let err = McpError::from_db_error(&e);
        assert_eq!(err.code(), McpErrorCode::ConstraintViolation);
        assert!(!err.is_retriable());
        // Message free text is preserved verbatim.
        assert_eq!(err.message(), e.to_string());
        let json = err.to_json();
        assert_eq!(json["code"], "CONSTRAINT_VIOLATION");
        assert_eq!(json["retriable"], false);
        assert_eq!(json["details"]["entity_kind"], "node");
        assert_eq!(json["details"]["label"], "Person");
        assert_eq!(json["details"]["property"], "age");
        // The type tokens are the stable DeclaredType/PropertyValue names.
        assert_eq!(json["details"]["expected_type"], "int");
        assert_eq!(json["details"]["actual_type"], "string");
    }

    #[test]
    fn missing_required_key_details_are_always_an_array() {
        // A single missing key must still serialize as a JSON array, so a
        // caller can iterate uniformly (Issue #3378).
        let single: Error = ConstraintError::MissingRequiredKey {
            entity_kind: "edge".into(),
            label: "KNOWS".into(),
            missing_keys: vec!["since".into()],
        }
        .into();
        let json = McpError::from_db_error(&single).to_json();
        assert_eq!(json["code"], "CONSTRAINT_VIOLATION");
        assert_eq!(json["retriable"], false);
        assert_eq!(json["details"]["entity_kind"], "edge");
        assert_eq!(json["details"]["label"], "KNOWS");
        assert!(
            json["details"]["missing_keys"].is_array(),
            "missing_keys must be an array even for one key"
        );
        assert_eq!(
            json["details"]["missing_keys"],
            serde_json::json!(["since"])
        );

        // Multiple missing keys are all reported, in order.
        let multi: Error = ConstraintError::MissingRequiredKey {
            entity_kind: "node".into(),
            label: "Person".into(),
            missing_keys: vec!["name".into(), "age".into()],
        }
        .into();
        let json = McpError::from_db_error(&multi).to_json();
        assert_eq!(
            json["details"]["missing_keys"],
            serde_json::json!(["name", "age"])
        );
    }

    #[test]
    fn non_conforming_on_enable_details_are_bounded() {
        // Issue #3378: NonConformingOnEnable is FAILED_PRECONDITION and
        // surfaces a *bounded* structured report — a per-(property,reason)
        // aggregated violations list plus a capped id sample, never a raw
        // per-entity id dump.
        use crate::core::constraint::ConformanceViolation;
        let e: Error = ConstraintError::NonConformingOnEnable {
            entity_kind: "node".into(),
            label: "Person".into(),
            violations: vec![ConformanceViolation {
                entity_kind: "node".into(),
                label: "Person".into(),
                property: Some("age".into()),
                reason: "expected type int, got string".into(),
                sample_ids: vec![1, 2, 3],
            }],
            total_non_conforming: 42,
            sample_ids: vec![1, 2, 3, 4, 5],
        }
        .into();
        let err = McpError::from_db_error(&e);
        assert_eq!(err.code(), McpErrorCode::FailedPrecondition);
        assert!(!err.is_retriable());
        let json = err.to_json();
        assert_eq!(json["code"], "FAILED_PRECONDITION");
        assert_eq!(json["retriable"], false);
        assert_eq!(json["details"]["entity_kind"], "node");
        assert_eq!(json["details"]["label"], "Person");
        assert_eq!(json["details"]["total_non_conforming"], 42);
        assert_eq!(
            json["details"]["sample_ids"],
            serde_json::json!([1, 2, 3, 4, 5])
        );
        let violations = json["details"]["violations"].as_array().unwrap();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0]["property"], "age");
        assert_eq!(violations[0]["reason"], "expected type int, got string");
        assert_eq!(violations[0]["sample_ids"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn unique_violation_details_are_not_built_by_from_db_error() {
        // The UniqueViolation `details` (existing_node_id + legacy top-level
        // fields) are built at the `server::db_error` call site, NOT here, so
        // `from_db_error` must leave `details` unset for it — the existing
        // behavior is unchanged by the schema-constraint work.
        let e: Error = ConstraintError::UniqueViolation {
            label: "Person".into(),
            property: "email".into(),
            value: "a".into(),
            existing_node_id: NodeId::new(1).unwrap(),
        }
        .into();
        let err = McpError::from_db_error(&e);
        assert_eq!(err.code(), McpErrorCode::ConstraintViolation);
        assert!(err.to_json().get("details").is_none());
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
