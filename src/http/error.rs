//! HTTP error type for AletheiaDB JSON API.
//!
//! Provides a single error enum with `IntoResponse` so handlers return
//! `Result<Json<ApiResponse>, AletheiaHttpError>` and get the right status
//! code + JSON error body for free.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

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
}

impl AletheiaHttpError {
    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) | Self::QueryParse(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Internal(_) | Self::StateMissing => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::PermissionDenied(_) => StatusCode::FORBIDDEN,
        }
    }

    /// Stable machine-readable code, present only for auth errors in the
    /// response body (additive; preserved for backward compatibility).
    fn code(&self) -> Option<&'static str> {
        match self {
            Self::Unauthorized => Some("UNAUTHENTICATED"),
            Self::PermissionDenied(_) => Some("PERMISSION_DENIED"),
            _ => None,
        }
    }

    /// Stable machine-readable code for *every* variant, using the Issue #3234
    /// vocabulary. Used to stamp `aletheiadb.error.code` onto the trace span
    /// (Issue #3376); the response body still only carries [`code`](Self::code)
    /// for auth errors so the existing wire shape is unchanged.
    #[must_use]
    pub(crate) fn code_str(&self) -> &'static str {
        match self {
            Self::BadRequest(_) | Self::QueryParse(_) => "INVALID_ARGUMENT",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Internal(_) | Self::StateMissing => "INTERNAL",
            Self::Unauthorized => "UNAUTHENTICATED",
            Self::PermissionDenied(_) => "PERMISSION_DENIED",
        }
    }

    /// Convert to a response, additively including the active trace id
    /// (Issue #3376) as a `trace_id` body field and `x-trace-id` header when
    /// one is available, so a failing call can be looked up in the trace
    /// backend directly.
    #[must_use]
    pub(crate) fn into_response_with_trace(self, trace_id: Option<String>) -> Response {
        let status = self.status();
        let mut body = match self.code() {
            // Auth errors additively carry a stable `code` field.
            Some(code) => json!({
                "success": false,
                "error": self.to_string(),
                "code": code,
            }),
            None => json!({
                "success": false,
                "error": self.to_string(),
            }),
        };
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
        if let Some(tid) = trace_id.and_then(|t| axum::http::HeaderValue::from_str(&t).ok()) {
            response
                .headers_mut()
                .insert(axum::http::HeaderName::from_static("x-trace-id"), tid);
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
