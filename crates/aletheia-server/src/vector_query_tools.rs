//! Vector + hybrid + declarative-query tools: `find_similar`,
//! `list_vector_indexes`, `hybrid_query`, and `query` (Issue #3524, PR5).
//!
//! Each is a single `#[get(...)]`/`#[post(...)]` + `#[api_doc(description=...,
//! mcp)]` handler, so one definition surfaces on **HTTP**, **MCP-over-HTTP**
//! (`/mcp`), and **OpenAPI** at once. The response body is produced by the
//! *exact* main-crate MCP surface, so it is byte-identical to the legacy MCP
//! surface (bare tool-result JSON, #3220 vector elision on `find_similar` /
//! `hybrid_query`, the #3353 token budget on the three budgetable reads, and the
//! `query` tool's full structured-error contract — `{error:{kind,message,
//! clause?,language}}` with the uniform #3234 `code`/`retriable` carried
//! additively, read-only enforcement, `language_unavailable` when the `cypher`
//! feature is not compiled, and the #3360 cursor → `unsupported_construct`
//! rejection) — asserted in the parity suite against the shared
//! [`AletheiaMcpServer`](aletheiadb::mcp::AletheiaMcpServer) over the same
//! `Arc<AletheiaDB>`.
//!
//! # Forwarding: dispatch-routed slow reads vs. a typed inline read
//!
//! The **single source of truth** for which reads honor the Issue #3353 token
//! budget is `aletheiadb::mcp`'s `BUDGETABLE_READ_TOOLS`, and for the Issue
//! #3360 cursor the `inject_cursor_schema_params` set. Verified against source,
//! of this cluster's four tools **`find_similar`, `hybrid_query`, and `query`
//! are budgetable** and **none of the four is cursorable**. That split drives
//! two forwarding shapes:
//!
//! 1. **`find_similar` / `hybrid_query` / `query`** — budgetable (#3353) +
//!    potentially slow (k-NN search / hybrid graph+vector+temporal / declarative
//!    query execution). Each forwards the raw JSON arguments through
//!    [`AletheiaMcpServer::dispatch_tool_json`](aletheiadb::mcp::AletheiaMcpServer::dispatch_tool_json)
//!    (so the #3353 budget pipeline, read off the raw arguments in
//!    `dispatch_tool`, applies) via the process-lifetime shared
//!    [`ServerState::mcp_server`](crate::state::ServerState::mcp_server). Each
//!    runs under the **exact** B4 resource-limits wiring `traverse` / `list_nodes`
//!    use — a bounded in-flight admission guard released by RAII on any exit, a
//!    per-query wall-clock [`run_with_timeout`], and a serialized-byte cap
//!    ([`check_byte_cap_of`]) — because all three are potentially-slow reads
//!    (mirroring the legacy HTTP `/query` execution model). Each is registered in
//!    [`crate::edge_tools::DISPATCH_ROUTED_READ_TOOLS`] with its hardcoded literal
//!    tool name so the shared `dispatch_pinned_names_match_routed_class`
//!    conformance test proves a read-gated handler can never pin a write tool's
//!    name. `query`'s structured-error contract, read-only enforcement,
//!    `language_unavailable`, and cursor → `unsupported_construct` behavior are
//!    **all** produced by dispatch — the handler reimplements none of it; it
//!    forwards the raw arguments (including `use_cursor` / `cursor`, so the
//!    dispatch layer can reject them) and returns the result verbatim.
//! 2. **`list_vector_indexes`** — NOT budgetable, NOT cursorable, no arguments.
//!    Forwards through the **typed** per-tool method
//!    (`AletheiaMcpServer::list_vector_indexes`), inline (like `count_nodes` /
//!    the edge writes) — zero reserialization, reusing only already-public
//!    symbols.
//!
//! All four are classified [`ReadClass`]; the marker rides in each handler's
//! `Authorized<C>` signature (the one place the class is declared), which autumn
//! replays for `/mcp tools/call`.
//!
//! The three slow reads take a **typed all-optional** request body
//! (`#[derive(Default, Deserialize)]`, every field `Option<_>`) so that (a)
//! `/openapi.json` gets a named, per-operation request schema instead of the
//! shared generic `Value` a raw `Json<Value>` body would emit (the #3589
//! lesson), and (b) requiredness (e.g. `query`'s `query` string,
//! `find_similar`'s `property_name` / `embedding`) stays a **runtime** concern
//! the dispatch layer enforces with a structured `INVALID_ARGUMENT`. The handler
//! rebuilds the raw MCP arguments object from the `Some` fields and forwards it,
//! so a no-budget call is byte-identical to the legacy tool. Embedding inputs
//! (`find_similar.embedding`, `hybrid_query.query_embedding`) and `query.params`
//! (numeric arrays treated as embeddings) round-trip through the rebuilt args
//! map unchanged.
//!
//! The MCP tool result carries in-band errors (e.g. a missing property index →
//! `{"error":{"code":...}}`); we return that JSON verbatim with a 200, matching
//! the MCP method's `String` return exactly.

