//! HTTP server module for GallifreyDB (Issue #465)
//!
//! This module provides an HTTP server using Actix-web for REST API access
//! to GallifreyDB functionality.
//!
//! # Features
//!
//! - Health endpoint at `GET /status`
//! - CORS support for cross-origin requests
//! - Request logging middleware
//! - Graceful shutdown handling
//! - Configurable port and host binding
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::http::{ServerConfig, run_server};
//!
//! #[actix_web::main]
//! async fn main() -> std::io::Result<()> {
//!     let config = ServerConfig::builder()
//!         .port(8080)
//!         .host("127.0.0.1")
//!         .build();
//!
//!     run_server(config).await
//! }
//! ```

mod config;
mod handlers;
mod server;

pub use config::{CorsConfig, ServerConfig, ServerConfigBuilder};
pub use handlers::health_check;
pub use server::{ShutdownHandle, configure_app, create_app, create_server, run_server};
