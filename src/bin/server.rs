//! AletheiaDB HTTP Server Binary
//!
//! This binary launches the AletheiaDB HTTP server, providing a REST API
//! for interacting with the database.
//!
//! # Usage
//!
//! ```bash
//! # Run with default settings (port 1963, restrictive CORS)
//! cargo run --bin aletheia-server --features http-server
//!
//! # Run with custom port
//! ALETHEIADB_PORT=3000 cargo run --bin aletheia-server --features http-server
//!
//! # Run with permissive CORS (development only!)
//! ALETHEIADB_CORS_PERMISSIVE=true cargo run --bin aletheia-server --features http-server
//!
//! # Health check
//! curl http://localhost:1963/status
//! ```
//!
//! # Environment Variables
//!
//! - `ALETHEIADB_PORT`: Port to listen on (default: 1963)
//! - `ALETHEIADB_HOST`: Host to bind to (default: 0.0.0.0)
//! - `ALETHEIADB_CORS_PERMISSIVE`: Set to "true" to allow any origin (default: false)
//! - `ALETHEIADB_CORS_ORIGINS`: Comma-separated list of allowed origins
//! - `ALETHEIADB_DATA_DIR`: Directory for WAL + index persistence. When set,
//!   the server persists state across restarts. When unset, runs in-memory
//!   (ephemeral — useful for demos, useless for production).
//!
//! # Endpoints
//!
//! - `GET /status` - Health check endpoint, returns `{"status": "healthy"}`
//!
//! # Security Notes
//!
//! - By default, CORS is restrictive (only localhost:3000 allowed)
//! - Set `ALETHEIADB_CORS_ORIGINS` to configure allowed origins for production
//! - Only use `ALETHEIADB_CORS_PERMISSIVE=true` in development environments
//!
//! # Graceful Shutdown
//!
//! The server handles SIGTERM and SIGINT signals for graceful shutdown,
//! allowing in-flight requests to complete before terminating.

use aletheiadb::http::{CorsConfig, ServerConfig, run_server};
use std::env;
use std::path::PathBuf;

/// Parse port from environment variable with warning on invalid values.
fn parse_port() -> u16 {
    match env::var("ALETHEIADB_PORT") {
        Ok(port_str) => match port_str.parse::<u16>() {
            Ok(port) => port,
            Err(e) => {
                eprintln!(
                    "WARNING: Invalid ALETHEIADB_PORT '{}': {}. Using default port 1963.",
                    port_str, e
                );
                1963
            }
        },
        Err(_) => 1963,
    }
}

/// Parse host from environment variable.
fn parse_host() -> String {
    env::var("ALETHEIADB_HOST").unwrap_or_else(|_| "0.0.0.0".to_string())
}

/// Parse CORS configuration from environment variables.
fn parse_cors_config() -> CorsConfig {
    // Check if permissive mode is enabled
    let is_permissive = env::var("ALETHEIADB_CORS_PERMISSIVE")
        .map(|v| v.to_lowercase() == "true" || v == "1")
        .unwrap_or(false);

    if is_permissive {
        return CorsConfig::permissive();
    }

    // Parse allowed origins from comma-separated list
    match env::var("ALETHEIADB_CORS_ORIGINS") {
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

/// Parse the data directory env var into an optional `PathBuf`.
///
/// Unset or empty → `None` (in-memory mode). Any non-empty value is taken
/// as a path literal; `AletheiaDB::with_unified_config` creates the directory
/// structure on startup.
fn parse_data_dir() -> Option<PathBuf> {
    env::var("ALETHEIADB_DATA_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
}

#[autumn_web::main]
async fn main() {
    let port = parse_port();
    let host = parse_host();
    let cors = parse_cors_config();
    let data_dir = parse_data_dir();

    let mut builder = ServerConfig::builder().port(port).host(host).cors(cors);
    if let Some(path) = data_dir {
        builder = builder.data_dir(path);
    }
    let config = builder.build();

    if let Err(e) = run_server(config).await {
        eprintln!("aletheia-server failed: {e}");
        std::process::exit(1);
    }
}
