//! HTTP error type for AletheiaDB JSON API.
//!
//! Provides a single error enum with `IntoResponse` so handlers return
//! `Result<Json<ApiResponse>, AletheiaHttpError>` and get the right status
//! code + JSON error body for free.

use crate::auth::{AccessClass, Role};
use crate::http::config::{LimitDimension, LimitOverrideError};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

/// A per-query resource limit was exceeded at the HTTP layer (Issue #3368).
///
/// Carries enough structure to render the `{code, retriable, details}` body:
/// the [`LimitDimension`], the effective `limit`, (for the byte/row caps) how
/// much was `consumed`, and whether a retry could usefully succeed. The HTTP
/// status is derived from the dimension:
///
/// - [`LimitDimension::WallClockTimeout`] → `429`
/// - [`LimitDimension::ResultBytes`] / [`LimitDimension::ResultRows`] → `413`
///
/// The [`retriable`](Self::retriable) flag is carried explicitly rather than
/// derived from the dimension: a wall-clock timeout is transient and normally
/// `retriable: true`, **but** a timeout on a *write*-class operation is
/// `retriable: false` — the write may already have committed on the blocking
/// pool, so a naive retry would duplicate it (Issue #3368 review). Result-row
/// and result-byte caps are always `retriable: false`, and (post-review) only
/// ever apply to read-class operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimitExceeded {
    /// Which dimension's cap was hit.
    pub dimension: LimitDimension,
    /// The effective limit that was exceeded (ms for timeout, else count/bytes).
    pub limit: u64,
    /// How much was consumed, when known (row count / byte size).
    pub consumed: Option<u64>,
    /// Whether a retry could usefully succeed. `true` only for a read-class
    /// wall-clock timeout; `false` for a write-class timeout and for every
    /// row/byte cap.
    pub retriable: bool,
}

impl std::fmt::Display for ResourceLimitExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.dimension {
            LimitDimension::WallClockTimeout => {
                write!(
                    f,
                    "query exceeded the wall-clock timeout of {} ms",
                    self.limit
                )
            }
            LimitDimension::ResultRows => match self.consumed {
                Some(c) => write!(
                    f,
                    "query result exceeded the row limit of {} (produced {})",
                    self.limit, c
                ),
                None => write!(f, "query result exceeded the row limit of {}", self.limit),
            },
            LimitDimension::ResultBytes => match self.consumed {
                Some(c) => write!(
                    f,
                    "query response exceeded the byte limit of {} (serialized {})",
                    self.limit, c
                ),
                None => write!(
                    f,
                    "query response exceeded the byte limit of {} bytes",
                    self.limit
                ),
            },
        }
    }
}

/// Errors raised by AletheiaDB's HTTP handlers.
#[derive(Debug, thiserror::Error)]
pub enum AletheiaHttpError {
    /// Request validation failed (bad IDs, pagination overflow, malformed input).
    #[error("{0}")]
    BadRequest(String),

    /// A resource lookup that the client reasonably expected to exist failed.
    #[error("{0}")]
    NotFound(String),

    /// Query syntax / parse errors surfaced from the AQL parser.
    #[error("{0}")]
    QueryParse(String),

    /// Any unexpected backend or serialization failure.
    #[error("{0}")]
    Internal(String),

    /// Shared application state was not installed at startup.
    ///
    /// Treated as HTTP 500 — this is a boot-time invariant; reaching it in a
    /// running server indicates the app was built incorrectly.
    #[error("application state not installed")]
    StateMissing,

    /// Authentication failed (missing/malformed/unknown/revoked credential).
    ///
    /// Deliberately a unit variant: every authentication failure produces a
    /// byte-identical 401 body so callers can't distinguish "no such key"
    /// from "revoked key", and the presented credential is never echoed.
    #[error("authentication required")]
    Unauthorized,

    /// The authenticated principal's role does not allow this operation.
    ///
    /// Carries the operation's required [`AccessClass`] and the principal's
    /// [`Role`] so the error body can surface a structured
    /// `details: {required_class, principal_role}` block (matching the MCP
    /// surface, Issue #3234). The `Display` free text is byte-identical to the
    /// legacy `"role '{role}' does not permit {class} access"` message.
    #[error("role '{principal_role}' does not permit {required_class} access")]
    PermissionDenied {
        /// The access class the operation required.
        required_class: AccessClass,
        /// The authenticated principal's role.
        principal_role: Role,
    },

    /// A per-query resource limit (timeout / rows / bytes) was exceeded
    /// (Issue #3368). Renders `429` (timeout) or `413` (rows/bytes).
    #[error("{0}")]
    ResourceLimitExceeded(ResourceLimitExceeded),