use crate::edge_tools::{insert_budget, insert_opt};
use crate::security::resource_limits::{check_byte_cap_of, run_with_timeout};
use crate::security::{Authorized, ReadClass, ServerSecurityState};
use crate::state::ServerState;
use aletheiadb::http::AletheiaHttpError;
use aletheiadb::mcp::ListVectorIndexesRequest;
use autumn_web::prelude::{Json, get, post};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Parse an MCP tool method's JSON string result into a [`Value`] for the HTTP
/// response. A non-JSON result (should not happen) degrades to a JSON string.
fn tool_json(s: String) -> Json<Value> {
    Json(serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)))
}

/// Run a dispatch-routed slow read under the B4 resource-limits wiring (identical
/// to `traverse` / `list_nodes`): a bounded in-flight admission guard (503
/// UNAVAILABLE at capacity, released by RAII on any exit), a per-query wall-clock
/// timeout (429 on deadline), and a serialized-byte cap on the exact wire
/// response (413). Generous defaults make this a no-op on the parity path.
///
/// Forwards the rebuilt raw `args` through the shared server's
/// `dispatch_tool_json` (so the #3353 token budget, read off the raw arguments,
/// applies) with the hardcoded literal `tool` name.
async fn dispatch_slow_read(
    security: &ServerSecurityState,
    state: &ServerState,
    tool: &'static str,
    args: Value,
) -> Result<Json<Value>, AletheiaHttpError> {
    let limits = security.limits();
    let _guard = security.in_flight().try_acquire()?;

    let server = state.mcp_server();
    let out = run_with_timeout(limits.timeout, false, async move {
        server.dispatch_tool_json(tool, args)
    })
    .await?;

    let value = serde_json::from_str::<Value>(&out).unwrap_or(Value::String(out));
    // Cap against the exact serialized response bytes (undercount-proof).
    check_byte_cap_of(&value, limits.max_response_bytes)?;
    Ok(Json(value))
}

// ════════════════════════════════════════════════════════════════════════════
// find_similar — [`ReadClass`], budgetable (#3353), NOT cursorable, potentially
// slow (k-NN). Forwards raw arguments through `dispatch_tool_json` (budget
// applies) under the B4 resource-limits wiring. #3220 vector elision applies to
// the response; the budget never drops/reorders ranked results (dispatch handles
// that — the handler does not post-process).
// ════════════════════════════════════════════════════════════════════════════

