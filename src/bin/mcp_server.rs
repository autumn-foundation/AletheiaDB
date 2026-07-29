//! AletheiaDB MCP Server binary.
//!
//! This binary exposes AletheiaDB's bi-temporal graph database capabilities
//! through the Model Context Protocol (MCP), enabling LLMs like Claude to
//! interact with the database.
//!
//! # Usage
//!
//! ```bash
//! cargo run --bin aletheia-mcp --features mcp-server
//! ```
//!
//! The server communicates over stdio using the MCP protocol.
//!
//! # Available Tools
//!
//! ## Node Operations
//! - `get_node`: Get a node by ID
//! - `create_node`: Create a new node with label and properties
//! - `update_node`: Update node properties
//! - `delete_node`: Delete a node
//! - `list_nodes`: List nodes with optional filtering
//! - `count_nodes`: Count nodes
//!
//! ## Edge Operations
//! - `get_edge`: Get an edge by ID
//! - `create_edge`: Create an edge between nodes
//! - `update_edge`: Update edge properties
//! - `delete_edge`: Delete an edge
//! - `list_edges`: List edges with optional filtering
//! - `count_edges`: Count edges
//! - `get_outgoing_edges`: Get outgoing edges from a node
//! - `get_incoming_edges`: Get incoming edges to a node
//!
//! ## Graph Traversal
//! - `traverse`: Traverse the graph from a starting node
//!
//! ## Vector Search
//! - `find_similar`: Find similar nodes by embedding
//! - `enable_vector_index`: Enable vector indexing on a property
//! - `list_vector_indexes`: List enabled vector indexes
//!
//! ## Temporal Queries
//! - `get_node_at_time`: Get node state at a specific time
//! - `get_edge_at_time`: Get edge state at a specific time
//!
//! ## Hybrid Queries
//! - `hybrid_query`: Combined graph + vector + temporal query
//!
//! ## Declarative Query
//! - `query`: Execute a single read-only Cypher/AQL statement (with optional
//!   `$param` bindings) and get structured rows back. Mutating statements are
//!   rejected before execution. Cypher requires the `cypher` feature; AQL is
//!   always available.
//!
//! # Authentication (Issue #3350)
//!
//! The MCP transport is stdio, so the credential is **session-scoped**:
//! supplied once at process start and re-verified on every tool call (a
//! revocation in the shared key store takes effect on the next call).
//!
//! Environment variables:
//!
//! - `ALETHEIADB_AUTH_MODE`: `required` (default) or `anonymous`. In
//!   `required` mode every tool call is authorized against the session
//!   principal's role, and the server refuses to start with zero
//!   credentials. `anonymous` disables authentication entirely (explicit
//!   opt-in; a prominent warning is printed to stderr). Invalid values fall
//!   back to `required` (fail-closed) with a warning.
//! - `ALETHEIADB_MCP_API_KEY`: The session credential. In `required` mode a
//!   missing/unknown/revoked key does not stop the server — every tool call
//!   then returns the uniform `UNAUTHENTICATED` error.
//! - `ALETHEIADB_BOOTSTRAP_ADMIN_KEY`: Plaintext admin key installed at
//!   startup as principal `bootstrap-admin` (role `admin`). Memory-only —
//!   re-supply it on every start, or use it once against the HTTP server's
//!   `POST /admin/keys` to mint persisted keys.
//! - Key persistence: when `ALETHEIADB_DATA_DIR` is set, keys are loaded
//!   from `{data_dir}/auth/keys.json` — the same path the HTTP server uses,
//!   so keys minted via the HTTP admin endpoints work here when both point
//!   at the same data directory.
//!
//! # Daemon-client mode (Issue #2905)
//!
//! Setting `ALETHEIADB_DAEMON_URL` (or passing `--daemon-url <URL>`) switches
//! this binary from *owning* a database to *relaying* to one:
//!
//! ```bash
//! ALETHEIADB_DAEMON_URL=http://127.0.0.1:1963 \
//! ALETHEIADB_MCP_API_KEY=$KEY \
//!   aletheia-mcp
//! ```
//!
//! In that mode the process never calls [`AletheiaDB::open_from_env`] and never
//! touches local storage: JSON-RPC frames read on stdin are forwarded to the
//! daemon's `/mcp` endpoint and its replies written back to stdout. Many MCP
//! client sessions can therefore share one database through one daemon instead
//! of each opening (or inventing) their own. The relay exits when its client
//! disconnects; the daemon is the long-lived process.
//!
//! Without a daemon URL the binary keeps its original embedded behavior, which
//! remains fully supported.
//!
//! See `docs/guides/daemon-mode.md`.

use std::sync::Arc;

use rmcp::{ServiceExt, transport::stdio};

use aletheiadb::AletheiaDB;
use aletheiadb::auth::{AuthMode, AuthStore, Role, SecretString, auth_mode_from_env};
use aletheiadb::mcp::proxy::{DaemonClientConfig, run_stdio_proxy};
use aletheiadb::mcp::{AletheiaMcpServer, McpAuthConfig, validate_mcp_auth_startup};

/// Read a secret-bearing env var into a redacted [`SecretString`], if set
/// and non-empty.
///
/// The value is trimmed: presented tokens are trimmed at the transport
/// layer, so an env value with stray whitespace could otherwise never
/// verify.
fn parse_secret_env(name: &str) -> Option<SecretString> {
    match std::env::var(name) {
        Ok(raw) if !raw.trim().is_empty() => Some(SecretString::new(raw.trim())),
        _ => None,
    }
}

