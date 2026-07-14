//! Node tools: the read proof slice (`get_node` / `list_nodes` / `count_nodes`)
//! plus the node-write cluster (`create_node` / `update_node` / `delete_node` /
//! `delete_node_cascade` / `retract_node`) and the point-in-time node finder
//! `find_nodes_at_time` (Issue #3524, PR2).
//!
//! Each is a single `#[get(...)]`/`#[post(...)]` + `#[api_doc(description=...,
//! mcp)]` handler, so one definition surfaces on **HTTP**, **MCP-over-HTTP**
//! (`/mcp`), and **OpenAPI** at once. The response body is produced by the
//! *exact* main-crate MCP typed method
//! ([`AletheiaMcpServer::get_node`]/`create_node`/`update_node`/…), so it is
//! byte-identical to the legacy MCP surface (bare entity JSON + the `temporal`
//! block #3232, #3220 vector elision, the #3209 DETACH refuse/detach contract on
//! `delete_node`, the #3230 retraction contract, #3221 `valid_time`, and #3236
//! point-in-time reconstruction) — asserted in the parity suite against a fresh
//! [`AletheiaMcpServer`] over the same `Arc<AletheiaDB>`.
//!
//! These reuse **already-public** symbols
//! (`aletheiadb::mcp::{AletheiaMcpServer, GetNodeRequest, ListNodesRequest,
//! CountNodesRequest, CreateNodeRequest, UpdateNodeRequest, DeleteNodeRequest,
//! DeleteNodeCascadeRequest, RetractNodeRequest, FindNodesAtTimeRequest}`), so no
//! main-crate visibility widening is required.
//!
//! The MCP tool result carries in-band errors (e.g. a missing node →
//! `{"error":{"code":"NOT_FOUND",...}}`); we return that JSON verbatim with a
//! 200, matching the MCP method's `String` return exactly (unifying HTTP status
//! semantics with the legacy HTTP envelope is a follow-up).
//!
//! The five write tools are classified [`WriteClass`] and the read finder
//! [`ReadClass`]; the marker rides in each handler's `Authorized<C>` signature
//! (the one place the class is declared), which autumn replays for `/mcp
//! tools/call`. The write handlers take their arguments as a JSON **body**
//! (autumn maps `Json<T>` to the reserved `body` MCP argument key), so the
//! `tools/call` `arguments` wrap the request under `"body"`.

use crate::security::resource_limits::{check_byte_cap_of, run_with_timeout};
use crate::security::{Authorized, ReadClass, ServerSecurityState, WriteClass};
use crate::state::ServerState;
use aletheiadb::http::AletheiaHttpError;
use aletheiadb::mcp::{
    AletheiaMcpServer, CountNodesRequest, CreateNodeRequest, DeleteNodeCascadeRequest,
    DeleteNodeRequest, FindNodesAtTimeRequest, GetNodeRequest, ListNodesRequest,
    RetractNodeRequest, UpdateNodeRequest,
};
use autumn_web::prelude::{Json, Path, Query, get, post};
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
    security: ServerSecurityState,
    state: ServerState,
    Query(opts): Query<ListNodesQuery>,
) -> Result<Json<Value>, AletheiaHttpError> {
    // B4 resource-limits wiring (#3542 / #3550): a bounded in-flight admission
    // guard (503 UNAVAILABLE at capacity, released by RAII on any exit), a
    // per-query wall-clock timeout (429 RESOURCE_EXHAUSTED on deadline), and a
    // serialized-byte cap on the exact wire response (413 RESOURCE_EXHAUSTED),
    // all reusing the shared `AletheiaHttpError` envelope. Generous defaults
    // (cfg) make this a no-op on the parity path; a tiny configured cap makes
    // the 413/503 boundaries observable.
    let limits = security.limits();
    let _guard = security.in_flight().try_acquire()?;

    let db = state.db_arc();
    let request = ListNodesRequest {
        label: opts.label,
        property_key: opts.property_key,
        property_value: opts.property_value.map(Value::String),
        limit: opts.limit,
        offset: opts.offset,
        include_vectors: opts.include_vectors,
    };
    let out = run_with_timeout(limits.timeout, false, async move {
        AletheiaMcpServer::new(db).list_nodes(request)
    })
    .await?;

    let value = serde_json::from_str::<Value>(&out).unwrap_or(Value::String(out));
    // Cap against the exact serialized response bytes (undercount-proof).
    check_byte_cap_of(&value, limits.max_response_bytes)?;
    Ok(Json(value))
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

// ════════════════════════════════════════════════════════════════════════════
// Node-write cluster (Issue #3524, PR2): create / update / delete /
// delete_cascade / retract — all [`WriteClass`], plus the [`ReadClass`]
// point-in-time finder `find_nodes_at_time`.
//
// Each handler forwards its request body to the exact legacy MCP typed method
// and returns that method's `String` verbatim, so every contract those methods
// implement — #3221 `valid_time`, #3209 DETACH refuse/detach, #3230 retraction
// idempotency, #3232 temporal block, #3220 vector elision, #3236 point-in-time
// reconstruction — is preserved byte-for-byte with **zero** reserialization.
// ════════════════════════════════════════════════════════════════════════════

/// `create_node` — create a node with properties and optional bi-temporal
/// `valid_time` (#3221), provenance, and derivation lineage (#3371). HTTP + MCP
/// tool. The response carries the new node with its `temporal` block (#3232).
#[post("/nodes")]
#[api_doc(
    description = "Create a node with properties and optional valid_time, provenance, and lineage",
    mcp
)]
pub async fn create_node(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<CreateNodeRequest>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.create_node(req))
}

