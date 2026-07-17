//! Traversal + temporal tools: the multi-hop `traverse` and the eleven
//! time-travel reads `get_node_history` / `get_edge_history` / `get_node_at_time`
//! / `get_edge_at_time` / `list_changes` / `get_node_at_valid_time` /
//! `get_node_at_transaction_time` / `diff_node_versions` /
//! `get_edge_at_valid_time` / `get_edge_at_transaction_time` /
//! `diff_edge_versions` (Issue #3524, PR4).
//!
//! Each is a single `#[get(...)]`/`#[post(...)]` + `#[api_doc(description=...,
//! mcp)]` handler, so one definition surfaces on **HTTP**, **MCP-over-HTTP**
//! (`/mcp`), and **OpenAPI** at once. The response body is produced by the
//! *exact* main-crate MCP surface, so it is byte-identical to the legacy MCP
//! surface (bare entity JSON + the `temporal` block #3232, #3220 vector elision
//! on the traversal read path, and #3225 point-in-time traversal semantics) —
//! asserted in the parity suite against the shared
//! [`AletheiaMcpServer`](aletheiadb::mcp::AletheiaMcpServer) over the same
//! `Arc<AletheiaDB>`.
//!
//! # Forwarding: dispatch-routed vs. typed methods vs. local-body dispatch
//!
//! The **single source of truth** for which reads honor the Issue #3353 token
//! budget is `aletheiadb::mcp`'s `BUDGETABLE_READ_TOOLS`, and for the Issue
//! #3360 cursor the `inject_cursor_schema_params` set. Verified against source,
//! of this cluster's twelve tools **only `traverse` and `get_node_history` are
//! budgetable**, and **only `traverse` is cursorable**. That split drives three
//! forwarding shapes:
//!
//! 1. **`traverse`** — budgetable + cursorable + potentially ms-scale. Forwards
//!    the raw JSON arguments through
//!    [`AletheiaMcpServer::dispatch_tool_json`](aletheiadb::mcp::AletheiaMcpServer::dispatch_tool_json)
//!    (so the #3353 budget and #3360 cursor pipelines, read off the raw
//!    arguments in `dispatch_tool`, apply) via the process-lifetime shared
//!    [`ServerState::mcp_server`](crate::state::ServerState::mcp_server) (so the
//!    cursor signing secret + live-cursor registry persist across the
//!    continuation requests). It runs under the **exact** B4 resource-limits
//!    wiring `list_nodes` uses — a bounded in-flight admission guard released by
//!    RAII on any exit, a per-query wall-clock [`run_with_timeout`], and a
//!    serialized-byte cap ([`check_byte_cap_of`]) — because a deep traversal is
//!    the one potentially-slow read in this cluster (mirroring the legacy HTTP
//!    `/query` execution model). `traverse` is registered in
//!    [`crate::edge_tools::DISPATCH_ROUTED_READ_TOOLS`] with the hardcoded literal
//!    `"traverse"`.
//! 2. **`get_node_history`** — budgetable (but NOT cursorable). Forwards raw
//!    arguments through `dispatch_tool_json` (budget only) so the #3353 budget
//!    applies. History reconstruction is fast, so it runs **inline** (no
//!    in-flight guard / timeout). Registered in `DISPATCH_ROUTED_READ_TOOLS` with
//!    the hardcoded literal `"get_node_history"`.
//! 3. **The other ten** — NOT budgetable, NOT cursorable. They forward through a
//!    **typed** path, inline (like the edge writes / `count_nodes`):
//!    - the four that have a public typed per-tool method
//!      (`get_node_at_time`, `get_edge_at_time`, `list_changes`,
//!      `get_edge_history`) take that method's re-exported request struct as the
//!      `Json<T>` body (or a `Path` id) and call
//!      `AletheiaMcpServer::get_node_at_time(req)` etc. directly — zero
//!      reserialization, using only already-public symbols;
//!    - the six that have **no** public typed method
//!      (`get_node_at_valid_time`, `get_node_at_transaction_time`,
//!      `diff_node_versions`, `get_edge_at_valid_time`,
//!      `get_edge_at_transaction_time`, `diff_edge_versions`) take a **local**
//!      typed body struct (mirroring the main-crate request shape, one-to-one, so
//!      `/openapi.json` gets a named per-operation component — never a generic
//!      `Value`) and forward through `dispatch_tool_json` with a hardcoded literal
//!      tool name. `dispatch_tool_json` is the *only* public entry for these six
//!      (their `handle_*` methods are private) and, being non-budgetable /
//!      non-cursorable, it reproduces a hypothetical typed method's output
//!      byte-for-byte (same `handle_*`). This keeps the migration free of any
//!      main-crate change for these six.
//!
//! All twelve are classified [`ReadClass`]; the marker rides in each handler's
//! `Authorized<C>` signature (the one place the class is declared), which autumn
//! replays for `/mcp tools/call`. The two dispatch-routed budgetable reads are
//! pinned with **hardcoded literal** tool names and registered in
//! `DISPATCH_ROUTED_READ_TOOLS` so the shared
//! `dispatch_pinned_names_match_routed_class` conformance test proves a
//! read-gated handler can never pin a write tool's name.
//!
//! The MCP tool result carries in-band errors (e.g. a missing node →
//! `{"error":{"code":"NOT_FOUND",...}}`); we return that JSON verbatim with a
//! 200, matching the MCP method's `String` return exactly.

