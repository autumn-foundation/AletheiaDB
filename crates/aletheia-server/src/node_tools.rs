//! Node tools: the read proof slice (`get_node` / `list_nodes` / `count_nodes`)
//! plus the node-write cluster (`create_node` / `update_node` / `delete_node` /
//! `delete_node_cascade` / `retract_node`) and the point-in-time node finder
//! `find_nodes_at_time` (Issue #3524, PR2).
//!
//! Each is a single `#[get(...)]`/`#[post(...)]` + `#[api_doc(description=...,
//! mcp)]` handler, so one definition surfaces on **HTTP**, **MCP-over-HTTP**
//! (`/mcp`), and **OpenAPI** at once. The response body is produced by the
//! *exact* main-crate MCP surface, so it is byte-identical to the legacy MCP
//! surface (bare entity JSON + the `temporal` block #3232, #3220 vector elision,
//! the #3209 DETACH refuse/detach contract on `delete_node`, the #3230
//! retraction contract, #3221 `valid_time`, and #3236 point-in-time
//! reconstruction) — asserted in the parity suite against a fresh
//! [`AletheiaMcpServer`] over the same `Arc<AletheiaDB>`.
//!
//! # Forwarding: typed methods vs. raw-argument dispatch
//!
//! The **writes** (`create_node`/`update_node`/`delete_node`/
//! `delete_node_cascade`/`retract_node`) and `count_nodes` forward through the
//! main crate's typed per-tool methods (`AletheiaMcpServer::create_node`, …).
//! The three **budgetable reads** (`get_node`, `list_nodes`,
//! `find_nodes_at_time`) instead forward the raw JSON arguments through
//! `AletheiaMcpServer::dispatch_tool_json`, because the Issue #3353 token budget
//! (`max_response_tokens`/`max_response_bytes`/`priority_properties`, all three)
//! and the Issue #3360 cursor paging (`use_cursor`/`cursor`, `list_nodes` /
//! `find_nodes_at_time` only — `get_node` is a single-entity read and is not
//! cursorable) are read off the raw arguments and applied in `dispatch_tool` —
//! the typed per-tool methods bypass that pipeline and their request structs do
//! not carry those params, so typed forwarding would silently drop them (the gap
//! this retrofit closes). With neither budget nor cursor present,
//! `dispatch_tool_json` reproduces the typed method's output byte-for-byte (same
//! `handle_*`; the shared server is anonymous, so its built-in tool
//! authorization is a no-op — the autumn `Authorized<C>` extractor is the real
//! gate). Cursor tokens are validated against a per-instance secret, so these
//! handlers use the process-lifetime shared
//! [`ServerState::mcp_server`](crate::state::ServerState::mcp_server) rather than
//! constructing a server per request. `find_nodes_at_time` takes an
//! **all-optional** typed body ([`FindNodesAtTimeQuery`]) — every field
//! `Option<_>` — so a #3360 cursor-continuation POST (`{"cursor": "…"}`, with no
//! `label`/`valid_time`) still deserializes (those two are logically required but
//! their requiredness stays a runtime concern the dispatch layer enforces),
//! while `/openapi.json` still gets a named, per-operation request schema instead
//! of the shared generic `Value` component a raw `Json<Value>` body would emit.
//!
//! The three dispatch-routed reads are pinned with **hardcoded literal** tool
//! names (`"get_node"`/`"list_nodes"`/`"find_nodes_at_time"`, never
//! request-derived) and registered in
//! [`crate::edge_tools::DISPATCH_ROUTED_READ_TOOLS`] so the shared
//! `dispatch_pinned_names_match_routed_class` conformance test proves a
//! read-gated handler can never pin a write tool's name.
//!
//! The writes and `count_nodes` reuse **already-public** symbols
//! (`aletheiadb::mcp::{AletheiaMcpServer, CountNodesRequest, CreateNodeRequest,
//! UpdateNodeRequest, DeleteNodeRequest, DeleteNodeCascadeRequest,
//! RetractNodeRequest}`); `dispatch_tool_json` (the reads' entry) is the
//! main-crate widening PR3 already landed. No further widening is required.
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