/// Request body for [`find_similar`] — an **all-optional** mirror of the legacy
/// `FindSimilarRequest` plus the #3353 token budget. `property_name` and
/// `embedding` are logically required for a fresh query, but requiredness stays a
/// **runtime** concern (`dispatch_tool` returns a structured `INVALID_ARGUMENT`
/// when they are missing); keeping every field optional restores a named,
/// per-operation `/openapi.json` request schema (vs. a generic `Value`).
#[derive(Debug, Default, Deserialize)]
pub struct FindSimilarBody {
    /// The property name that contains the vector embedding (logically required;
    /// enforced at dispatch).
    pub property_name: Option<String>,
    /// The query embedding vector (logically required; enforced at dispatch).
    pub embedding: Option<Vec<f32>>,
    /// Number of similar results to return (default 10).
    pub k: Option<usize>,
    /// Number of results to skip (offset pagination, #3226).
    pub offset: Option<usize>,
    /// Return full vector/embedding arrays instead of the elided descriptor
    /// (#3220). Never affects the similarity `score`.
    pub include_vectors: Option<bool>,
    /// #3353: cap the response at roughly this many tokens (utf8_bytes / 4).
    pub max_response_tokens: Option<u64>,
    /// #3353: byte-exact response cap.
    pub max_response_bytes: Option<u64>,
    /// #3353: property keys to protect first as the response degrades.
    pub priority_properties: Option<Vec<String>>,
}

/// `find_similar` — k-nearest-neighbor search for nodes whose `property_name`
/// embedding is closest to a query `embedding`. [`ReadClass`]; vectors elided by
/// default (#3220); budgetable (#3353), not cursorable. HTTP + MCP tool.
///
/// Forwards raw arguments through `dispatch_tool_json` (so the #3353 budget
/// applies) under the B4 resource-limits wiring (in-flight guard, wall-clock
/// timeout, byte cap), because k-NN is a potentially-slow read. The budget never
/// drops or reorders ranked results (only per-result payloads degrade) — the
/// handler forwards raw and does not post-process.
#[post("/find_similar")]
#[api_doc(
    description = "Find the k nodes whose embedding property is most similar to a query embedding vector",
    mcp
)]
pub async fn find_similar(
    _auth: Authorized<ReadClass>,
    security: ServerSecurityState,
    state: ServerState,
    Json(opts): Json<FindSimilarBody>,
) -> Result<Json<Value>, AletheiaHttpError> {
    let mut args = Map::new();
    insert_opt(
        &mut args,
        "property_name",
        opts.property_name.map(Value::from),
    );
    insert_opt(&mut args, "embedding", opts.embedding.map(Value::from));
    insert_opt(&mut args, "k", opts.k.map(Value::from));
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
    dispatch_slow_read(&security, &state, "find_similar", Value::Object(args)).await
}

// ════════════════════════════════════════════════════════════════════════════
// hybrid_query — [`ReadClass`], budgetable (#3353), NOT cursorable, slow
// (graph + vector + temporal). Same wiring as find_similar.
// ════════════════════════════════════════════════════════════════════════════

/// Request body for [`hybrid_query`] — an **all-optional** mirror of the legacy
/// `HybridQueryRequest` plus the #3353 token budget. Every field is already
/// `Option<_>` in the legacy request; the body restores a named,
/// per-operation `/openapi.json` schema.
#[derive(Debug, Default, Deserialize)]
pub struct HybridQueryBody {
    /// Starting node id (optional, for graph-first queries).
    pub start_node_id: Option<u64>,
    /// Edge label for traversal (optional).
    pub traverse_edge: Option<String>,
    /// Traversal depth (optional, default 1).
    pub traverse_depth: Option<usize>,
    /// Property name for vector similarity (optional).
    pub vector_property: Option<String>,
    /// Query embedding for vector similarity (optional).
    pub query_embedding: Option<Vec<f32>>,
    /// Number of similar results for vector search (optional).
    pub top_k: Option<usize>,
    /// Valid time for temporal filtering (optional, ISO 8601).
    pub valid_time: Option<String>,
    /// Transaction time for temporal filtering (optional, ISO 8601).
    pub transaction_time: Option<String>,
    /// Filter by node label (optional).
    pub filter_label: Option<String>,
    /// Maximum number of results (optional, default 100).
    pub limit: Option<usize>,
    /// Return full vector/embedding arrays instead of the elided descriptor
    /// (#3220). Never affects the `similarity_score`.
    pub include_vectors: Option<bool>,
    /// #3353: cap the response at roughly this many tokens (utf8_bytes / 4).
    pub max_response_tokens: Option<u64>,
    /// #3353: byte-exact response cap.
    pub max_response_bytes: Option<u64>,
    /// #3353: property keys to protect first as the response degrades.
    pub priority_properties: Option<Vec<String>>,
}