use crate::edge_tools::{insert_budget, insert_opt, namespace_query_value};
use crate::security::resource_limits::{check_byte_cap_of, run_with_timeout};
use crate::security::{Authorized, ReadClass, ServerSecurityState};
use crate::state::ServerState;
use aletheiadb::http::AletheiaHttpError;
use aletheiadb::mcp::{
    GetEdgeAtTimeRequest, GetEdgeHistoryRequest, GetNodeAtTimeRequest, ListChangesRequest,
};
use autumn_web::prelude::{Json, Path, Query, get, post};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Parse an MCP tool method's JSON string result into a [`Value`] for the HTTP
/// response. A non-JSON result (should not happen) degrades to a JSON string.
fn tool_json(s: String) -> Json<Value> {
    Json(serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)))
}

// ════════════════════════════════════════════════════════════════════════════
// traverse — [`ReadClass`], budgetable (#3353) + cursorable (#3360) + the one
// potentially-slow read in this cluster. Forwards raw arguments through
// `dispatch_tool_json` (so budget + cursor apply) under the exact B4
// resource-limits wiring `list_nodes` uses.
// ════════════════════════════════════════════════════════════════════════════

/// Query options for [`traverse`] — every field optional so a #3360
/// cursor-continuation request (`?cursor=…`, with no `start_node_id` /
/// `edge_label`) still deserializes; requiredness of `start_node_id` /
/// `edge_label` on a fresh call stays a runtime concern the dispatch layer
/// enforces with a structured `INVALID_ARGUMENT`. Carries the traversal params
/// plus the #3353 token budget and the #3360 cursor parameters.
#[derive(Debug, Default, Deserialize)]
pub struct TraverseQuery {
    /// Starting node id for the traversal (logically required; enforced at
    /// dispatch, and superseded by the token on a cursor continuation).
    pub start_node_id: Option<u64>,
    /// Edge label to follow (logically required; enforced at dispatch).
    pub edge_label: Option<String>,
    /// Traversal direction: `outgoing` (default), `incoming`, or `both`.
    pub direction: Option<String>,
    /// Maximum traversal depth (default 1).
    pub depth: Option<usize>,
    /// Maximum number of results to return.
    pub limit: Option<usize>,
    /// Number of results to skip (offset pagination, #3226).
    pub offset: Option<usize>,
    /// Return full vector/embedding arrays instead of the elided descriptor.
    pub include_vectors: Option<bool>,
    /// #3225: valid time (ISO 8601 / RFC 3339 or microseconds since epoch) —
    /// switches to a bi-temporal, point-in-time traversal.
    pub as_of_valid_time: Option<String>,
    /// #3225: transaction time (ISO 8601 / RFC 3339 or microseconds since epoch).
    pub as_of_transaction_time: Option<String>,
    /// #3353: cap the response at roughly this many tokens (utf8_bytes / 4).
    pub max_response_tokens: Option<u64>,
    /// #3353: byte-exact response cap.
    pub max_response_bytes: Option<u64>,
    /// #3353: property keys to protect first as the response degrades.
    /// Comma-separated on the HTTP query surface (OpenAPI form / `explode=false`
    /// array), e.g. `?priority_properties=name,title`; entries are trimmed and
    /// empties dropped. See [`crate::edge_tools::de_priority_properties`].
    #[serde(
        default,
        deserialize_with = "crate::edge_tools::de_priority_properties"
    )]
    pub priority_properties: Option<Vec<String>>,
    /// #3360: request a snapshot-anchored cursor on the first page.
    pub use_cursor: Option<bool>,
    /// #3360: opaque continuation token from a prior page (passed back alone).
    pub cursor: Option<String>,
    /// #3349: namespace read scope — a single name, a comma-separated union
    /// (`a,b`), or `all`. Omitted = the `default` namespace only
    /// (isolated-by-default). Forwarded into dispatch so HTTP scopes identically
    /// to the MCP twin (a narrowing scope is outgoing-only; incoming/both with a
    /// narrowing scope is INVALID_ARGUMENT; a narrowing scope with `use_cursor`
    /// fails closed).
    pub namespace: Option<String>,
}