use crate::edge_tools::{insert_budget, insert_opt};
use crate::security::resource_limits::{check_byte_cap_of, run_with_timeout};
use crate::security::{Authorized, ReadClass, ServerSecurityState, WriteClass};
use crate::state::ServerState;
use aletheiadb::http::AletheiaHttpError;
use aletheiadb::mcp::{
    AletheiaMcpServer, CountNodesRequest, CreateNodeRequest, CreateNodeWithEmbeddingRequest,
    DeleteNodeCascadeRequest, DeleteNodeRequest, RetractNodeRequest, UpdateNodeEmbeddingRequest,
    UpdateNodeRequest,
};
use autumn_web::prelude::{Json, Path, Query, get, post};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Parse an MCP tool method's JSON string result into a [`Value`] for the HTTP
/// response. A non-JSON result (should not happen) degrades to a JSON string.
fn tool_json(s: String) -> Json<Value> {
    Json(serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)))
}

/// Query options for [`get_node`] — the entity flag plus the #3353 token-budget
/// parameters. `get_node` is a single-entity read: budgetable (#3353) but not
/// cursorable (#3360).
#[derive(Debug, Default, Deserialize)]
pub struct GetNodeQuery {
    /// Return full vector/embedding arrays instead of the elided descriptor.
    pub include_vectors: Option<bool>,
    /// #3353: cap the response at roughly this many tokens (utf8_bytes / 4).
    pub max_response_tokens: Option<u64>,
    /// #3353: byte-exact response cap.
    pub max_response_bytes: Option<u64>,
    /// #3353: property keys to protect first as the response degrades.
    pub priority_properties: Option<Vec<String>>,
}

/// `get_node` — fetch a node by id, with bi-temporal bounds. HTTP + MCP tool.
/// Budgetable (#3353); forwards raw arguments through `dispatch_tool_json` so the
/// token budget applies (the typed method bypasses that pipeline).
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
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("node_id".to_string(), Value::from(id));
    insert_opt(
        &mut args,
        "include_vectors",
        opts.include_vectors.map(Value::from),
    );
    insert_budget(
        &mut args,
        opts.max_response_tokens,
        opts.max_response_bytes,
        opts.priority_properties,
    );
    tool_json(server.dispatch_tool_json("get_node", Value::Object(args)))
}

/// Query options for [`list_nodes`] — label/property/paging plus the #3353 token
/// budget and the #3360 cursor parameters. `list_nodes` is budgetable **and**
/// cursorable.
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
    /// #3353: cap the response at roughly this many tokens (utf8_bytes / 4).
    pub max_response_tokens: Option<u64>,
    /// #3353: byte-exact response cap.
    pub max_response_bytes: Option<u64>,
    /// #3353: property keys to protect first as the response degrades.
    pub priority_properties: Option<Vec<String>>,
    /// #3360: request a snapshot-anchored cursor on the first page.
    pub use_cursor: Option<bool>,
    /// #3360: opaque continuation token from a prior page (passed back alone).
    pub cursor: Option<String>,
}

