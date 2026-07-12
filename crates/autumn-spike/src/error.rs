//! Structured error envelope for the migration spike.
//!
//! Reuses AletheiaDB's **flat** HTTP error shape
//! (`{success, error, code, retriable, details?}` — see
//! [`aletheiadb::http::AletheiaHttpError`]) and the Issue #3234 structured-code
//! vocabulary (`UNAUTHENTICATED`, `PERMISSION_DENIED`, `NOT_FOUND`,
//! `INVALID_ARGUMENT`, `INTERNAL`).
//!
//! One deliberate enrichment over today's `AletheiaHttpError`: the
//! [`PermissionDenied`](SpikeError::PermissionDenied) variant carries
//! `details.required_class` / `details.principal_role`, matching the **MCP**
//! surface's `PERMISSION_DENIED` payload (`src/mcp/auth.rs`). The current HTTP
//! `AletheiaHttpError::PermissionDenied` omits those details — a gap a full
//! port would close so the two surfaces converge (see the spike doc's
//! Findings).

use aletheiadb::auth::{AccessClass, Role};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Errors raised by the spike's `GET /nodes/{id}` handler and its extractors.
#[derive(Debug)]
pub enum SpikeError {
    /// Authentication failed: missing, malformed, unknown, or revoked
    /// credential. Rendered as a **uniform** 401 (callers cannot distinguish
    /// the sub-cause), with `WWW-Authenticate: Bearer`.
    Unauthenticated,
    /// The authenticated principal's role does not permit the required class.
    PermissionDenied {
        /// The access class the operation required (e.g. `Read`).
        required_class: AccessClass,
        /// The principal's actual role.
        principal_role: Role,
    },
    /// A node lookup the caller reasonably expected to succeed did not.
    NotFound(String),
    /// Request validation failed (e.g. an id exceeding `MAX_VALID_ID`).
    InvalidArgument(String),
    /// An unexpected backend or serialization failure.
    Internal(String),
}

impl SpikeError {
    fn status(&self) -> StatusCode {
        match self {
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::PermissionDenied { .. } => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::InvalidArgument(_) => StatusCode::BAD_REQUEST,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The stable Issue #3234 machine-readable code.
    fn code(&self) -> &'static str {
        match self {
            Self::Unauthenticated => "UNAUTHENTICATED",
            Self::PermissionDenied { .. } => "PERMISSION_DENIED",
            Self::NotFound(_) => "NOT_FOUND",
            Self::InvalidArgument(_) => "INVALID_ARGUMENT",
            Self::Internal(_) => "INTERNAL",
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Unauthenticated => "authentication required".to_owned(),
            Self::PermissionDenied {
                required_class,
                principal_role,
            } => format!("role '{principal_role}' does not permit {required_class} access"),
            Self::NotFound(m) | Self::InvalidArgument(m) | Self::Internal(m) => m.clone(),
        }
    }
}

impl std::fmt::Display for SpikeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for SpikeError {}

impl IntoResponse for SpikeError {
    fn into_response(self) -> Response {
        // Flat envelope, mirroring `AletheiaHttpError` (Issue #3234 vocabulary).
        let mut body = json!({
            "success": false,
            "error": self.message(),
            "code": self.code(),
            // Every spike error class is a caller fault or terminal — none is
            // transient, so `retriable` is always false (matching the MCP
            // contract for these codes).
            "retriable": false,
        });
        if let Self::PermissionDenied {
            required_class,
            principal_role,
        } = &self
        {
            // Matches the MCP surface's PERMISSION_DENIED details exactly
            // (`src/mcp/auth.rs`): lowercase class/role strings.
            body["details"] = json!({
                "required_class": required_class.to_string(),
                "principal_role": principal_role.to_string(),
            });
        }
        let mut response = (self.status(), Json(body)).into_response();
        if matches!(self, Self::Unauthenticated) {
            response.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
        }
        response
    }
}