/// `traverse` — multi-hop graph traversal from a start node following edges of a
/// label, with optional #3225 bi-temporal `as_of_*` coordinates. [`ReadClass`];
/// vectors elided by default (#3220); budgetable (#3353) and cursorable (#3360).
/// HTTP + MCP tool.
///
/// Forwards raw arguments through `dispatch_tool_json` (so the #3353 budget and
/// #3360 cursor pipelines apply) under the B4 resource-limits wiring — a bounded
/// in-flight admission guard (503 at capacity, released by RAII on any exit), a
/// per-query wall-clock timeout (429 on deadline), and a serialized-byte cap
/// (413) — identical to `list_nodes`, because a deep traversal is the one
/// potentially-slow read here (mirroring the legacy `/query` execution model).
#[get("/traverse")]
#[api_doc(
    description = "Traverse the graph from a start node following edges of a label, optionally as of a bi-temporal point",
    mcp
)]
pub async fn traverse(
    _auth: Authorized<ReadClass>,
    security: ServerSecurityState,
    state: ServerState,
    Query(opts): Query<TraverseQuery>,
) -> Result<Json<Value>, AletheiaHttpError> {
    // B4 resource-limits wiring (#3542 / #3550), identical to `list_nodes`: a
    // bounded in-flight admission guard (503 UNAVAILABLE at capacity, released by
    // RAII on any exit), a per-query wall-clock timeout (429 on deadline), and a
    // serialized-byte cap on the exact wire response (413). Generous defaults
    // make this a no-op on the parity path.
    let limits = security.limits();
    let _guard = security.in_flight().try_acquire()?;

    // Route through the shared server's `dispatch_tool_json` so the #3353 token
    // budget and #3360 cursor paging (read off the raw arguments) apply — the
    // typed `traverse` method bypasses that pipeline. The cursor secret lives on
    // the process-lifetime shared server, so continuations resume correctly.
    let server = state.mcp_server();
    let mut args = Map::new();
    insert_opt(
        &mut args,
        "start_node_id",
        opts.start_node_id.map(Value::from),
    );
    insert_opt(&mut args, "edge_label", opts.edge_label.map(Value::from));
    insert_opt(&mut args, "direction", opts.direction.map(Value::from));
    insert_opt(&mut args, "depth", opts.depth.map(Value::from));
    insert_opt(&mut args, "limit", opts.limit.map(Value::from));
    insert_opt(&mut args, "offset", opts.offset.map(Value::from));
    insert_opt(
        &mut args,
        "include_vectors",
        opts.include_vectors.map(Value::from),
    );
    insert_opt(
        &mut args,
        "as_of_valid_time",
        opts.as_of_valid_time.map(Value::from),
    );
    insert_opt(
        &mut args,
        "as_of_transaction_time",
        opts.as_of_transaction_time.map(Value::from),
    );
    insert_budget(
        &mut args,
        opts.max_response_tokens,
        opts.max_response_bytes,
        opts.priority_properties,
    );
    insert_opt(&mut args, "use_cursor", opts.use_cursor.map(Value::from));
    insert_opt(&mut args, "cursor", opts.cursor.map(Value::from));
    insert_opt(
        &mut args,
        "namespace",
        namespace_query_value(opts.namespace),
    );
    let args = Value::Object(args);

    let out = run_with_timeout(limits.timeout, false, async move {
        server.dispatch_tool_json("traverse", args)
    })
    .await?;

    let value = serde_json::from_str::<Value>(&out).unwrap_or(Value::String(out));
    // Cap against the exact serialized response bytes (undercount-proof).
    check_byte_cap_of(&value, limits.max_response_bytes)?;
    Ok(Json(value))
}