/// `hybrid_query` — combined graph traversal + vector similarity + temporal
/// filtering in one query. [`ReadClass`]; vectors elided by default (#3220);
/// budgetable (#3353), not cursorable. HTTP + MCP tool.
///
/// Same wiring as [`find_similar`]: forwards raw arguments through
/// `dispatch_tool_json` (budget applies) under the B4 resource-limits guard. The
/// budget never drops or reorders ranked results (only per-result payloads
/// degrade).
#[post("/hybrid_query")]
#[api_doc(
    description = "Combined graph traversal, vector similarity, and temporal filtering in a single query",
    mcp
)]
pub async fn hybrid_query(
    _auth: Authorized<ReadClass>,
    security: ServerSecurityState,
    state: ServerState,
    Json(opts): Json<HybridQueryBody>,
) -> Result<Json<Value>, AletheiaHttpError> {
    let mut args = Map::new();
    insert_opt(
        &mut args,
        "start_node_id",
        opts.start_node_id.map(Value::from),
    );
    insert_opt(
        &mut args,
        "traverse_edge",
        opts.traverse_edge.map(Value::from),
    );
    insert_opt(
        &mut args,
        "traverse_depth",
        opts.traverse_depth.map(Value::from),
    );
    insert_opt(
        &mut args,
        "vector_property",
        opts.vector_property.map(Value::from),
    );
    insert_opt(
        &mut args,
        "query_embedding",
        opts.query_embedding.map(Value::from),
    );
    insert_opt(&mut args, "top_k", opts.top_k.map(Value::from));
    insert_opt(&mut args, "valid_time", opts.valid_time.map(Value::from));
    insert_opt(
        &mut args,
        "transaction_time",
        opts.transaction_time.map(Value::from),
    );
    insert_opt(
        &mut args,
        "filter_label",
        opts.filter_label.map(Value::from),
    );
    insert_opt(&mut args, "limit", opts.limit.map(Value::from));
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
    dispatch_slow_read(&security, &state, "hybrid_query", Value::Object(args)).await
}

// ════════════════════════════════════════════════════════════════════════════
// query — [`ReadClass`], budgetable (#3353), NOT cursorable, slow (declarative
// query execution). Same wiring. The full structured-error contract, read-only
// enforcement, `language_unavailable`, and cursor → `unsupported_construct` are
// ALL produced by dispatch — the handler reimplements none of it, forwarding raw
// arguments (including `use_cursor` / `cursor`, so dispatch can reject them) and
// returning the result verbatim.
// ════════════════════════════════════════════════════════════════════════════