/// `update_node` — replace a node's properties, recording a new bi-temporal
/// version (optional `valid_time` #3221, provenance, lineage). HTTP + MCP tool.
#[post("/nodes/update")]
#[api_doc(
    description = "Update a node's properties, recording a new version with optional valid_time",
    mcp
)]
pub async fn update_node(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<UpdateNodeRequest>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.update_node(req))
}

/// `delete_node` — safe-by-default delete (#3209): refuses with
/// `connected_edges` unless `detach: true`, which cascade-deletes and reports
/// `edges_removed`. Optional `valid_time` (#3221; not with `detach`). HTTP + MCP.
#[post("/nodes/delete")]
#[api_doc(
    description = "Delete a node; refuses if it has edges unless detach:true (DETACH DELETE contract)",
    mcp
)]
pub async fn delete_node(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<DeleteNodeRequest>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.delete_node(req))
}

/// `delete_node_cascade` — atomically delete a node and every connected edge,
/// preventing orphans. HTTP + MCP tool.
#[post("/nodes/delete_cascade")]
#[api_doc(
    description = "Delete a node and all its connected edges atomically (cascade delete)",
    mcp
)]
pub async fn delete_node_cascade(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<DeleteNodeCascadeRequest>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.delete_node_cascade(req))
}

/// `retract_node` — close a node's valid-time interval without deleting history
/// (#3230): refuses with `connected_edges` unless `detach: true` (co-retracts
/// edges, reports `edges_retracted`); re-retraction is an idempotent no-op.
/// HTTP + MCP tool.
#[post("/nodes/retract")]
#[api_doc(
    description = "Retract a node (close its valid-time interval) without deleting history",
    mcp
)]
pub async fn retract_node(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<RetractNodeRequest>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.retract_node(req))
}

/// `find_nodes_at_time` — resolve nodes by label (+ optional exact property
/// match) as they existed at a bi-temporal point (#3236), reconstructing each
/// node's state at `(valid_time, transaction_time)`. Read-only ([`ReadClass`]);
/// vectors elided by default (#3220). HTTP + MCP tool.
#[post("/nodes/find_at_time")]
#[api_doc(
    description = "Find nodes by label/property as of a bi-temporal point, reconstructed at that time",
    mcp
)]
pub async fn find_nodes_at_time(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<FindNodesAtTimeRequest>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.find_nodes_at_time(req))
}
