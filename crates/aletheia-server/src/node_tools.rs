//! Node-read proof slice: `get_node` / `list_nodes` / `count_nodes` (Issue #3524).
//!
//! Each is a single `#[get(...)]` + `#[api_doc(description=..., mcp)]` handler,
//! so one definition surfaces on **HTTP**, **MCP-over-HTTP** (`/mcp`), and
//! **OpenAPI** at once. The response body is produced by the *exact* main-crate
//! MCP typed method ([`AletheiaMcpServer::get_node`]/`list_nodes`/`count_nodes`),
//! so it is byte-identical to the legacy MCP surface (bare entity JSON + the
//! `temporal` block, #3220 vector elision) — asserted in the parity suite
//! against a fresh [`AletheiaMcpServer`] over the same `Arc<AletheiaDB>`.
//!
//! These reuse **already-public** symbols
//! (`aletheiadb::mcp::{AletheiaMcpServer, GetNodeRequest, ListNodesRequest,
//! CountNodesRequest}`), so PR1 requires **no** main-crate visibility widening.
//!
//! The MCP tool result carries in-band errors (e.g. a missing node →
//! `{"error":{"code":"NOT_FOUND",...}}`); PR1 returns that JSON verbatim with a
//! 200, matching the MCP method's `String` return exactly (unifying HTTP status
//! semantics with the legacy HTTP envelope is Lane B / a follow-up).

use crate::security::{Authorized, ReadClass};
use crate::state::ServerState;
use aletheiadb::mcp::{AletheiaMcpServer, CountNodesRequest, GetNodeRequest, ListNodesRequest};
use autumn_web::prelude::{Json, Path, Query, get};
use serde::Deserialize;
use serde_json::Value;

/// Parse an MCP tool method's JSON string result into a [`Value`] for the HTTP
/// response. A non-JSON result (should not happen) degrades to a JSON string.
fn tool_json(s: String) -> Json<Value> {
    Json(serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)))
}

/// Query options for [`get_node`].
#[derive(Debug, Default, Deserialize)]
pub struct GetNodeQuery {
    /// Return full vector/embedding arrays instead of the elided descriptor.
    pub include_vectors: Option<bool>,
}

/// `get_node` — fetch a node by id, with bi-temporal bounds. HTTP + MCP tool.
#[get("/nodes/{id}")]
#[api_doc(
    description = "Fetch a node by id, returning its properties and bi-temporal bounds",
    mcp
)]
pub async fn get_node(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Path(id): Path<u64>,
    Query(opts): Query<GetNodeQuery>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    let out = server.get_node(GetNodeRequest {
        node_id: id,
        include_vectors: opts.include_vectors,
    });
    tool_json(out)
}

/// Query options for [`list_nodes`].
#[derive(Debug, Default, Deserialize)]
pub struct ListNodesQuery {
    /// Filter by node label.
    pub label: Option<String>,
    /// Filter by property key (with `label` + `property_value`).
    pub property_key: Option<String>,
    /// Filter by property value (string form on the HTTP query surface).
    pub property_value: Option<String>,
    /// Maximum number of nodes to return.
    pub limit: Option<usize>,
    /// Number of nodes to skip.
    pub offset: Option<usize>,
    /// Return full vector/embedding arrays instead of the elided descriptor.
    pub include_vectors: Option<bool>,
}

/// `list_nodes` — list nodes with optional label/property filtering + paging.
/// HTTP + MCP tool.
#[get("/nodes")]
#[api_doc(
    description = "List nodes with optional label/property filtering and pagination",
    mcp
)]
pub async fn list_nodes(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Query(opts): Query<ListNodesQuery>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    let out = server.list_nodes(ListNodesRequest {
        label: opts.label,
        property_key: opts.property_key,
        property_value: opts.property_value.map(Value::String),
        limit: opts.limit,
        offset: opts.offset,
        include_vectors: opts.include_vectors,
    });
    tool_json(out)
}

/// Query options for [`count_nodes`].
#[derive(Debug, Default, Deserialize)]
pub struct CountNodesQuery {
    /// Count only nodes with this label (else all nodes).
    pub label: Option<String>,
}

/// `count_nodes` — total node count, or count matching a label. HTTP + MCP tool.
#[get("/nodes/count")]
#[api_doc(
    description = "Count nodes in the graph, or nodes matching a specific label",
    mcp
)]
pub async fn count_nodes(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Query(opts): Query<CountNodesQuery>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    let out = server.count_nodes(CountNodesRequest { label: opts.label });
    tool_json(out)
}