/// Build the auth key store: persisted under `{data_dir}/auth/keys.json`
/// when `ALETHEIADB_DATA_DIR` is set (the same derivation the HTTP server
/// uses), memory-only otherwise. Installs the bootstrap admin key
/// (memory-only, never persisted) when configured.
fn build_auth_store() -> Result<AuthStore, String> {
    let store = match aletheiadb::config::data_dir_from_env() {
        Some(dir) => AuthStore::with_persistence(dir.join("auth").join("keys.json"))
            .map_err(|e| format!("failed to open auth key store: {e}"))?,
        None => AuthStore::new(),
    };
    if let Some(key) = parse_secret_env("ALETHEIADB_BOOTSTRAP_ADMIN_KEY") {
        store
            .insert_bootstrap_key("bootstrap-admin", Role::Admin, key.expose())
            .map_err(|e| format!("failed to install bootstrap admin key: {e}"))?;
    }
    Ok(store)
}

/// Relay stdio MCP traffic to a daemon, opening no local storage.
///
/// Authentication and authorization are the **daemon's** decisions here: the
/// session credential is forwarded as a bearer token and the daemon answers
/// `UNAUTHENTICATED` / `PERMISSION_DENIED` exactly as it would for a direct
/// HTTP client. Duplicating that logic in the relay could only ever disagree
/// with the authority, so it is deliberately absent.
async fn run_daemon_client(
    config: DaemonClientConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    eprintln!(
        "aletheia-mcp: daemon-client mode — relaying to {} (no local database is opened)",
        config.endpoint()
    );
    if !config.has_api_key() {
        eprintln!(
            "aletheia-mcp: no ALETHEIADB_MCP_API_KEY set; the daemon will reject \
             every call unless it runs in anonymous mode."
        );
    }

    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    run_stdio_proxy(config, stdin, tokio::io::stdout()).await?;
    // stdin EOF: the MCP client disconnected. Exiting here is what keeps proxy
    // processes from accumulating (and, on Windows, pinning the executable).
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Daemon-client mode is decided FIRST and never falls back: a proxy that
    // silently opened its own database when the daemon was unreachable would
    // recreate exactly the divergent-state bug this mode exists to prevent
    // (Issue #2905).
    let args: Vec<String> = std::env::args().skip(1).collect();
    match DaemonClientConfig::resolve(&args) {
        Ok(Some(config)) => return run_daemon_client(config).await,
        Ok(None) => {}
        Err(message) => {
            eprintln!("aletheia-mcp refusing to start: {message}");
            std::process::exit(1);
        }
    }

    // Embedded mode owns storage — so refuse a data directory a live daemon
    // already owns (Issue #2905). Without this check the most likely daemon
    // misconfiguration (a client left without ALETHEIADB_DAEMON_URL while
    // ALETHEIADB_DATA_DIR still points at the daemon's directory) silently
    // produces a second writer, which is the corruption mode daemon mode
    // exists to retire.
    if let Some(dir) = aletheiadb::config::data_dir_from_env()
        && let Some(pid) = aletheiadb::daemon_lock::live_owner(&dir)
    {
        eprintln!(
            "aletheia-mcp refusing to start: {}\nTo use the daemon, set \
             ALETHEIADB_DAEMON_URL (see docs/guides/daemon-mode.md).",
            aletheiadb::daemon_lock::already_owned_message(&dir, pid)
        );
        std::process::exit(1);
    }

    // Initialize the database. Persistence is selected by environment:
    //   ALETHEIADB_CONFIG=/path/to/config.toml   -> full TOML-driven config
    //   ALETHEIADB_DATA_DIR=/path                -> canonical durable layout
    //   (neither set)                            -> ephemeral tempdir-backed DB
    let db = AletheiaDB::open_from_env()?;

    // Resolve authentication (Issue #3350): required by default; the
    // session credential authenticates every tool call.
    let auth_mode = auth_mode_from_env();
    let store = build_auth_store()?;
    if let Err(msg) = validate_mcp_auth_startup(auth_mode, &store) {
        eprintln!("aletheia-mcp refusing to start: {msg}");
        std::process::exit(1);
    }
    let store = Arc::new(store);

    let session_key = parse_secret_env("ALETHEIADB_MCP_API_KEY");
    if auth_mode == AuthMode::Required
        && session_key
            .as_ref()
            .is_none_or(|key| store.verify(key.expose()).is_none())
    {
        eprintln!(
            "WARNING: no valid session credential was supplied \
             (ALETHEIADB_MCP_API_KEY is unset or not recognized by the key \
             store); every tool call will return UNAUTHENTICATED until the \
             server is restarted with a valid key."
        );
    }

    let mut auth = McpAuthConfig::new(auth_mode, store);
    if let Some(key) = session_key {
        auth = auth.with_credential(key);
    }

    // Create the MCP server (with_auth prints the prominent anonymous-mode
    // warning to stderr when applicable — stdout is the protocol channel).
    let server = AletheiaMcpServer::with_auth(Arc::new(db), auth);

    // Serve over stdio using the MCP protocol
    let service = server.serve(stdio()).await?;

    // Wait for the server to shut down
    service.waiting().await?;

    Ok(())
}
