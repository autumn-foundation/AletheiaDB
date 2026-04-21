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
}

impl AletheiaHttpError {
    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) | Self::QueryParse(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Internal(_) | Self::StateMissing => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AletheiaHttpError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = json!({
            "success": false,
            "error": self.to_string(),
        });
        (status, Json(body)).into_response()
    }
}
