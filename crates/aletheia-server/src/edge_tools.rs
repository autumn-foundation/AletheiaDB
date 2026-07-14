//! Edge tools: the edge cluster — the reads `get_edge` / `list_edges` /
//! `count_edges` / `get_outgoing_edges` / `get_incoming_edges` and the writes
//! `create_edge` / `update_edge` / `delete_edge` / `retract_edge` (Issue #3524,
//! PR3).
//!
//! Each is a single `#[get(...)]`/`#[post(...)]` + `#[api_doc(description=...,
//! mcp)]` handler, so one definition surfaces on **HTTP**, **MCP-over-HTTP**
//! (`/mcp`), and **OpenAPI** at once. The response body is produced by the
//! *exact* main-crate MCP dispatch, so it is byte-identical to the legacy MCP
//! surface (bare entity JSON + the `temporal` block #3232, #3220 vector elision
//! on the edge read path, the #3416 concurrent-write orphan-abort on
//! `create_edge`/`delete_edge`, the #3230 retraction contract, and #3221
//! `valid_time`) — asserted in the parity suite against
//! `aletheiadb::mcp::AletheiaMcpServer` over the same `Arc<AletheiaDB>`.
//!
//! # Forwarding: typed methods vs. raw-argument dispatch
//!
//! The **writes** (`create_edge`/`update_edge`/`delete_edge`/`retract_edge`) and
//! `count_edges` forward through the main crate's typed per-tool methods
//! (`AletheiaMcpServer::create_edge`, …). The **four budgetable reads**
//! (`get_edge`/`list_edges`/`get_outgoing_edges`/`get_incoming_edges`) instead
//! forward the raw JSON arguments through
//! `AletheiaMcpServer::dispatch_tool_json`, because the Issue #3353 token budget
//! (`max_response_tokens`/`max_response_bytes`/`priority_properties`) and the
//! Issue #3360 cursor paging (`use_cursor`/`cursor`, adjacency tools only) are
//! read off the raw arguments and applied in `dispatch_tool` — the typed
//! per-tool methods bypass that pipeline and their request structs do not carry
//! those params, so typed forwarding would silently drop them. With neither
//! budget nor cursor present, `dispatch_tool_json` reproduces the typed method's
//! output byte-for-byte (same `handle_*`; the shared server is anonymous, so its
//! built-in tool authorization is a no-op — the autumn `Authorized<C>` extractor
//! is the real gate).
//!
//! `dispatch_tool_json` is the one main-crate visibility widening PR3 adds
//! (marked `// exposed for autumn-web migration (Issue #3524)`); every request
//! type it needs was already public. Cursor tokens are validated against a
//! per-instance secret, so these handlers use the process-lifetime shared
//! [`ServerState::mcp_server`](crate::state::ServerState::mcp_server) rather than
//! constructing a server per request.
//!
//! The MCP tool result carries in-band errors (e.g. a missing edge →
//! `{"error":{"code":"NOT_FOUND",...}}`); we return that JSON verbatim with a
//! 200, matching the MCP method's `String` return exactly (unifying HTTP status
//! semantics with the legacy HTTP envelope is a follow-up).
//!
//! The four write tools are classified [`WriteClass`] and the five reads
//! [`ReadClass`]; the marker rides in each handler's `Authorized<C>` signature
//! (the one place the class is declared), which autumn replays for `/mcp
//! tools/call`. The write handlers take their arguments as a JSON **body**
//! (autumn maps `Json<T>` to the reserved `body` MCP argument key), so the
//! `tools/call` `arguments` wrap the request under `"body"`.

use crate::security::{Authorized, ReadClass, WriteClass};
use crate::state::ServerState;
use aletheiadb::auth::AccessClass;
use aletheiadb::mcp::{
    CountEdgesRequest, CreateEdgeRequest, DeleteEdgeRequest, RetractEdgeRequest, UpdateEdgeRequest,
};
use autumn_web::prelude::{Json, Path, Query, get, post};
use serde::Deserialize;
use serde_json::{Map, Value};

