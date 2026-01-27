//! GallifreyDB HTTP Server Binary
//!
//! This binary launches the GallifreyDB HTTP server, providing a REST API
//! for interacting with the database.
//!
//! # Usage
//!
//! ```bash
//! # Run with default settings (port 8080)
//! cargo run --bin gallifrey-server --features http-server
//!
//! # The server will start and listen for requests
//! # Health check: curl http://localhost:8080/status
//! ```
//!
//! # Environment Variables
//!
//! - `GALLIFREYDB_PORT`: Port to listen on (default: 8080)
//! - `GALLIFREYDB_HOST`: Host to bind to (default: 0.0.0.0)
//!
//! # Endpoints
//!
//! - `GET /status` - Health check endpoint, returns `{"status": "healthy"}`
//!
//! # Graceful Shutdown
//!
//! The server handles SIGTERM and SIGINT signals for graceful shutdown,
//! allowing in-flight requests to complete before terminating.

use gallifreydb::http::{ServerConfig, run_server};
use std::env;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Read configuration from environment variables
    let port: u16 = env::var("GALLIFREYDB_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    let host = env::var("GALLIFREYDB_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

    let config = ServerConfig::builder().port(port).host(host).build();

    run_server(config).await
}
