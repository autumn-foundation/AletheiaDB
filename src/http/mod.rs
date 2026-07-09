//! HTTP server module for AletheiaDB (Issue #465).
//!
//! Exposes a thin JSON API over [`autumn-web`](autumn_web), which itself
//! wraps [`axum`]. Endpoints:
//!
//! - `GET  /status` — liveness probe, returns `{"status":"healthy"}`.
//! - `POST /query`  — polymorphic JSON operations against AletheiaDB.
//! - `POST /admin/keys`, `GET /admin/keys`, `POST /admin/keys/revoke` —
//!   API-key lifecycle (admin role required; Issue #3350).
//!
//! See [`handlers::QueryRequest`] for the payload shape.
//!
//! # Authentication (Issue #3350)
//!
//! Authentication defaults to required ([`crate::auth::AuthMode::Required`]):
//! requests present an API key via `Authorization: Bearer <key>` or
//! `x-api-key`, and each operation is authorized against the key's role.
//! Anonymous operation is an explicit opt-in
//! ([`crate::auth::AuthMode::Anonymous`]). See [`crate::auth`] for the core
//! model and [`auth::AuthContext`] for the extractor.
//!
//! # Example
//!
//! ```ignore
//! use aletheiadb::http::{ServerConfig, run_server};
//!
//! #[autumn_web::main]
//! async fn main() -> std::io::Result<()> {
//!     let config = ServerConfig::builder().port(1963).build();
//!     run_server(config).await
//! }
//! ```

pub mod admin;
pub mod auth;
mod config;
pub(crate) mod converters;
mod error;
pub mod handlers;
mod server;
mod state;

pub use auth::{AuthContext, AuthState, validate_auth_startup};
pub use config::{
    CorsConfig, DEFAULT_MAX_REQUEST_BODY_BYTES, EffectiveQueryLimits, LimitDimension,
    LimitOverrideError, QueryLimitsConfig, QueryLimitsOverride, RateLimitConfig, RowOverflowPolicy,
    ServerConfig, ServerConfigBuilder,
};
pub use error::AletheiaHttpError;
pub use handlers::{ApiResponse, QueryRequest, handle_query, health_check};
pub use server::{build_test_router, build_test_router_with_auth, run_server};
pub use state::AppState;