    /// A per-call limit override exceeded the operator ceiling (Issue #3368).
    /// Renders `422 INVALID_ARGUMENT`.
    #[error(
        "limit override for '{}' ({}) exceeds the maximum allowed ({})",
        .0.dimension.as_str(), .0.requested, .0.ceiling
    )]
    InvalidLimitOverride(LimitOverrideError),

    /// The bounded in-flight-query worker pool is full (Issue #3368 DoS guard):
    /// a new *timed* `/query` was rejected instead of spawning yet another
    /// detached, non-cancellable worker. Renders `503 UNAVAILABLE`, `retriable:
    /// true` — the caller should back off and retry once load subsides. Carries
    /// the configured `cap` so the caller can see the operative limit.
    #[error(
        "server is at its cap of {cap} concurrently executing timed queries; retry with backoff"
    )]
    InFlightCapacityExceeded {
        /// The configured `max_in_flight_queries` cap that was reached.
        cap: usize,
    },

    /// A per-principal changefeed subscription quota was exceeded (Issue #3678).
    /// Renders `429 RESOURCE_EXHAUSTED`, `retriable: true`, with
    /// `details {principal, current, limit}` — byte-shape-identical to the MCP
    /// surface's classification of [`StorageError::PrincipalQuotaExceeded`].
    #[error(
        "principal '{principal}' has reached its changefeed subscription quota (current={current}, limit={limit})"
    )]
    PrincipalQuotaExceeded {
        /// The principal id whose quota was exceeded.
        principal: String,
        /// The principal's current live-subscription count.
        current: usize,
        /// The principal's configured maximum.
        limit: usize,
    },

    /// A db-layer error pre-classified into the unified #3234 envelope, so the
    /// HTTP body carries the same `code`/`retriable`/`details` as the MCP
    /// surface. Built via [`Self::from_db_error`], which classifies the
    /// schema-constraint (Issue #3378), uniqueness, and transaction
    /// conflict/precondition classes; every other db error keeps its prior HTTP
    /// mapping (a 400 `BadRequest`).
    #[error("{message}")]
    Structured {
        /// The #3234 code string (e.g. `CONSTRAINT_VIOLATION`,
        /// `FAILED_PRECONDITION`, `CONFLICT`, `UNAVAILABLE`).
        code: &'static str,
        /// HTTP status derived from the code (`409` / `412` / `503` / …).
        status: StatusCode,
        /// Whether a fresh attempt could usefully succeed. `true` only for the
        /// transient conflict / clock-skew classes; `false` for the caller-fault
        /// constraint / precondition classes.
        retriable: bool,
        /// Free-text message: the db error's `Display` (preserved verbatim).
        message: String,
        /// Structured, machine-readable metadata (shared with the MCP surface).
        details: Option<Value>,
    },
}