// ════════════════════════════════════════════════════════════════════════════
// get_node_history — [`ReadClass`], budgetable (#3353) but NOT cursorable.
// Forwards raw arguments through `dispatch_tool_json` (budget only). Inline
// (history reconstruction is fast).
// ════════════════════════════════════════════════════════════════════════════

/// Query options for [`get_node_history`] / (shape-shared) the #3353 token
/// budget on a single-entity history read.
#[derive(Debug, Default, Deserialize)]
pub struct HistoryBudgetQuery {
    /// #3353: cap the response at roughly this many tokens (utf8_bytes / 4).
    pub max_response_tokens: Option<u64>,
    /// #3353: byte-exact response cap.
    pub max_response_bytes: Option<u64>,
    /// #3353: property keys to protect first as the response degrades.
    /// Comma-separated on the HTTP query surface (OpenAPI form / `explode=false`
    /// array), e.g. `?priority_properties=name,title`; entries are trimmed and
    /// empties dropped. See [`crate::edge_tools::de_priority_properties`].
    #[serde(
        default,
        deserialize_with = "crate::edge_tools::de_priority_properties"
    )]
    pub priority_properties: Option<Vec<String>>,
}

/// `get_node_history` — the complete version history of a node, oldest first.
/// [`ReadClass`]; budgetable (#3353), not cursorable. Forwards raw arguments
/// through `dispatch_tool_json` so the token budget applies. HTTP + MCP tool.
#[get("/nodes/{id}/history")]
#[api_doc(
    description = "Get the complete version history of a node, oldest first, with per-version provenance",
    mcp
)]
pub async fn get_node_history(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Path(id): Path<u64>,
    Query(opts): Query<HistoryBudgetQuery>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("node_id".to_string(), Value::from(id));
    insert_budget(
        &mut args,
        opts.max_response_tokens,
        opts.max_response_bytes,
        opts.priority_properties,
    );
    tool_json(server.dispatch_tool_json("get_node_history", Value::Object(args)))
}

// ════════════════════════════════════════════════════════════════════════════
// Typed-method reads (NOT budgetable, NOT cursorable): forward through the exact
// public typed per-tool method, inline, like the edge writes / count_nodes.
// ════════════════════════════════════════════════════════════════════════════

/// `get_node_at_time` — time-travel a node to a bi-temporal `(valid_time,
/// transaction_time)` point (#3232 temporal block). [`ReadClass`]. HTTP + MCP.
#[post("/nodes/at_time")]
#[api_doc(
    description = "Get a node's state at a specific valid time and transaction time (bi-temporal point-in-time read)",
    mcp
)]
pub async fn get_node_at_time(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<GetNodeAtTimeRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.get_node_at_time(req))
}

/// `get_edge_at_time` — time-travel an edge to a bi-temporal `(valid_time,
/// transaction_time)` point (#3232 temporal block). [`ReadClass`]. HTTP + MCP.
#[post("/edges/at_time")]
#[api_doc(
    description = "Get an edge's state at a specific valid time and transaction time (bi-temporal point-in-time read)",
    mcp
)]
pub async fn get_edge_at_time(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<GetEdgeAtTimeRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.get_edge_at_time(req))
}