/// The dispatch-routed budgetable edge reads, each paired with (a) the
/// **hardcoded literal tool name** it pins in its `dispatch_tool_json(...)` call
/// and (b) the [`AccessClass`] of its `Authorized<C>` gate (all
/// [`ReadClass`](crate::security::ReadClass) → [`AccessClass::Read`]).
///
/// This is the single source of truth binding *pinned name* ↔ *routed name* ↔
/// *registry class*. The `dispatch_pinned_names_match_routed_class` conformance
/// test in `tests/server_parity.rs` asserts, for every entry, that the pinned
/// name is routed on `/mcp` **and** that its RBAC-registry class equals the
/// class declared here — so a read-gated handler can never pin a write tool's
/// name (a privilege-escalation route through a gated handler). The names here
/// are the exact literals passed at the call sites; keep them in lockstep.
///
/// `pub` (not `pub(crate)`) because the conformance test is an integration test
/// — an external crate — exactly like the already-`pub`
/// `security::rbac::MCP_TOOL_CLASSES` it cross-checks against.
///
/// The three **node** reads (`get_node`, `list_nodes`, `find_nodes_at_time`) are
/// dispatch-routed too (see [`crate::node_tools`]) and are pinned here in the
/// same table so the one `dispatch_pinned_names_match_routed_class` conformance
/// test covers every dispatch-routed read across both clusters.
///
/// The two dispatch-routed **traversal/temporal** reads (`traverse`,
/// `get_node_history`) are pinned here too (see
/// [`crate::traverse_temporal_tools`]): both are budgetable (#3353) so they
/// forward raw arguments through `dispatch_tool_json`, `traverse` additionally
/// cursorable (#3360). The other ten tools in that cluster are not budgetable /
/// cursorable and forward through typed methods, so they are not dispatch-routed
/// and do not appear here.
pub const DISPATCH_ROUTED_READ_TOOLS: &[(&str, AccessClass)] = &[
    ("get_edge", AccessClass::Read),
    ("list_edges", AccessClass::Read),
    ("get_outgoing_edges", AccessClass::Read),
    ("get_incoming_edges", AccessClass::Read),
    ("get_node", AccessClass::Read),
    ("list_nodes", AccessClass::Read),
    ("find_nodes_at_time", AccessClass::Read),
    ("traverse", AccessClass::Read),
    ("get_node_history", AccessClass::Read),
];

/// Parse an MCP tool method's JSON string result into a [`Value`] for the HTTP
/// response. A non-JSON result (should not happen) degrades to a JSON string.
fn tool_json(s: String) -> Json<Value> {
    Json(serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)))
}

/// Fold a present optional value into the raw MCP arguments object under `key`.
pub(crate) fn insert_opt(args: &mut Map<String, Value>, key: &str, val: Option<Value>) {
    if let Some(v) = val {
        args.insert(key.to_string(), v);
    }
}

/// Fold the Issue #3353 token-budget parameters (`max_response_tokens` /
/// `max_response_bytes` / `priority_properties`) into the raw arguments object,
/// each only when present. These ride raw arguments (the legacy typed request
/// structs do not carry them; #3353 shaping is applied in
/// `AletheiaMcpServer::dispatch_tool`), so budgetable edge read handlers forward
/// them via `AletheiaMcpServer::dispatch_tool_json` rather than the typed
/// per-tool methods (which would silently drop them).
pub(crate) fn insert_budget(
    args: &mut Map<String, Value>,
    max_response_tokens: Option<u64>,
    max_response_bytes: Option<u64>,
    priority_properties: Option<Vec<String>>,
) {
    insert_opt(
        args,
        "max_response_tokens",
        max_response_tokens.map(Value::from),
    );
    insert_opt(
        args,
        "max_response_bytes",
        max_response_bytes.map(Value::from),
    );
    if let Some(props) = priority_properties {
        args.insert("priority_properties".to_string(), Value::from(props));
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Edge reads: get_edge / list_edges / count_edges / get_outgoing_edges /
// get_incoming_edges — all [`ReadClass`], #3220 vector elision default +
// `include_vectors`, #3232 temporal block.
//
// The four **budgetable** reads (`get_edge`, `list_edges`, `get_outgoing_edges`,
// `get_incoming_edges`) forward through [`AletheiaMcpServer::dispatch_tool_json`]
// rather than the typed per-tool methods, because the Issue #3353 token budget
// (`max_response_tokens` / `max_response_bytes` / `priority_properties`) and the
// Issue #3360 cursor paging (`use_cursor` / `cursor`, adjacency tools only) are
// read off the **raw arguments** and applied in `dispatch_tool` — the typed
// methods bypass that pipeline and their request structs do not carry those
// params, so typed forwarding would silently drop them. With neither budget nor
// cursor present, `dispatch_tool_json` reproduces the typed method's output
// byte-for-byte (same `handle_*`, anonymous auth is a no-op). `count_edges` is
// not budgetable/cursorable, so it keeps the typed method.
// ════════════════════════════════════════════════════════════════════════════

/// Query options for [`get_edge`] — the entity flag plus the #3353 token-budget
/// parameters.
#[derive(Debug, Default, Deserialize)]
pub struct GetEdgeQuery {
    /// Return full vector/embedding arrays instead of the elided descriptor.
    pub include_vectors: Option<bool>,
    /// #3353: cap the response at roughly this many tokens (utf8_bytes / 4).
    pub max_response_tokens: Option<u64>,
    /// #3353: byte-exact response cap.
    pub max_response_bytes: Option<u64>,
    /// #3353: property keys to protect first as the response degrades.
    pub priority_properties: Option<Vec<String>>,
}

/// `get_edge` — fetch an edge by id, with bi-temporal bounds. HTTP + MCP tool.
#[get("/edges/{id}")]
#[api_doc(
    description = "Fetch an edge by id, returning its source, target, label, properties, and bi-temporal bounds",
    mcp
)]
pub async fn get_edge(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Path(id): Path<u64>,
    Query(opts): Query<GetEdgeQuery>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("edge_id".to_string(), Value::from(id));
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
    tool_json(server.dispatch_tool_json("get_edge", Value::Object(args)))
}