impl AletheiaHttpError {
    /// Classify a db-layer [`Error`](crate::core::error::Error) surfaced from a
    /// write handler into the HTTP error envelope, using the same #3234 code
    /// vocabulary as the MCP surface (`src/mcp/error.rs`).
    ///
    /// Three classes are pre-classified into [`Self::Structured`] so the
    /// response carries the same `code`/`retriable`/`details` as the MCP
    /// surface:
    ///
    /// - **Constraint violations** (Issue #3378 + uniqueness): a
    ///   type/required-key/unique violation is a declared constraint rejecting
    ///   *this* write → `CONSTRAINT_VIOLATION` (`409`); a non-conforming enable
    ///   or a pre-existing duplicate is a state problem the caller must fix
    ///   first → `FAILED_PRECONDITION` (`412`). All non-retriable.
    /// - **Transaction conflicts / preconditions**: a serialization failure /
    ///   write conflict / abort is transient → `CONFLICT` (`409`, retriable); a
    ///   validation failure (e.g. the #3416 concurrent-orphan guard), invalid
    ///   state, already-committed, or lost CAS is a caller-fault precondition →
    ///   `FAILED_PRECONDITION` (`412`, non-retriable); clock skew heals on its
    ///   own → `UNAVAILABLE` (`503`, retriable).
    ///
    /// Everything else preserves the prior write-path mapping (`400 BadRequest`
    /// with the free-text message) — critically, this keeps genuinely bad input
    /// (a malformed property map wrapped as [`Error::Other`], an invalid id) at
    /// `400`, so extending the classifier never over-broadens a real
    /// bad-request into a constraint or internal error.
    ///
    /// [`Error::Other`]: crate::core::error::Error::Other
    #[must_use]
    pub(crate) fn from_db_error(e: &crate::core::error::Error) -> Self {
        use crate::core::error::ConstraintError as CE;
        use crate::core::error::Error as E;
        use crate::core::error::TransactionError as TE;

        // --- Constraint violations (Issue #3378 schema + uniqueness). ---
        if let Some(ce) = e.as_constraint() {
            return match ce {
                // A declared type/required-key violation rejects *this* write.
                CE::TypeViolation { .. } | CE::MissingRequiredKey { .. } => Self::Structured {
                    code: "CONSTRAINT_VIOLATION",
                    status: StatusCode::CONFLICT,
                    retriable: false,
                    message: e.to_string(),
                    details: ce.structured_details(),
                },
                // A uniqueness violation is likewise CONSTRAINT_VIOLATION. Its
                // `structured_details()` is `None`, so build the machine-readable
                // metadata here, mirroring the MCP surface's `db_error` call site
                // (`label`/`property`/`value`/`existing_node_id`).
                CE::UniqueViolation {
                    label,
                    property,
                    value,
                    existing_node_id,
                } => Self::Structured {
                    code: "CONSTRAINT_VIOLATION",
                    status: StatusCode::CONFLICT,
                    retriable: false,
                    message: e.to_string(),
                    details: Some(json!({
                        "label": label,
                        "property": property,
                        "value": value,
                        "existing_node_id": existing_node_id.as_u64(),
                    })),
                },
                // A non-conforming enable or a pre-existing duplicate is a state
                // problem the caller must fix first.
                CE::NonConformingOnEnable { .. } | CE::DuplicateOnEnable { .. } => {
                    Self::Structured {
                        code: "FAILED_PRECONDITION",
                        status: StatusCode::PRECONDITION_FAILED,
                        retriable: false,
                        message: e.to_string(),
                        // `None` for DuplicateOnEnable (no shared structured details).
                        details: ce.structured_details(),
                    }
                }
                // An unsupported key type in the *enable request itself* is a
                // genuine bad request, not a constraint rejecting a write.
                CE::UnsupportedKeyType { .. } => Self::BadRequest(e.to_string()),
            };
        }

        // --- Transaction conflicts / preconditions (mirrors the MCP
        // classifier in src/mcp/error.rs::classify_transaction_error). ---
        if let E::Transaction(te) = e {
            return match te {
                // Concurrency conflicts a fresh attempt can win.
                TE::SerializationFailure { .. } | TE::WriteConflict { .. } | TE::Aborted { .. } => {
                    Self::Structured {
                        code: "CONFLICT",
                        status: StatusCode::CONFLICT,
                        retriable: true,
                        message: e.to_string(),
                        details: None,
                    }
                }
                // Well-formed but forbidden by current state (e.g. the #3416
                // concurrent-orphan validation guard, a lost compare-and-set):
                // retrying the identical call cannot succeed.
                TE::ValidationFailed { .. }
                | TE::InvalidState { .. }
                | TE::AlreadyCommitted { .. }
                | TE::CasMismatch { .. } => Self::Structured {
                    code: "FAILED_PRECONDITION",
                    status: StatusCode::PRECONDITION_FAILED,
                    retriable: false,
                    message: e.to_string(),
                    details: None,
                },
                // A clock-adjacent hiccup heals on its own at a later tick.
                TE::ClockSkew { .. } => Self::Structured {
                    code: "UNAVAILABLE",
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    retriable: true,
                    message: e.to_string(),
                    details: None,
                },
                // Genuine internal failures (commit/rollback/poisoned lock).
                TE::CommitFailed { .. } | TE::RollbackFailed { .. } | TE::LockPoisoned { .. } => {
                    Self::Internal(e.to_string())
                }
            };
        }

        // Every other db error keeps the prior write-path mapping (400
        // BadRequest), preserving the contract that genuine bad input stays 400.
        Self::BadRequest(e.to_string())
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) | Self::QueryParse(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Internal(_) | Self::StateMissing => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::PermissionDenied { .. } => StatusCode::FORBIDDEN,
            Self::InvalidLimitOverride(_) => StatusCode::UNPROCESSABLE_ENTITY,
            // A pre-classified db error carries its own status (409 / 412 / 503).
            Self::Structured { status, .. } => *status,
            // Transient overload → 503 Service Unavailable (retriable).
            Self::InFlightCapacityExceeded { .. } => StatusCode::SERVICE_UNAVAILABLE,
            // A per-principal quota breach → 429 Too Many Requests (retriable).
            Self::PrincipalQuotaExceeded { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::ResourceLimitExceeded(e) => match e.dimension {
                // Timeout is transient → 429 Too Many Requests.
                LimitDimension::WallClockTimeout => StatusCode::TOO_MANY_REQUESTS,
                // Result too large → 413 Payload Too Large.
                LimitDimension::ResultRows | LimitDimension::ResultBytes => {
                    StatusCode::PAYLOAD_TOO_LARGE
                }
            },
        }
    }