/// `list_nodes` — list nodes with optional label/property filtering + paging.
/// HTTP + MCP tool. Budgetable (#3353) and cursorable (#3360): forwards raw
/// arguments through `dispatch_tool_json` so both pipelines apply (the typed
/// method bypasses them).
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

    // Route through the shared server's `dispatch_tool_json` so the #3353 token
    // budget and #3360 cursor paging (read off the raw arguments) apply — the
    // typed `list_nodes` method bypasses that pipeline. The cursor secret lives
    // on the process-lifetime shared server, so continuations resume correctly.
    let server = state.mcp_server();
    let mut args = Map::new();
    insert_opt(&mut args, "label", opts.label.map(Value::from));
    insert_opt(
        &mut args,
        "property_key",
        opts.property_key.map(Value::from),
    );
    // property_value arrives as a string on the HTTP query surface (matching the
    // prior typed forwarding, which wrapped it in `Value::String`).
    insert_opt(
        &mut args,
        "property_value",
        opts.property_value.map(Value::from),
    );
    insert_opt(&mut args, "limit", opts.limit.map(Value::from));
    insert_opt(&mut args, "offset", opts.offset.map(Value::from));
    insert_opt(
        &mut args,
        "include_vectors",
        opts.include_vectors.map(Value::from),
    );
    insert_budget(
        &mut args,
        opts.max_response_tokens,
        opts.max_response_bytes,
        opts.priority_properties,
    );
    insert_opt(&mut args, "use_cursor", opts.use_cursor.map(Value::from));
    insert_opt(&mut args, "cursor", opts.cursor.map(Value::from));
    let args = Value::Object(args);

    let out = run_with_timeout(limits.timeout, false, async move {
        server.dispatch_tool_json("list_nodes", args)
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
// delete_cascade / retract — all [`WriteClass`].
//
// Each handler forwards its request body to the exact legacy MCP typed method
// and returns that method's `String` verbatim, so every contract those methods
// implement — #3221 `valid_time`, #3209 DETACH refuse/detach, #3230 retraction
// idempotency, #3232 temporal block, #3220 vector elision — is preserved
// byte-for-byte with **zero** reserialization. The [`ReadClass`] point-in-time
// finder `find_nodes_at_time` follows the reads (below), routing through
// `dispatch_tool_json` so the #3353 budget and #3360 cursor pipelines apply.
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

/// `create_node_with_embedding` — create a node whose embedding is generated
/// from `text` using the server's configured model and stored under
/// `embedding_property` (Issue #2906). [`WriteClass`]. HTTP + MCP tool. Forwards
/// through the SHARED server (so a configured embedder is used); returns a
/// structured FAILED_PRECONDITION / unavailable-feature payload when no model is
/// configured or the `embeddings` feature is not compiled.
#[post("/nodes/with_embedding")]
#[api_doc(
    description = "Create a node whose embedding is generated from text and stored as a vector property",
    mcp
)]
pub async fn create_node_with_embedding(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<CreateNodeWithEmbeddingRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.create_node_with_embedding(req))
}

/// `update_node_embedding` — regenerate a node's embedding from `text` and
/// update ONLY the embedding property, preserving all other properties (Issue
/// #2906). [`WriteClass`]. HTTP + MCP tool. Forwards through the SHARED server.
#[post("/nodes/update_embedding")]
#[api_doc(
    description = "Regenerate a node's embedding from text, updating only the embedding property",
    mcp
)]
pub async fn update_node_embedding(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<UpdateNodeEmbeddingRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.update_node_embedding(req))
}

/// Request body for [`find_nodes_at_time`] — an **all-optional** mirror of the
/// legacy `FindNodesAtTimeRequest` plus the #3353 token budget and the #3360
/// cursor parameters.
///
/// Every field is `Option<_>` on purpose. `label` and `valid_time` are logically
/// required for a fresh query, but requiredness stays a **runtime** concern:
/// `dispatch_tool` already returns a structured `INVALID_ARGUMENT` when they are
/// missing. Keeping them optional here is what lets a #3360 cursor-continuation
/// body (`{"cursor": "…"}`, which omits `label` / `valid_time`) still
/// deserialize. The handler rebuilds the raw MCP arguments object from the
/// `Some` fields and forwards it through `dispatch_tool_json`, so budget/cursor
/// still apply and a no-budget/no-cursor call is byte-identical to before.
///
/// This typed body (vs. a bare `Json<Value>`) is what restores a named,
/// per-operation request schema in `/openapi.json` — mirroring the typed sibling
/// handlers (`Json<CreateNodeRequest>` etc.) — instead of the shared, generic
/// `Value` component the raw body produced.
#[derive(Debug, Default, Deserialize)]
pub struct FindNodesAtTimeQuery {
    /// The node label to match (logically required; enforced at dispatch).
    pub label: Option<String>,
    /// Filter by property key (with `property_value`).
    pub property_key: Option<String>,
    /// Filter by exact property value (with `property_key`).
    pub property_value: Option<Value>,
    /// Valid time (logically required; enforced at dispatch): ISO 8601 / RFC 3339
    /// or microseconds since epoch.
    pub valid_time: Option<String>,
    /// Transaction time (defaults to now when omitted).
    pub transaction_time: Option<String>,
    /// Maximum number of nodes to return.
    pub limit: Option<usize>,
    /// Number of matching nodes to skip.
    pub offset: Option<usize>,
    /// Return full vector/embedding arrays instead of the elided descriptor.
    pub include_vectors: Option<bool>,
    /// #3353: cap the response at roughly this many tokens (utf8_bytes / 4).
    pub max_response_tokens: Option<u64>,
    /// #3353: byte-exact response cap.
    pub max_response_bytes: Option<u64>,
    /// #3353: property keys to protect first as the response degrades.
    pub priority_properties: Option<Vec<String>>,
    /// #3360: request a snapshot-anchored cursor on the first page.
    pub use_cursor: Option<bool>,
    /// #3360: opaque continuation token from a prior page (passed back alone).
    pub cursor: Option<String>,
}