/// Query options for [`list_edges`] — label/paging plus the #3353 token budget.
#[derive(Debug, Default, Deserialize)]
pub struct ListEdgesQuery {
    /// Filter by edge label.
    pub label: Option<String>,
    /// Maximum number of edges to return.
    pub limit: Option<usize>,
    /// Number of edges to skip.
    pub offset: Option<usize>,
    /// Return full vector/embedding arrays instead of the elided descriptor.
    pub include_vectors: Option<bool>,
    /// #3353: cap the response at roughly this many tokens (utf8_bytes / 4).
    pub max_response_tokens: Option<u64>,
    /// #3353: byte-exact response cap.
    pub max_response_bytes: Option<u64>,
    /// #3353: property keys to protect first as the response degrades.
    pub priority_properties: Option<Vec<String>>,
}

/// `list_edges` — list edges with optional label filtering + paging. HTTP + MCP
/// tool. Budgetable (#3353) but not cursorable (per the design doc, `list_edges`
/// does not enumerate edges — use the adjacency tools).
#[get("/edges")]
#[api_doc(
    description = "List edges with optional label filtering and pagination",
    mcp
)]
pub async fn list_edges(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Query(opts): Query<ListEdgesQuery>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    insert_opt(&mut args, "label", opts.label.map(Value::from));
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
    tool_json(server.dispatch_tool_json("list_edges", Value::Object(args)))
}

/// Query options for [`count_edges`].
#[derive(Debug, Default, Deserialize)]
pub struct CountEdgesQuery {
    /// Count only edges with this label (else all edges).
    pub label: Option<String>,
}

/// `count_edges` — total edge count, or count matching a label. HTTP + MCP tool.
/// Not budgetable/cursorable, so it forwards through the typed method.
#[get("/edges/count")]
#[api_doc(
    description = "Count edges in the graph, or edges matching a specific label",
    mcp
)]
pub async fn count_edges(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Query(opts): Query<CountEdgesQuery>,
) -> Json<Value> {
    let server = state.mcp_server();
    let out = server.count_edges(CountEdgesRequest { label: opts.label });
    tool_json(out)
}

/// Query options for [`get_outgoing_edges`] / [`get_incoming_edges`] — the
/// filter/flag plus the #3353 token budget and the #3360 cursor parameters.
#[derive(Debug, Default, Deserialize)]
pub struct AdjacencyQuery {
    /// Filter by edge label.
    pub label: Option<String>,
    /// Return full vector/embedding arrays instead of the elided descriptor.
    pub include_vectors: Option<bool>,
    /// #3360: per-page size when cursor paging (ignored by the full-adjacency
    /// non-cursor path, which returns every edge).
    pub limit: Option<usize>,
    /// #3353: cap the response at roughly this many tokens (utf8_bytes / 4).
    pub max_response_tokens: Option<u64>,
    /// #3353: byte-exact response cap.
    pub max_response_bytes: Option<u64>,
    /// #3353: property keys to protect first as the response degrades.
    pub priority_properties: Option<Vec<String>>,
    /// #3360: request a snapshot-anchored cursor on the first page.
    pub use_cursor: Option<bool>,
    /// #3360: opaque continuation token from a prior page. The token carries the
    /// scan's `node_id`; the path `node_id` is still required by the route but
    /// is superseded by the token on continuation.
    pub cursor: Option<String>,
}

