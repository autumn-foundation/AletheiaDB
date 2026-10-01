//! The `aletheia-daemon` binary — one process that owns the local AletheiaDB
//! and serves CLI, HTTP, MCP, and SDK workflows from it (Issue #2905).
//!
//! # Usage
//!
//! ```bash
//! # Normally started via the CLI, which manages the pid file and logs:
//! aletheia daemon start
//!
//! # Or directly:
//! ALETHEIADB_DATA_DIR=~/.aletheiadb \
//! ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$(openssl rand -base64 32)" \
//!   aletheia-daemon
//! ```
//!
//! # Environment
//!
//! | Variable | Meaning | Default |
//! |---|---|---|
//! | `ALETHEIADB_DATA_DIR` | Directory the daemon owns (WAL + indexes + keys) | — (in-memory) |
//! | `ALETHEIADB_CONFIG` | TOML config path (used when no data dir is set) | — |
//! | `ALETHEIADB_HOST` | Bind address | `127.0.0.1` |
//! | `ALETHEIADB_PORT` | Port | `1963` |
//! | `ALETHEIADB_AUTH_MODE` | `required` (default) or `anonymous` | `required` |
//! | `ALETHEIADB_BOOTSTRAP_ADMIN_KEY` | Memory-only bootstrap admin key | — |
//!
//! # Endpoints
//!
//! `/mcp` (MCP over Streamable HTTP), the REST routes, `/openapi.json`,
//! `/status`, and `/metrics`. MCP clients either speak HTTP directly or run
//! `aletheia-mcp` with `ALETHEIADB_DAEMON_URL` set as a stdio proxy.

use aletheia_server::daemon::{DaemonConfig, run_server};

#[tokio::main]
async fn main() {
    if let Err(message) = run_server(DaemonConfig::from_env()).await {
        eprintln!("aletheia-daemon failed to start: {message}");
        std::process::exit(1);
    }
}