/// `list_changes` — the nodes and edges whose versions were committed in a
/// transaction-time window `[tx_from, tx_to)`, with optional valid-time / label
/// filtering and stable cursor pagination. [`ReadClass`]. HTTP + MCP.
#[post("/changes")]
#[api_doc(
    description = "List nodes and edges whose versions were committed within a transaction-time window",
    mcp
)]
pub async fn list_changes(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<ListChangesRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.list_changes(req))
}

/// `get_edge_history` — the complete version history of an edge, oldest first.
/// [`ReadClass`]; NOT budgetable (so it forwards through the typed method, not
/// `dispatch_tool_json`). HTTP + MCP tool.
#[get("/edges/{id}/history")]
#[api_doc(
    description = "Get the complete version history of an edge, oldest first, with per-version provenance",
    mcp
)]
pub async fn get_edge_history(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Path(id): Path<u64>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.get_edge_history(GetEdgeHistoryRequest { edge_id: id }))
}

// ════════════════════════════════════════════════════════════════════════════
// Local-body dispatch reads (NOT budgetable, NOT cursorable, NO public typed
// method): a local typed body (mirroring the main-crate request shape one-to-one
// so /openapi.json gets a named per-operation component) forwarded through
// `dispatch_tool_json` with a hardcoded literal tool name — the only public
// entry for these six. Byte-identical to a hypothetical typed method (same
// `handle_*`), so no main-crate change is required.
// ════════════════════════════════════════════════════════════════════════════

/// Request body for [`get_node_at_valid_time`] — mirrors the main-crate
/// `GetNodeAtValidTimeRequest`.
#[derive(Debug, Deserialize)]
pub struct NodeAtValidTimeBody {
    /// The unique identifier of the node.
    pub node_id: u64,
    /// Valid time (ISO 8601 / RFC 3339): when the fact was true in reality.
    pub valid_time: String,
}

/// `get_node_at_valid_time` — a node's state at a specific valid time
/// (independent-dimension query, transaction time = now). [`ReadClass`]. HTTP +
/// MCP tool.
#[post("/nodes/at_valid_time")]
#[api_doc(
    description = "Get a node's state at a specific valid time (independent-dimension query)",
    mcp
)]
pub async fn get_node_at_valid_time(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<NodeAtValidTimeBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("node_id".to_string(), Value::from(req.node_id));
    args.insert("valid_time".to_string(), Value::from(req.valid_time));
    tool_json(server.dispatch_tool_json("get_node_at_valid_time", Value::Object(args)))
}

/// Request body for [`get_node_at_transaction_time`] — mirrors the main-crate
/// `GetNodeAtTransactionTimeRequest`.
#[derive(Debug, Deserialize)]
pub struct NodeAtTransactionTimeBody {
    /// The unique identifier of the node.
    pub node_id: u64,
    /// Transaction time (ISO 8601 / RFC 3339): when the fact was recorded.
    pub transaction_time: String,
}

/// `get_node_at_transaction_time` — a node's state at a specific transaction
/// time (independent-dimension query, valid time = now). [`ReadClass`]. HTTP +
/// MCP tool.
#[post("/nodes/at_transaction_time")]
#[api_doc(
    description = "Get a node's state at a specific transaction time (independent-dimension query)",
    mcp
)]
pub async fn get_node_at_transaction_time(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<NodeAtTransactionTimeBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("node_id".to_string(), Value::from(req.node_id));
    args.insert(
        "transaction_time".to_string(),
        Value::from(req.transaction_time),
    );
    tool_json(server.dispatch_tool_json("get_node_at_transaction_time", Value::Object(args)))
}

/// Request body for [`diff_node_versions`] — mirrors the main-crate
/// `DiffNodeVersionsRequest`.
#[derive(Debug, Deserialize)]
pub struct DiffNodeVersionsBody {
    /// The unique identifier of the node.
    pub node_id: u64,
    /// The id of the older version (from).
    pub from_version: u64,
    /// The id of the newer version (to).
    pub to_version: u64,
}