/// Request body for [`query`] — an **all-optional** mirror of the legacy
/// `QueryRequest` plus the #3353 token budget and the #3360 cursor parameters
/// (carried only so the dispatch layer can reject them with a structured
/// `unsupported_construct`; the `query` tool is not cursorable in v1). `language`
/// and `query` are logically required, but requiredness stays a **runtime**
/// concern the dispatch layer enforces; keeping every field optional restores a
/// named, per-operation `/openapi.json` schema.
///
/// `params` (Cypher `$param` bindings; numeric arrays are treated as embeddings)
/// and `limits` (the #3368 per-call resource-limit override) round-trip through
/// the rebuilt args map unchanged. `limits` is passed through as a raw [`Value`]
/// so the handler needs no dependency on the internal override type.
#[derive(Debug, Default, Deserialize)]
pub struct QueryBody {
    /// Query language: "cypher" or "aql" (logically required; enforced at
    /// dispatch).
    pub language: Option<String>,
    /// The read-only query string to execute (logically required; enforced at
    /// dispatch).
    pub query: Option<String>,
    /// Optional `$param` bindings (Cypher only). Numeric arrays are treated as
    /// embeddings.
    pub params: Option<Map<String, Value>>,
    /// Maximum number of rows to return (default 100, capped at 10000).
    pub limit: Option<usize>,
    /// Optional per-call resource-limit override (#3368): `{timeout_ms,
    /// max_result_rows, max_response_bytes}`. Passed through verbatim.
    pub limits: Option<Value>,
    /// #3353: cap the response at roughly this many tokens (utf8_bytes / 4).
    pub max_response_tokens: Option<u64>,
    /// #3353: byte-exact response cap.
    pub max_response_bytes: Option<u64>,
    /// #3353: property keys to protect first as the response degrades.
    pub priority_properties: Option<Vec<String>>,
    /// #3360: not supported by the `query` tool in v1 — forwarded so the dispatch
    /// layer returns a structured `unsupported_construct` (no silent fallback).
    pub use_cursor: Option<bool>,
    /// #3360: not supported by the `query` tool in v1 (see `use_cursor`).
    pub cursor: Option<String>,
}

/// `query` — execute a single read-only declarative statement (Cypher or AQL)
/// and return structured rows + column metadata, or the query tool's structured
/// error payload. [`ReadClass`]; budgetable (#3353), not cursorable. HTTP + MCP.
///
/// Forwards raw arguments through `dispatch_tool_json` (budget applies) under the
/// B4 resource-limits wiring, because query execution is potentially slow. The
/// tool's full contract — mutating-clause rejection (`read_only_violation`),
/// `language_unavailable` when `cypher` is not compiled, cursor →
/// `unsupported_construct`, and the structured `{error:{kind,message,clause?,
/// language}}` payload with the additive #3234 `code`/`retriable` — is produced
/// entirely by dispatch and returned verbatim.
#[post("/query")]
#[api_doc(
    description = "Execute a single read-only Cypher or AQL statement, returning structured rows and columns",
    mcp
)]
pub async fn query(
    _auth: Authorized<ReadClass>,
    security: ServerSecurityState,
    state: ServerState,
    Json(opts): Json<QueryBody>,
) -> Result<Json<Value>, AletheiaHttpError> {
    let mut args = Map::new();
    insert_opt(&mut args, "language", opts.language.map(Value::from));
    insert_opt(&mut args, "query", opts.query.map(Value::from));
    insert_opt(&mut args, "params", opts.params.map(Value::Object));
    insert_opt(&mut args, "limit", opts.limit.map(Value::from));
    insert_opt(&mut args, "limits", opts.limits);
    insert_budget(
        &mut args,
        opts.max_response_tokens,
        opts.max_response_bytes,
        opts.priority_properties,
    );
    insert_opt(&mut args, "use_cursor", opts.use_cursor.map(Value::from));
    insert_opt(&mut args, "cursor", opts.cursor.map(Value::from));
    dispatch_slow_read(&security, &state, "query", Value::Object(args)).await
}

// ════════════════════════════════════════════════════════════════════════════
// list_vector_indexes — [`ReadClass`], NOT budgetable, NOT cursorable, no
// arguments. Forwards through the typed per-tool method, inline (like
// count_nodes).
// ════════════════════════════════════════════════════════════════════════════

/// `list_vector_indexes` — list all active vector indexes and their
/// configuration (property, dimensions, distance metric). [`ReadClass`]; not
/// budgetable, not cursorable, takes no arguments. HTTP + MCP tool. Forwards
/// through the typed `AletheiaMcpServer::list_vector_indexes` method inline.
#[get("/vector/indexes")]
#[api_doc(
    description = "List all active vector indexes and their configuration (property, dimensions, metric)",
    mcp
)]
pub async fn list_vector_indexes(_auth: Authorized<ReadClass>, state: ServerState) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.list_vector_indexes(ListVectorIndexesRequest {}))
}