/// Build the raw adjacency arguments (node_id + filter/flag + #3353 budget +
/// #3360 cursor), shared by the outgoing and incoming handlers.
fn adjacency_args(node_id: u64, opts: AdjacencyQuery) -> Value {
    let mut args = Map::new();
    args.insert("node_id".to_string(), Value::from(node_id));
    insert_opt(&mut args, "label", opts.label.map(Value::from));
    insert_opt(
        &mut args,
        "include_vectors",
        opts.include_vectors.map(Value::from),
    );
    insert_opt(&mut args, "limit", opts.limit.map(Value::from));
    insert_budget(
        &mut args,
        opts.max_response_tokens,
        opts.max_response_bytes,
        opts.priority_properties,
    );
    insert_opt(&mut args, "use_cursor", opts.use_cursor.map(Value::from));
    insert_opt(&mut args, "cursor", opts.cursor.map(Value::from));
    Value::Object(args)
}

/// `get_outgoing_edges` — the edges starting at a node, optionally filtered by
/// label. [`ReadClass`]; vectors elided by default (#3220); budgetable (#3353)
/// and cursorable (#3360). HTTP + MCP tool.
#[get("/nodes/{node_id}/edges/outgoing")]
#[api_doc(
    description = "Get outgoing edges from a node, optionally filtered by edge label",
    mcp
)]
pub async fn get_outgoing_edges(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Path(node_id): Path<u64>,
    Query(opts): Query<AdjacencyQuery>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.dispatch_tool_json("get_outgoing_edges", adjacency_args(node_id, opts)))
}

/// `get_incoming_edges` — the edges ending at a node, optionally filtered by
/// label. [`ReadClass`]; vectors elided by default (#3220); budgetable (#3353)
/// and cursorable (#3360). HTTP + MCP tool.
#[get("/nodes/{node_id}/edges/incoming")]
#[api_doc(
    description = "Get incoming edges to a node, optionally filtered by edge label",
    mcp
)]
pub async fn get_incoming_edges(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Path(node_id): Path<u64>,
    Query(opts): Query<AdjacencyQuery>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.dispatch_tool_json("get_incoming_edges", adjacency_args(node_id, opts)))
}

// ════════════════════════════════════════════════════════════════════════════
// Edge-write cluster (Issue #3524, PR3): create / update / delete / retract —
// all [`WriteClass`].
//
// Each handler forwards its request body to the exact legacy MCP typed method
// and returns that method's `String` verbatim, so every contract those methods
// implement — #3221 `valid_time`, #3416 concurrent-write orphan-abort on
// create_edge/delete_edge, #3230 retraction idempotency, #3232 temporal block —
// is preserved byte-for-byte with **zero** reserialization.
// ════════════════════════════════════════════════════════════════════════════

/// `create_edge` — create an edge between two nodes with properties and optional
/// bi-temporal `valid_time` (#3221), provenance, and derivation lineage (#3371).
/// The response carries the new edge with its `temporal` block (#3232). HTTP +
/// MCP tool.
#[post("/edges")]
#[api_doc(
    description = "Create an edge between two nodes with properties and optional valid_time, provenance, and lineage",
    mcp
)]
pub async fn create_edge(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<CreateEdgeRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.create_edge(req))
}

/// `update_edge` — replace an edge's properties, recording a new bi-temporal
/// version (optional `valid_time` #3221, provenance, lineage). HTTP + MCP tool.
#[post("/edges/update")]
#[api_doc(
    description = "Update an edge's properties, recording a new version with optional valid_time",
    mcp
)]
pub async fn update_edge(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<UpdateEdgeRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.update_edge(req))
}

/// `delete_edge` — remove an edge, with optional `valid_time` (#3221). HTTP +
/// MCP tool.
#[post("/edges/delete")]
#[api_doc(
    description = "Delete an edge, removing the relationship between two nodes",
    mcp
)]
pub async fn delete_edge(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<DeleteEdgeRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.delete_edge(req))
}

/// `retract_edge` — close an edge's valid-time interval without deleting history
/// (#3230); re-retraction is an idempotent no-op. HTTP + MCP tool.
#[post("/edges/retract")]
#[api_doc(
    description = "Retract an edge (close its valid-time interval) without deleting history",
    mcp
)]
pub async fn retract_edge(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<RetractEdgeRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.retract_edge(req))
}