    /// Whether a caller may usefully retry. `true` only for transient failure
    /// classes: a read-class wall-clock timeout (carried on the
    /// [`ResourceLimitExceeded`] itself) and a full in-flight worker pool
    /// (transient overload). A write-class timeout is not retriable (a
    /// committed write must not be duplicated), and every caller-fault class
    /// (bad-request, not-found, auth, override, row/byte caps) is `false`.
    ///
    /// Mirrors the MCP surface's explicit per-error classification (Issue
    /// #3234): the nested error envelope always carries a concrete `retriable`
    /// boolean.
    fn retriable(&self) -> bool {
        match self {
            Self::ResourceLimitExceeded(e) => e.retriable,
            // Transient overload: backing off and retrying can succeed.
            Self::InFlightCapacityExceeded { .. } => true,
            // A fairness quota: another of this principal's subscriptions may
            // drop, so a backed-off retry can succeed (Issue #3678).
            Self::PrincipalQuotaExceeded { .. } => true,
            // A pre-classified db error carries its own retriability (true only
            // for the transient conflict / clock-skew classes).
            Self::Structured { retriable, .. } => *retriable,
            _ => false,
        }
    }

    /// Structured, per-code metadata (additive; Issue #3368 and, for
    /// `PermissionDenied`, Issue #3234). Rendered under `error.details`.
    fn details(&self) -> Option<Value> {
        match self {
            // A role denial names the required class and the principal's role,
            // mirroring the MCP-surface 403 details (Issue #3234).
            Self::PermissionDenied {
                required_class,
                principal_role,
            } => Some(json!({
                "required_class": required_class.to_string(),
                "principal_role": principal_role.to_string(),
            })),
            Self::ResourceLimitExceeded(e) => {
                let mut d = json!({
                    "dimension": e.dimension.as_str(),
                });
                match e.dimension {
                    LimitDimension::WallClockTimeout => {
                        d["limit_ms"] = json!(e.limit);
                    }
                    LimitDimension::ResultRows | LimitDimension::ResultBytes => {
                        d["limit"] = json!(e.limit);
                        if let Some(c) = e.consumed {
                            d["consumed"] = json!(c);
                        }
                    }
                }
                Some(d)
            }
            Self::InvalidLimitOverride(e) => Some(json!({
                "dimension": e.dimension.as_str(),
                "requested": e.requested,
                "ceiling": e.ceiling,
            })),
            Self::InFlightCapacityExceeded { cap } => Some(json!({
                "reason": "in_flight_query_cap",
                "max_in_flight_queries": cap,
            })),
            Self::PrincipalQuotaExceeded {
                principal,
                current,
                limit,
            } => Some(json!({
                "principal": principal,
                "current": current,
                "limit": limit,
            })),
            // A pre-classified db error forwards its structured details (shared
            // with the MCP surface), so the HTTP body is byte-shape-identical.
            Self::Structured { details, .. } => details.clone(),
            _ => None,
        }
    }

