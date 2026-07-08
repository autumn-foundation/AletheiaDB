//! HTTP server module for AletheiaDB (Issue #465).
//!
//! Exposes a thin JSON API over [`autumn-web`](autumn_web), which itself
//! wraps [`axum`]. Two endpoints:
//!
//! - `GET  /status` — liveness probe, returns `{"status":"healthy"}`.
//! - `POST /query`  — polymorphic JSON operations against AletheiaDB.
//!
//! See [`handlers::QueryRequest`] for the payload shape.
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

mod config;
pub(crate) mod converters;
mod error;
pub mod handlers;
mod server;
mod state;

pub use config::{
    CorsConfig, DEFAULT_MAX_REQUEST_BODY_BYTES, RateLimitConfig, ServerConfig, ServerConfigBuilder,
};
pub use error::AletheiaHttpError;
pub use handlers::{ApiResponse, QueryRequest, handle_query, health_check};
pub use server::{build_test_router, run_server};
pub use state::AppState;
