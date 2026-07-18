//! Production autumn-web 0.5.0 server for AletheiaDB (Issue #3524, PR1 skeleton).
//!
//! This crate is the crate-boundary-staged (§5.0 of the migration design) home
//! for AletheiaDB's unified HTTP + MCP + OpenAPI surface on autumn-web **0.5**.
//! It depends on autumn-web 0.5 **unrenamed** (so the 0.5 proc-macros'
//! hard-coded `::autumn_web` paths resolve) plus a path-dep on `aletheiadb`,
//! keeping the main crate's 0.4 surface and gates entirely untouched.
//!
//! # PR1 scope
//!
//! - The production crate skeleton + app assembly ([`app`]): `routes` +
//!   `.openapi(...)` (`/openapi.json`) + `.mount_mcp("/mcp")`.
//! - The Lane-B-owned [`security`] seam (scaffolds Lane A stands up and Lane B
//!   fills in): the class-parameterized [`security::Authorized`] extractor, the
//!   `apply_security`/`init_state`/`validate_startup` seam functions, and empty
//!   `rate_limit`/`resource_limits`/`cursor` modules.
//! - Four of the five HTTP routes ported byte-identically ([`http_routes`]):
//!   `/status`, `POST/GET /admin/keys`, `POST /admin/keys/revoke`. The
//!   polymorphic `POST /query` is a larger, stateful port deferred to a
//!   follow-up.
//! - The node-read proof slice ([`node_tools`]): `get_node` / `list_nodes` /
//!   `count_nodes` on HTTP **and** `/mcp`, reusing the main crate's MCP typed
//!   methods so output is byte-identical to the legacy MCP surface.
//!
//! Behavior is pinned by the parity suite in `tests/`, which asserts
//! byte-equality against the legacy 0.4 HTTP router and the legacy MCP server.

#![warn(missing_docs)]

pub mod app;
pub mod changefeed_stream;
pub mod constraints_lineage_audit_tools;
pub mod crypto_shred_tools;
pub mod edge_tools;
pub mod http_routes;
pub mod metrics_exposition;
pub mod namespace_tools;
pub mod node_tools;
pub mod schema_batch_tools;
pub mod security;
pub mod state;
pub mod traverse_temporal_tools;
pub mod vector_query_tools;

pub use app::{
    build_server_client, build_server_client_with_config, build_server_testapp,
    try_build_server_client, try_build_server_testapp,
};
pub use security::{SecurityConfig, ServerSecurityState};
pub use state::ServerState;