/// `diff_node_versions` — the property-level difference between two versions of
/// a node. [`ReadClass`]. HTTP + MCP tool.
#[post("/nodes/diff")]
#[api_doc(
    description = "Compute the property-level difference between two versions of a node",
    mcp
)]
pub async fn diff_node_versions(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<DiffNodeVersionsBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("node_id".to_string(), Value::from(req.node_id));
    args.insert("from_version".to_string(), Value::from(req.from_version));
    args.insert("to_version".to_string(), Value::from(req.to_version));
    tool_json(server.dispatch_tool_json("diff_node_versions", Value::Object(args)))
}

/// Request body for [`get_edge_at_valid_time`] — mirrors the main-crate
/// `GetEdgeAtValidTimeRequest`.
#[derive(Debug, Deserialize)]
pub struct EdgeAtValidTimeBody {
    /// The unique identifier of the edge.
    pub edge_id: u64,
    /// Valid time (ISO 8601 / RFC 3339): when the fact was true in reality.
    pub valid_time: String,
}

/// `get_edge_at_valid_time` — an edge's state at a specific valid time
/// (independent-dimension query, transaction time = now). [`ReadClass`]. HTTP +
/// MCP tool.
#[post("/edges/at_valid_time")]
#[api_doc(
    description = "Get an edge's state at a specific valid time (independent-dimension query)",
    mcp
)]
pub async fn get_edge_at_valid_time(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<EdgeAtValidTimeBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("edge_id".to_string(), Value::from(req.edge_id));
    args.insert("valid_time".to_string(), Value::from(req.valid_time));
    tool_json(server.dispatch_tool_json("get_edge_at_valid_time", Value::Object(args)))
}

/// Request body for [`get_edge_at_transaction_time`] — mirrors the main-crate
/// `GetEdgeAtTransactionTimeRequest`.
#[derive(Debug, Deserialize)]
pub struct EdgeAtTransactionTimeBody {
    /// The unique identifier of the edge.
    pub edge_id: u64,
    /// Transaction time (ISO 8601 / RFC 3339): when the fact was recorded.
    pub transaction_time: String,
}

/// `get_edge_at_transaction_time` — an edge's state at a specific transaction
/// time (independent-dimension query, valid time = now). [`ReadClass`]. HTTP +
/// MCP tool.
#[post("/edges/at_transaction_time")]
#[api_doc(
    description = "Get an edge's state at a specific transaction time (independent-dimension query)",
    mcp
)]
pub async fn get_edge_at_transaction_time(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<EdgeAtTransactionTimeBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("edge_id".to_string(), Value::from(req.edge_id));
    args.insert(
        "transaction_time".to_string(),
        Value::from(req.transaction_time),
    );
    tool_json(server.dispatch_tool_json("get_edge_at_transaction_time", Value::Object(args)))
}

/// Request body for [`diff_edge_versions`] — mirrors the main-crate
/// `DiffEdgeVersionsRequest`.
#[derive(Debug, Deserialize)]
pub struct DiffEdgeVersionsBody {
    /// The unique identifier of the edge.
    pub edge_id: u64,
    /// The id of the older version (from).
    pub from_version: u64,
    /// The id of the newer version (to).
    pub to_version: u64,
}

/// `diff_edge_versions` — the property-level difference between two versions of
/// an edge. [`ReadClass`]. HTTP + MCP tool.
#[post("/edges/diff")]
#[api_doc(
    description = "Compute the property-level difference between two versions of an edge",
    mcp
)]
pub async fn diff_edge_versions(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<DiffEdgeVersionsBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("edge_id".to_string(), Value::from(req.edge_id));
    args.insert("from_version".to_string(), Value::from(req.from_version));
    args.insert("to_version".to_string(), Value::from(req.to_version));
    tool_json(server.dispatch_tool_json("diff_edge_versions", Value::Object(args)))
}