    /// Stable machine-readable code for *every* variant, using the Issue #3234
    /// vocabulary. Rendered as `error.code` in the response body (unified with
    /// the MCP surface) and stamped as `aletheiadb.error.code` onto the trace
    /// span (Issue #3376).
    #[must_use]
    pub(crate) fn code_str(&self) -> &'static str {
        match self {
            Self::BadRequest(_) | Self::QueryParse(_) | Self::InvalidLimitOverride(_) => {
                "INVALID_ARGUMENT"
            }
            Self::NotFound(_) => "NOT_FOUND",
            Self::Internal(_) | Self::StateMissing => "INTERNAL",
            Self::Unauthorized => "UNAUTHENTICATED",
            Self::PermissionDenied { .. } => "PERMISSION_DENIED",
            Self::ResourceLimitExceeded(_) => "RESOURCE_EXHAUSTED",
            Self::PrincipalQuotaExceeded { .. } => "RESOURCE_EXHAUSTED",
            Self::InFlightCapacityExceeded { .. } => "UNAVAILABLE",
            // The #3234 code was fixed when the variant was classified.
            Self::Structured { code, .. } => code,
        }
    }

    /// Convert to a response with the unified nested error envelope (Issue
    /// #3234): `{"error": {"code", "message", "retriable", "details"?}}`,
    /// byte-shape-identical to the MCP surface. The active trace id (Issue
    /// #3376) is carried additively as a **top-level** `trace_id` sibling of
    /// `error` (the SDK reads it top-level) and as an `x-trace-id` header when
    /// one is available, so a failing call can be looked up in the trace
    /// backend directly.
    #[must_use]
    pub(crate) fn into_response_with_trace(self, trace_id: Option<String>) -> Response {
        let status = self.status();
        // Build the nested `error` object, mirroring `McpError::to_json`'s
        // field order: code, message, retriable, then optional details.
        let mut error_obj = serde_json::Map::new();
        error_obj.insert("code".to_string(), json!(self.code_str()));
        error_obj.insert("message".to_string(), json!(self.to_string()));
        error_obj.insert("retriable".to_string(), json!(self.retriable()));
        if let Some(details) = self.details() {
            error_obj.insert("details".to_string(), details);
        }
        let mut body = json!({ "error": Value::Object(error_obj) });
        // Additively carry the active trace id (Issue #3376) as a *top-level*
        // `trace_id` field (sibling of `error`, not nested) so a failing call
        // can be correlated in the trace backend and the SDK can read it.
        if let (Some(map), Some(tid)) = (body.as_object_mut(), trace_id.as_deref()) {
            map.insert("trace_id".to_string(), json!(tid));
        }
        let mut response = (status, Json(body)).into_response();
        if matches!(self, Self::Unauthorized) {
            response.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
        }
        // Additively surface the trace id as an `x-trace-id` response header
        // (Issue #3376) so limit errors (429/413/422) are correlatable too.
        if let Some(tid) = trace_id.and_then(|t| axum::http::HeaderValue::from_str(&t).ok()) {
            response
                .headers_mut()
                .insert(axum::http::HeaderName::from_static("x-trace-id"), tid);
        }
        // A transient overload — a wall-clock timeout (429) or a full in-flight
        // worker pool (503) — hints clients to back off briefly before retrying
        // (Issue #3368). Fixed conservative 1 s.
        if matches!(
            &self,
            Self::ResourceLimitExceeded(e) if e.dimension == LimitDimension::WallClockTimeout
        ) || matches!(
            &self,
            Self::InFlightCapacityExceeded { .. } | Self::PrincipalQuotaExceeded { .. }
        ) {
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("1"),
            );
        }
        response
    }
}

impl IntoResponse for AletheiaHttpError {
    fn into_response(self) -> Response {
        // Auto-conversion path (extractor rejections, `?` in handlers not
        // wrapped in a root span): include a trace id if one happens to be
        // active on this thread.
        let trace_id = crate::http::trace::active_trace_id();
        self.into_response_with_trace(trace_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_json(err: AletheiaHttpError) -> (StatusCode, Value) {
        let resp = err.into_response();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn timeout_maps_to_429_retriable_with_details() {
        let (status, body) = body_json(AletheiaHttpError::ResourceLimitExceeded(
            ResourceLimitExceeded {
                dimension: LimitDimension::WallClockTimeout,
                limit: 100,
                consumed: None,
                retriable: true,
            },
        ))
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            body.get("success").is_none(),
            "flat `success` field dropped"
        );
        assert_eq!(body["error"]["code"], "RESOURCE_EXHAUSTED");
        assert_eq!(body["error"]["retriable"], true);
        assert_eq!(body["error"]["details"]["dimension"], "wall_clock_timeout");
        assert_eq!(body["error"]["details"]["limit_ms"], 100);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("timeout")
        );
    }

    #[tokio::test]
    async fn byte_cap_maps_to_413_not_retriable_with_consumed() {
        let (status, body) = body_json(AletheiaHttpError::ResourceLimitExceeded(
            ResourceLimitExceeded {
                dimension: LimitDimension::ResultBytes,
                limit: 1024,
                consumed: Some(4096),
                retriable: false,
            },
        ))
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["error"]["code"], "RESOURCE_EXHAUSTED");
        assert_eq!(body["error"]["retriable"], false);
        assert_eq!(body["error"]["details"]["dimension"], "result_bytes");
        assert_eq!(body["error"]["details"]["limit"], 1024);
        assert_eq!(body["error"]["details"]["consumed"], 4096);
    }