/// `find_nodes_at_time` — resolve nodes by label (+ optional exact property
/// match) as they existed at a bi-temporal point (#3236), reconstructing each
/// node's state at `(valid_time, transaction_time)`. Read-only ([`ReadClass`]);
/// vectors elided by default (#3220). Budgetable (#3353) and cursorable (#3360).
/// HTTP + MCP tool.
///
/// Takes the typed all-optional [`FindNodesAtTimeQuery`] body and rebuilds the
/// raw MCP arguments (only the `Some` fields) before forwarding through
/// `dispatch_tool_json`, so (a) the #3353 budget
/// (`max_response_tokens`/`max_response_bytes`/`priority_properties`) and #3360
/// cursor (`use_cursor`/`cursor`) params flow through, and (b) a cursor
/// continuation (`{"cursor": "…"}`, which omits the otherwise-required `label` /
/// `valid_time`) deserializes — required-ness stays a runtime concern the
/// dispatch layer enforces with a structured `INVALID_ARGUMENT`. With no
/// budget/cursor present the rebuilt args equal a `FindNodesAtTimeRequest`, so
/// the result is byte-identical to the legacy method. A non-object body is not
/// representable through the typed extractor; were one to reach dispatch it would
/// degrade to the same structured `INVALID_ARGUMENT` as the legacy `Value::Null`
/// contract (no guard needed).
#[post("/nodes/find_at_time")]
#[api_doc(
    description = "Find nodes by label/property as of a bi-temporal point, reconstructed at that time",
    mcp
)]
pub async fn find_nodes_at_time(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(opts): Json<FindNodesAtTimeQuery>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    insert_opt(&mut args, "label", opts.label.map(Value::from));
    insert_opt(
        &mut args,
        "property_key",
        opts.property_key.map(Value::from),
    );
    // property_value is already a JSON value (string/number/bool/null).
    insert_opt(&mut args, "property_value", opts.property_value);
    insert_opt(&mut args, "valid_time", opts.valid_time.map(Value::from));
    insert_opt(
        &mut args,
        "transaction_time",
        opts.transaction_time.map(Value::from),
    );
    insert_opt(&mut args, "limit", opts.limit.map(Value::from));
    insert_opt(&mut args, "offset", opts.offset.map(Value::from));
    insert_opt(
        &mut args,
        "include_vectors",
        opts.include_vectors.map(Value::from),
    );
    insert_budget(
        &mut args,
        opts.max_response_tokens,
        opts.max_response_bytes,
        opts.priority_properties,
    );
    insert_opt(&mut args, "use_cursor", opts.use_cursor.map(Value::from));
    insert_opt(&mut args, "cursor", opts.cursor.map(Value::from));
    tool_json(server.dispatch_tool_json("find_nodes_at_time", Value::Object(args)))
}
