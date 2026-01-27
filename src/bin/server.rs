//! GallifreyDB HTTP Server Binary
//!
//! This binary launches the GallifreyDB HTTP server, providing a REST API
//! for interacting with the database.
//!
//! # Usage
//!
//! ```bash
//! # Run with default settings (port 8080, restrictive CORS)
//! cargo run --bin gallifrey-server --features http-server
//!
//! # Run with custom port
//! GALLIFREYDB_PORT=3000 cargo run --bin gallifrey-server --features http-server
//!
//! # Run with permissive CORS (development only!)
//! GALLIFREYDB_CORS_PERMISSIVE=true cargo run --bin gallifrey-server --features http-server
//!
//! # Health check
//! curl http://localhost:8080/status
//! ```
//!
//! # Environment Variables
//!
//! - `GALLIFREYDB_PORT`: Port to listen on (default: 8080)
//! - `GALLIFREYDB_HOST`: Host to bind to (default: 0.0.0.0)
//! - `GALLIFREYDB_CORS_PERMISSIVE`: Set to "true" to allow any origin (default: false)
//! - `GALLIFREYDB_CORS_ORIGINS`: Comma-separated list of allowed origins
//!
//! # Endpoints
//!
//! - `GET /status` - Health check endpoint, returns `{"status": "healthy"}`
//!
//! # Security Notes
//!
//! - By default, CORS is restrictive (only localhost:3000 allowed)
//! - Set `GALLIFREYDB_CORS_ORIGINS` to configure allowed origins for production
//! - Only use `GALLIFREYDB_CORS_PERMISSIVE=true` in development environments
//!
//! # Graceful Shutdown
//!
//! The server handles SIGTERM and SIGINT signals for graceful shutdown,
//! allowing in-flight requests to complete before terminating.

use gallifreydb::http::{CorsConfig, ServerConfig, run_server};
use std::env;

/// Parse port from environment variable with warning on invalid values.
fn parse_port() -> u16 {
    match env::var("GALLIFREYDB_PORT") {
        Ok(port_str) => match port_str.parse::<u16>() {
            Ok(port) => port,
            Err(e) => {
                eprintln!(
                    "WARNING: Invalid GALLIFREYDB_PORT '{}': {}. Using default port 8080.",
                    port_str, e
                );
                8080
            }
        },
        Err(_) => 8080,
    }
}

/// Parse host from environment variable.
fn parse_host() -> String {
    env::var("GALLIFREYDB_HOST").unwrap_or_else(|_| "0.0.0.0".to_string())
}

/// Parse CORS configuration from environment variables.
fn parse_cors_config() -> CorsConfig {
    // Check if permissive mode is enabled
    let is_permissive = env::var("GALLIFREYDB_CORS_PERMISSIVE")
        .map(|v| v.to_lowercase() == "true" || v == "1")
        .unwrap_or(false);

    if is_permissive {
        return CorsConfig::permissive();
    }

    // Parse allowed origins from comma-separated list
    match env::var("GALLIFREYDB_CORS_ORIGINS") {
        Ok(origins_str) => {
            let mut config = CorsConfig::restrictive();
            for origin in origins_str
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                config = config.allow_origin(origin);
            }
            config
        }
        Err(_) => CorsConfig::restrictive(),
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let port = parse_port();
    let host = parse_host();
    let cors = parse_cors_config();

    let config = ServerConfig::builder()
        .port(port)
        .host(host)
        .cors(cors)
        .build();

    run_server(config).await
}
