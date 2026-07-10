//! HTTP error type for AletheiaDB JSON API.
//!
//! Provides a single error enum with `IntoResponse` so handlers return
//! `Result<Json<ApiResponse>, AletheiaHttpError>` and get the right status
//! code + JSON error body for free.

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
    #[error("{0}")]
    PermissionDenied(String),

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
}

impl AletheiaHttpError {
    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) | Self::QueryParse(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Internal(_) | Self::StateMissing => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::PermissionDenied(_) => StatusCode::FORBIDDEN,
            Self::InvalidLimitOverride(_) => StatusCode::UNPROCESSABLE_ENTITY,
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

    /// Stable machine-readable code (additive: present for auth errors since
    /// Issue #3350 and for resource-limit errors since Issue #3368).
    fn code(&self) -> Option<&'static str> {
        match self {
            Self::Unauthorized => Some("UNAUTHENTICATED"),
            Self::PermissionDenied(_) => Some("PERMISSION_DENIED"),
            Self::ResourceLimitExceeded(_) => Some("RESOURCE_EXHAUSTED"),
            Self::InvalidLimitOverride(_) => Some("INVALID_ARGUMENT"),
            _ => None,
        }
    }

    /// Whether a caller may usefully retry (additive; Issue #3368). The flag is
    /// carried on the [`ResourceLimitExceeded`] itself: a read-class wall-clock
    /// timeout is transient (`true`), a write-class timeout is not (a committed
    /// write must not be duplicated), and row/byte caps are never retriable.
    /// `None` where the flag does not apply (existing variants keep their
    /// pre-#3368 body shape).
    fn retriable(&self) -> Option<bool> {
        match self {
            Self::ResourceLimitExceeded(e) => Some(e.retriable),
            Self::InvalidLimitOverride(_) => Some(false),
            _ => None,
        }
    }

    /// Structured, per-code metadata (additive; Issue #3368).
    fn details(&self) -> Option<Value> {
        match self {
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
            _ => None,
        }
    }

    /// Stable machine-readable code for *every* variant, using the Issue #3234
    /// vocabulary. Used to stamp `aletheiadb.error.code` onto the trace span
    /// (Issue #3376); the response body still only carries [`code`](Self::code)
    /// for auth errors so the existing wire shape is unchanged.
    #[must_use]
    // Only consumed by the tracing span-stamping path, which is compiled out
    // when `observability` is disabled; keep it available without warning.
    #[cfg_attr(not(feature = "observability"), allow(dead_code))]
    pub(crate) fn code_str(&self) -> &'static str {
        match self {
            Self::BadRequest(_) | Self::QueryParse(_) => "INVALID_ARGUMENT",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Internal(_) | Self::StateMissing => "INTERNAL",
            Self::Unauthorized => "UNAUTHENTICATED",
            Self::PermissionDenied(_) => "PERMISSION_DENIED",
            Self::ResourceLimitExceeded(_) => "RESOURCE_EXHAUSTED",
            Self::InvalidLimitOverride(_) => "INVALID_ARGUMENT",
        }
    }

    /// Convert to a response, additively including the active trace id
    /// (Issue #3376) as a `trace_id` body field and `x-trace-id` header when
    /// one is available, so a failing call can be looked up in the trace
    /// backend directly.
    #[must_use]
    pub(crate) fn into_response_with_trace(self, trace_id: Option<String>) -> Response {
        let status = self.status();
        let mut body = json!({
            "success": false,
            "error": self.to_string(),
        });
        // Additive fields — only inserted when the variant defines them, so
        // existing error bodies (bad-request, not-found, internal) are byte
        // identical to their pre-#3368 shape.
        if let Some(code) = self.code() {
            body["code"] = json!(code);
        }
        if let Some(retriable) = self.retriable() {
            body["retriable"] = json!(retriable);
        }
        if let Some(details) = self.details() {
            body["details"] = details;
        }
        // Additively carry the active trace id (Issue #3376) as a `trace_id`
        // body field so a failing call can be correlated in the trace backend.
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
        // A wall-clock timeout is a transient 429; hint clients to back off
        // briefly before retrying (Issue #3368 review). Fixed conservative 1 s.
        if matches!(
            &self,
            Self::ResourceLimitExceeded(e) if e.dimension == LimitDimension::WallClockTimeout
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
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "RESOURCE_EXHAUSTED");
        assert_eq!(body["retriable"], true);
        assert_eq!(body["details"]["dimension"], "wall_clock_timeout");
        assert_eq!(body["details"]["limit_ms"], 100);
        assert!(body["error"].as_str().unwrap().contains("timeout"));
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
        assert_eq!(body["code"], "RESOURCE_EXHAUSTED");
        assert_eq!(body["retriable"], false);
        assert_eq!(body["details"]["dimension"], "result_bytes");
        assert_eq!(body["details"]["limit"], 1024);
        assert_eq!(body["details"]["consumed"], 4096);
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
        assert_eq!(body["details"]["dimension"], "result_rows");
        assert_eq!(body["details"]["consumed"], 25);
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
        assert_eq!(body["code"], "RESOURCE_EXHAUSTED");
        assert_eq!(
            body["retriable"], false,
            "a write timeout must not invite a duplicate retry"
        );
        assert_eq!(body["details"]["dimension"], "wall_clock_timeout");
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
        assert_eq!(body["code"], "INVALID_ARGUMENT");
        assert_eq!(body["retriable"], false);
        assert_eq!(body["details"]["dimension"], "wall_clock_timeout");
        assert_eq!(body["details"]["requested"], 999);
        assert_eq!(body["details"]["ceiling"], 100);
    }

    #[tokio::test]
    async fn existing_variants_keep_minimal_body_shape() {
        // No `code`/`retriable`/`details` for a plain bad-request (unchanged).
        let (status, body) = body_json(AletheiaHttpError::BadRequest("nope".into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["success"], false);
        assert_eq!(body["error"], "nope");
        assert!(body.get("code").is_none());
        assert!(body.get("retriable").is_none());
        assert!(body.get("details").is_none());
    }

    #[tokio::test]
    async fn auth_error_body_unchanged_still_has_code_only() {
        let (status, body) = body_json(AletheiaHttpError::Unauthorized).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "UNAUTHENTICATED");
        // Auth variants deliberately keep their pre-#3368 shape (no retriable).
        assert!(body.get("retriable").is_none());
        assert!(body.get("details").is_none());
    }
}
