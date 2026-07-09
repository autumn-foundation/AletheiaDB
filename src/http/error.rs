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
/// the [`LimitDimension`], the effective `limit`, and (for the byte/row caps)
/// how much was `consumed`. The HTTP status and `retriable` flag are derived
/// from the dimension:
///
/// - [`LimitDimension::WallClockTimeout`] → `429`, `retriable: true`
/// - [`LimitDimension::ResultBytes`] / [`LimitDimension::ResultRows`] → `413`,
///   `retriable: false`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimitExceeded {
    /// Which dimension's cap was hit.
    pub dimension: LimitDimension,
    /// The effective limit that was exceeded (ms for timeout, else count/bytes).
    pub limit: u64,
    /// How much was consumed, when known (row count / byte size).
    pub consumed: Option<u64>,
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

    /// Whether a caller may usefully retry (additive; Issue #3368). Only the
    /// wall-clock timeout is transient. `None` where the flag does not apply
    /// (existing variants keep their pre-#3368 body shape).
    fn retriable(&self) -> Option<bool> {
        match self {
            Self::ResourceLimitExceeded(e) => Some(e.dimension == LimitDimension::WallClockTimeout),
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
}

impl IntoResponse for AletheiaHttpError {
    fn into_response(self) -> Response {
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
        let mut response = (status, Json(body)).into_response();
        if matches!(self, Self::Unauthorized) {
            response.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
        }
        response
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
            },
        ))
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["details"]["dimension"], "result_rows");
        assert_eq!(body["details"]["consumed"], 25);
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