    #[tokio::test]
    async fn row_reject_maps_to_413() {
        let (status, body) = body_json(AletheiaHttpError::ResourceLimitExceeded(
            ResourceLimitExceeded {
                dimension: LimitDimension::ResultRows,
                limit: 10,
                consumed: Some(25),
                retriable: false,
            },
        ))
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["error"]["details"]["dimension"], "result_rows");
        assert_eq!(body["error"]["details"]["consumed"], 25);
    }

    /// A write-class wall-clock timeout renders `429` but `retriable: false`
    /// (the write may already have committed; a retry could duplicate it —
    /// Issue #3368 review MUST-FIX 1).
    #[tokio::test]
    async fn write_timeout_maps_to_429_not_retriable() {
        let (status, body) = body_json(AletheiaHttpError::ResourceLimitExceeded(
            ResourceLimitExceeded {
                dimension: LimitDimension::WallClockTimeout,
                limit: 100,
                consumed: None,
                retriable: false,
            },
        ))
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["code"], "RESOURCE_EXHAUSTED");
        assert_eq!(
            body["error"]["retriable"], false,
            "a write timeout must not invite a duplicate retry"
        );
        assert_eq!(body["error"]["details"]["dimension"], "wall_clock_timeout");
    }

    #[tokio::test]
    async fn invalid_override_maps_to_422_invalid_argument() {
        let (status, body) = body_json(AletheiaHttpError::InvalidLimitOverride(
            LimitOverrideError {
                dimension: LimitDimension::WallClockTimeout,
                requested: 999,
                ceiling: 100,
            },
        ))
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(body["error"]["retriable"], false);
        assert_eq!(body["error"]["details"]["dimension"], "wall_clock_timeout");
        assert_eq!(body["error"]["details"]["requested"], 999);
        assert_eq!(body["error"]["details"]["ceiling"], 100);
    }

    /// The in-flight-capacity guard renders `503 UNAVAILABLE`, `retriable:
    /// true`, names the cap in `details`, and carries `Retry-After` (Issue
    /// #3368 DoS guard).
    #[tokio::test]
    async fn in_flight_cap_maps_to_503_unavailable_retriable() {
        let err = AletheiaHttpError::InFlightCapacityExceeded { cap: 64 };
        let resp = err.into_response();
        let status = resp.status();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("1"),
        );
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            body.get("success").is_none(),
            "flat `success` field dropped"
        );
        assert_eq!(body["error"]["code"], "UNAVAILABLE");
        assert_eq!(body["error"]["retriable"], true);
        assert_eq!(body["error"]["details"]["reason"], "in_flight_query_cap");
        assert_eq!(body["error"]["details"]["max_in_flight_queries"], 64);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("cap of 64")
        );
    }

    /// Issue #3678: a per-principal changefeed quota breach renders `429
    /// RESOURCE_EXHAUSTED`, `retriable: true`, `Retry-After: 1`, and
    /// `details {principal, current, limit}` — byte-shape-identical to the MCP
    /// surface.
    #[tokio::test]
    async fn principal_quota_maps_to_429_resource_exhausted_retriable() {
        let err = AletheiaHttpError::PrincipalQuotaExceeded {
            principal: "alice".into(),
            current: 2,
            limit: 2,
        };
        let resp = err.into_response();
        let status = resp.status();
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("1"),
        );
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            body.get("success").is_none(),
            "flat `success` field dropped"
        );
        assert_eq!(body["error"]["code"], "RESOURCE_EXHAUSTED");
        assert_eq!(body["error"]["retriable"], true);
        assert_eq!(body["error"]["details"]["principal"], "alice");
        assert_eq!(body["error"]["details"]["current"], 2);
        assert_eq!(body["error"]["details"]["limit"], 2);
    }

    #[tokio::test]
    async fn bad_request_renders_nested_invalid_argument() {
        // The nested envelope now always carries code/message/retriable; a
        // plain bad-request has no `details` and is never retriable.
        let (status, body) = body_json(AletheiaHttpError::BadRequest("nope".into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body.get("success").is_none(),
            "flat `success` field dropped"
        );
        assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(body["error"]["message"], "nope");
        assert_eq!(body["error"]["retriable"], false);
        assert!(body["error"].get("details").is_none());
    }

    /// Issue #3378: a schema type violation surfaced from a write handler is
    /// classified into the unified #3234 envelope — `409 CONSTRAINT_VIOLATION`,
    /// `retriable: false`, and structured `details` byte-shape-identical to the
    /// MCP surface (`expected_type`/`actual_type`).
    #[tokio::test]
    async fn schema_type_violation_renders_nested_constraint_violation_with_details() {
        use crate::core::error::ConstraintError;
        let db_err: crate::core::error::Error = ConstraintError::TypeViolation {
            entity_kind: "node".into(),
            label: "Person".into(),
            property: "age".into(),
            expected_type: "int",
            actual_type: "string",
        }
        .into();
        let http_err = AletheiaHttpError::from_db_error(&db_err);
        let (status, body) = body_json(http_err).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body.get("success").is_none(),
            "flat `success` field dropped"
        );
        assert_eq!(body["error"]["code"], "CONSTRAINT_VIOLATION");
        assert_eq!(body["error"]["retriable"], false);
        // Message free text is preserved verbatim from the db error.
        assert_eq!(body["error"]["message"], db_err.to_string());
        assert_eq!(body["error"]["details"]["entity_kind"], "node");
        assert_eq!(body["error"]["details"]["label"], "Person");
        assert_eq!(body["error"]["details"]["property"], "age");
        assert_eq!(body["error"]["details"]["expected_type"], "int");
        assert_eq!(body["error"]["details"]["actual_type"], "string");
    }

    /// Parity guard: the HTTP `error.details` payload is exactly the shared
    /// core [`ConstraintError::structured_details`] output — the same single
    /// source of truth the MCP `from_db_error` mapping uses — so the two
    /// surfaces can never drift. Also covers the `missing_keys`-is-an-array
    /// contract and the `FAILED_PRECONDITION` (412) rung.
    #[tokio::test]
    async fn schema_constraint_http_details_match_shared_core_source() {
        use crate::core::error::ConstraintError;

        // MissingRequiredKey → CONSTRAINT_VIOLATION (409), details.missing_keys array.
        let missing: crate::core::error::Error = ConstraintError::MissingRequiredKey {
            entity_kind: "edge".into(),
            label: "KNOWS".into(),
            missing_keys: vec!["since".into()],
        }
        .into();
        let expected = missing.as_constraint().unwrap().structured_details();
        let (status, body) = body_json(AletheiaHttpError::from_db_error(&missing)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "CONSTRAINT_VIOLATION");
        assert!(body["error"]["details"]["missing_keys"].is_array());
        assert_eq!(Some(&body["error"]["details"]), expected.as_ref());

        // NonConformingOnEnable → FAILED_PRECONDITION (412), bounded report.
        let non_conforming: crate::core::error::Error = ConstraintError::NonConformingOnEnable {
            entity_kind: "node".into(),
            label: "Person".into(),
            violations: vec![],
            total_non_conforming: 3,
            sample_ids: vec![1, 2, 3],
        }
        .into();
        let expected = non_conforming.as_constraint().unwrap().structured_details();
        let (status, body) = body_json(AletheiaHttpError::from_db_error(&non_conforming)).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(body["error"]["code"], "FAILED_PRECONDITION");
        assert_eq!(body["error"]["retriable"], false);
        assert_eq!(body["error"]["details"]["total_non_conforming"], 3);
        assert_eq!(Some(&body["error"]["details"]), expected.as_ref());
    }

    /// A non-constraint db error keeps the prior write-path mapping unchanged:
    /// a `400 BadRequest` with no `details` (no behavior change outside the
    /// schema-constraint cases).
    #[tokio::test]
    async fn from_db_error_preserves_bad_request_for_non_constraint_errors() {
        let db_err = crate::core::error::Error::other("boom");
        let (status, body) = body_json(AletheiaHttpError::from_db_error(&db_err)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(body["error"]["retriable"], false);
        assert!(body["error"].get("details").is_none());
    }

    /// Issue #3629/#3234: a uniqueness violation surfaced from a legacy
    /// JSON-RPC write is classified into the unified envelope — `409
    /// CONSTRAINT_VIOLATION`, `retriable: false`, and machine-readable
    /// `details` (`label`/`property`/`value`/`existing_node_id`) mirroring the
    /// MCP surface — instead of the pre-#3629 blanket `400 INVALID_ARGUMENT`.
    #[tokio::test]
    async fn unique_violation_renders_nested_constraint_violation_with_existing_id() {
        use crate::core::error::ConstraintError;
        use crate::core::id::NodeId;
        let db_err: crate::core::error::Error = ConstraintError::UniqueViolation {
            label: "Person".into(),
            property: "email".into(),
            value: "a@b.com".into(),
            existing_node_id: NodeId::new(7).unwrap(),
        }
        .into();
        let (status, body) = body_json(AletheiaHttpError::from_db_error(&db_err)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.get("success").is_none());
        assert_eq!(body["error"]["code"], "CONSTRAINT_VIOLATION");
        assert_eq!(body["error"]["retriable"], false);
        assert_eq!(body["error"]["message"], db_err.to_string());
        assert_eq!(body["error"]["details"]["label"], "Person");
        assert_eq!(body["error"]["details"]["property"], "email");
        assert_eq!(body["error"]["details"]["value"], "a@b.com");
        assert_eq!(body["error"]["details"]["existing_node_id"], 7);
    }

    /// A pre-existing duplicate blocking a constraint enable is a state problem
    /// the caller must fix first → `412 FAILED_PRECONDITION`, non-retriable.
    #[tokio::test]
    async fn duplicate_on_enable_renders_412_failed_precondition() {
        use crate::core::error::ConstraintError;
        use crate::core::id::NodeId;
        let db_err: crate::core::error::Error = ConstraintError::DuplicateOnEnable {
            label: "Person".into(),
            property: "email".into(),
            value: "a@b.com".into(),
            node_ids: vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()],
        }
        .into();
        let (status, body) = body_json(AletheiaHttpError::from_db_error(&db_err)).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(body["error"]["code"], "FAILED_PRECONDITION");
        assert_eq!(body["error"]["retriable"], false);
        // No shared structured details for this uniqueness variant.
        assert!(body["error"].get("details").is_none());
    }

    /// An unsupported key type in the *enable request itself* is genuine bad
    /// input and stays `400 INVALID_ARGUMENT` (not reclassified as a
    /// constraint) — guards against the classifier over-broadening.
    #[tokio::test]
    async fn unsupported_key_type_stays_400_invalid_argument() {
        use crate::core::error::ConstraintError;
        let db_err: crate::core::error::Error = ConstraintError::UnsupportedKeyType {
            label: "Person".into(),
            property: "email".into(),
            type_name: "vector".into(),
        }
        .into();
        let (status, body) = body_json(AletheiaHttpError::from_db_error(&db_err)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(body["error"]["retriable"], false);
        assert!(body["error"].get("details").is_none());
    }

    /// A validation failure at commit (e.g. the #3416 concurrent-orphan guard)
    /// is a caller-fault precondition → `412 FAILED_PRECONDITION`,
    /// non-retriable (retrying the identical call cannot succeed).
    #[tokio::test]
    async fn validation_failed_renders_412_failed_precondition_not_retriable() {
        use crate::core::error::TransactionError;
        let db_err: crate::core::error::Error = TransactionError::ValidationFailed {
            reason: "delete would orphan a concurrently-created edge".into(),
        }
        .into();
        let (status, body) = body_json(AletheiaHttpError::from_db_error(&db_err)).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(body["error"]["code"], "FAILED_PRECONDITION");
        assert_eq!(body["error"]["retriable"], false);
        assert_eq!(body["error"]["message"], db_err.to_string());
    }

    /// A serialization failure / write conflict is transient → `409 CONFLICT`,
    /// `retriable: true` (a fresh attempt can win the race).
    #[tokio::test]
    async fn serialization_failure_renders_409_conflict_retriable() {
        use crate::core::error::TransactionError;
        let db_err: crate::core::error::Error = TransactionError::SerializationFailure {
            entity: "node:5".into(),
            reason: "concurrent write committed after snapshot".into(),
        }
        .into();
        let (status, body) = body_json(AletheiaHttpError::from_db_error(&db_err)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "CONFLICT");
        assert_eq!(
            body["error"]["retriable"], true,
            "a serialization conflict is retriable"
        );
    }

    /// Clock skew heals on its own → `503 UNAVAILABLE`, `retriable: true`.
    #[tokio::test]
    async fn clock_skew_renders_503_unavailable_retriable() {
        use crate::core::error::TransactionError;
        let db_err: crate::core::error::Error = TransactionError::ClockSkew {
            wallclock: 100,
            previous: 200,
            drift_us: -100,
            max_allowed: 50,
        }
        .into();
        let (status, body) = body_json(AletheiaHttpError::from_db_error(&db_err)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "UNAVAILABLE");
        assert_eq!(body["error"]["retriable"], true);
    }

    /// A genuine internal transaction failure (commit failed) stays `500
    /// INTERNAL`, non-retriable — never reclassified as a caller fault.
    #[tokio::test]
    async fn commit_failed_renders_500_internal() {
        use crate::core::error::TransactionError;
        let db_err: crate::core::error::Error = TransactionError::CommitFailed {
            reason: "wal fsync failed".into(),
        }
        .into();
        let (status, body) = body_json(AletheiaHttpError::from_db_error(&db_err)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "INTERNAL");
        assert_eq!(body["error"]["retriable"], false);
    }

    #[tokio::test]
    async fn auth_error_renders_nested_unauthenticated() {
        let (status, body) = body_json(AletheiaHttpError::Unauthorized).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(
            body.get("success").is_none(),
            "flat `success` field dropped"
        );
        assert_eq!(body["error"]["code"], "UNAUTHENTICATED");
        assert_eq!(body["error"]["message"], "authentication required");
        assert_eq!(body["error"]["retriable"], false);
        assert!(body["error"].get("details").is_none());
    }
}
