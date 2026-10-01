//! Constraints / index-admin + lineage + audit-chain tools: the write
//! index/constraint admin `enable_vector_index` / `enable_unique_constraint`,
//! the read `list_unique_constraints`, the derivation-lineage queries
//! `lineage_upstream` / `lineage_downstream`, and the provenance-hash-chain
//! tools `audit_export` / `verify_chain` / `export_chain_head` (Issue #3524,
//! PR7 — the FINAL tool slice, completing all 46 MCP tools on the autumn
//! surface).
//!
//! Each is a single `#[get(...)]`/`#[post(...)]` + `#[api_doc(description=...,
//! mcp)]` handler, so one definition surfaces on **HTTP**, **MCP-over-HTTP**
//! (`/mcp`), and **OpenAPI** at once. The response body is produced by the
//! *exact* main-crate MCP surface, so it is byte-identical to the legacy MCP
//! surface — asserted in the parity suite against the shared
//! [`AletheiaMcpServer`](aletheiadb::mcp::AletheiaMcpServer) over the same
//! `Arc<AletheiaDB>`.
//!
//! # Forwarding: all eight are typed / inline (no budget, no cursor, no guard)
//!
//! The **single source of truth** for which reads honor the Issue #3353 token
//! budget is `aletheiadb::mcp`'s `BUDGETABLE_READ_TOOLS`, and for the Issue
//! #3360 cursor the `inject_cursor_schema_params` set. Verified against source,
//! **none of these eight is budgetable and none is cursorable**. So — unlike the
//! read-heavy PR5/PR6 clusters — there is **no** dispatch-routed slow read here:
//! no `DISPATCH_ROUTED_READ_TOOLS` addition, no in-flight admission guard, no
//! wall-clock timeout, no byte cap. Every handler forwards **inline**, in one of
//! two shapes that mirror the proven PR3–PR6 patterns exactly:
//!
//! 1. **Five tools with a public typed per-tool method** — `enable_vector_index`
//!    / `enable_unique_constraint` (writes) and `list_unique_constraints` /
//!    `lineage_upstream` / `lineage_downstream` (reads) — forward through that
//!    method's re-exported request struct
//!    (`AletheiaMcpServer::enable_vector_index(req)` etc.), inline (like the edge
//!    writes / `count_nodes` / `list_vector_indexes`) — zero reserialization,
//!    reusing only already-public symbols. `lineage_upstream` / `_downstream`
//!    forward the whole [`LineageQueryRequest`] verbatim, so the entire #3371
//!    contract — version-pinned root ref, the #3226 `has_more`/`next_offset`
//!    pagination, per-entry `depth` + `FactStatus`, the `as_of_transaction_time`
//!    scoping, and the #3234 structured error codes (`NOT_FOUND` for a dangling
//!    ref, `INVALID_ARGUMENT` for a bad entity kind / self-cycle,
//!    `FAILED_PRECONDITION` for an already-recorded write) — is preserved with
//!    **zero** reimplementation.
//! 2. **Three tools with NO public typed method** — the provenance-hash-chain
//!    tools `audit_export` / `verify_chain` / `export_chain_head` (their
//!    `handle_*` methods are private and their request types are not
//!    re-exported). Each takes a **local** typed body struct (mirroring the
//!    main-crate request shape one-to-one so `/openapi.json` gets a named
//!    per-operation component — never a generic `Value`, the #3589 lesson) and
//!    forwards through
//!    [`AletheiaMcpServer::dispatch_tool_json`](aletheiadb::mcp::AletheiaMcpServer::dispatch_tool_json)
//!    with a **hardcoded literal** tool name — the only public entry for these
//!    three. Being non-budgetable / non-cursorable, `dispatch_tool_json`
//!    reproduces a hypothetical typed method's output byte-for-byte (same
//!    `handle_*`), so the full #3351 contract — signed offline-verifiable audit
//!    artifact + redaction, the full / entity-scoped / anchor-extension verify
//!    modes, the exported chain head, and the "chain not enabled" →
//!    `FAILED_PRECONDITION` — is preserved with no main-crate change. Because
//!    each pins its **own** tool name (a read handler pinning `"verify_chain"`
//!    routes to `verify_chain`), there is structurally no cross-tool escalation,
//!    so they are intentionally NOT registered in
//!    [`crate::edge_tools::DISPATCH_ROUTED_READ_TOOLS`] (that table binds the
//!    budgetable/cursorable slow reads whose pinned name could otherwise differ
//!    from the route).
//!
//! The two writes are classified [`WriteClass`] and the six reads [`ReadClass`]
//! — each verified BY HAND against `tests/parity/inventory.json`'s
//! `access_class` (`enable_vector_index`=write, `enable_unique_constraint`=write,
//! `list_unique_constraints`=read, `lineage_upstream`=read,
//! `lineage_downstream`=read, `audit_export`=read, `verify_chain`=read,
//! `export_chain_head`=read). The marker rides in each handler's `Authorized<C>`
//! signature (the one place the class is declared), which autumn replays for
//! `/mcp tools/call`. The write handlers (and the POST-body reads) take their
//! arguments as a JSON **body** (autumn maps `Json<T>` to the reserved `body`
//! MCP argument key), so the `tools/call` `arguments` wrap the request under
//! `"body"`.
//!
//! The MCP tool result carries in-band errors (e.g. a dangling lineage ref →
//! `{"error":{"code":"NOT_FOUND",...}}`, a disabled chain →
//! `{"error":{"code":"FAILED_PRECONDITION",...}}`); we return that JSON verbatim
//! with a 200, matching the MCP method's `String` return exactly.

use crate::security::{Authorized, ReadClass, WriteClass};
use crate::state::ServerState;
use aletheiadb::mcp::{
    EnableUniqueConstraintRequest, EnableVectorIndexRequest, LineageQueryRequest,
    ListUniqueConstraintsRequest,
};
use autumn_web::prelude::{Json, get, post};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Parse an MCP tool method's JSON string result into a [`Value`] for the HTTP
/// response. A non-JSON result (should not happen) degrades to a JSON string.
fn tool_json(s: String) -> Json<Value> {
    Json(serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)))
}

/// Fold a present optional value into the raw MCP arguments object under `key`.
fn insert_opt(args: &mut Map<String, Value>, key: &str, val: Option<Value>) {
    if let Some(v) = val {
        args.insert(key.to_string(), v);
    }
}

// ════════════════════════════════════════════════════════════════════════════
// enable_vector_index — [`WriteClass`], NOT budgetable, NOT cursorable. Typed
// forwarding to the legacy `enable_vector_index` method, inline.
// ════════════════════════════════════════════════════════════════════════════

/// `enable_vector_index` — enable HNSW vector indexing on a node property so
/// `find_similar` / `hybrid_query` can k-NN-search its embeddings. [`WriteClass`].
/// HTTP + MCP tool. Forwards the whole [`EnableVectorIndexRequest`] to the exact
/// legacy `AletheiaMcpServer::enable_vector_index` typed method and returns its
/// `String` verbatim (`{success, property_name, dimensions, distance_metric}`
/// on success; a structured error otherwise).
#[post("/vector/indexes")]
#[api_doc(
    description = "Enable HNSW vector indexing on a node property for k-NN similarity search",
    mcp
)]
pub async fn enable_vector_index(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<EnableVectorIndexRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.enable_vector_index(req))
}

// ════════════════════════════════════════════════════════════════════════════
// enable_unique_constraint — [`WriteClass`], NOT budgetable, NOT cursorable.
// Typed forwarding to the legacy `enable_unique_constraint` method, inline.
// ════════════════════════════════════════════════════════════════════════════

/// `enable_unique_constraint` — enforce that a `property` is unique within a node
/// `label` (e.g. `Person.email`). [`WriteClass`]. HTTP + MCP tool. Forwards the
/// whole [`EnableUniqueConstraintRequest`] to the exact legacy
/// `AletheiaMcpServer::enable_unique_constraint` typed method and returns its
/// `String` verbatim (`{success, label, property}` on success; a structured
/// error otherwise).
#[post("/constraints/unique")]
#[api_doc(
    description = "Enable a uniqueness constraint on a node label + property pair",
    mcp
)]
pub async fn enable_unique_constraint(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<EnableUniqueConstraintRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.enable_unique_constraint(req))
}

// ════════════════════════════════════════════════════════════════════════════
// list_unique_constraints — [`ReadClass`], NOT budgetable, NOT cursorable, no
// arguments. Typed forwarding to the legacy method, inline (like
// list_vector_indexes / count_nodes).
// ════════════════════════════════════════════════════════════════════════════

/// `list_unique_constraints` — list all active uniqueness constraints as
/// `{label, property}` pairs. [`ReadClass`]; not budgetable, not cursorable,
/// takes no arguments. HTTP + MCP tool. Forwards through the typed
/// `AletheiaMcpServer::list_unique_constraints` method inline.
#[get("/constraints/unique")]
#[api_doc(
    description = "List all active uniqueness constraints (label + property pairs)",
    mcp
)]
pub async fn list_unique_constraints(
    _auth: Authorized<ReadClass>,
    state: ServerState,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.list_unique_constraints(ListUniqueConstraintsRequest {}))
}

// ════════════════════════════════════════════════════════════════════════════
// lineage_upstream / lineage_downstream — [`ReadClass`], NOT budgetable, NOT
// cursorable. Typed forwarding to the legacy methods, inline. The whole
// LineageQueryRequest is forwarded verbatim, so the #3371 contract (version-
// pinned root, #3226 has_more/next_offset, per-entry depth + FactStatus,
// as_of_transaction_time scoping) and the #3234 error codes are preserved.
// ════════════════════════════════════════════════════════════════════════════

/// `lineage_upstream` — the transitive **evidence chain**: what a fact was
/// derived from (Issue #3371). [`ReadClass`]. HTTP + MCP tool. Forwards the whole
/// [`LineageQueryRequest`] to the exact legacy `AletheiaMcpServer::lineage_upstream`
/// typed method, so the version-pinned root ref, the #3226 `has_more` /
/// `next_offset` pagination, each entry's `depth` + `FactStatus`, the
/// `as_of_transaction_time` scoping, and the #3234 structured error codes
/// (`NOT_FOUND` for a dangling ref, `INVALID_ARGUMENT` for a bad entity kind) are
/// preserved byte-for-byte.
#[post("/lineage/upstream")]
#[api_doc(
    description = "Query the upstream derivation lineage of a fact: what it was derived from (evidence chain)",
    mcp
)]
pub async fn lineage_upstream(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<LineageQueryRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.lineage_upstream(req))
}

/// `lineage_downstream` — the transitive **blast radius**: what has been derived
/// from a fact (Issue #3371), the report a caller consults before retracting it.
/// [`ReadClass`]. HTTP + MCP tool. Forwards the whole [`LineageQueryRequest`] to
/// the exact legacy `AletheiaMcpServer::lineage_downstream` typed method (same
/// contract preservation as [`lineage_upstream`]).
#[post("/lineage/downstream")]
#[api_doc(
    description = "Query the downstream derivation lineage of a fact: what has been derived from it (blast radius)",
    mcp
)]
pub async fn lineage_downstream(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<LineageQueryRequest>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.lineage_downstream(req))
}

// ════════════════════════════════════════════════════════════════════════════
// Local-body dispatch reads (NOT budgetable, NOT cursorable, NO public typed
// method): the provenance-hash-chain tools (Issue #3351). A local typed body
// (mirroring the main-crate request shape one-to-one so /openapi.json gets a
// named per-operation component) forwarded through `dispatch_tool_json` with a
// hardcoded literal tool name — the only public entry for these three.
// Byte-identical to a hypothetical typed method (same `handle_*`), so no
// main-crate change is required. Each pins its OWN tool name (no cross-tool
// escalation), so none is registered in DISPATCH_ROUTED_READ_TOOLS.
// ════════════════════════════════════════════════════════════════════════════

/// Request body for [`audit_export`] — mirrors the main-crate
/// `AuditExportRequest`. `entity_type` + `entity_id` are required; `database_id`
/// and `redact_keys` are optional (omitted → the main-crate `#[serde(default)]`
/// applies, i.e. the default database id and no redaction).
#[derive(Debug, Deserialize)]
pub struct AuditExportBody {
    /// Entity kind to export: `node` or `edge`.
    pub entity_type: String,
    /// The unique identifier of the node or edge to export.
    pub entity_id: u64,
    /// Optional database identity recorded in the artifact metadata.
    pub database_id: Option<String>,
    /// Optional list of property keys to redact at export; their values are
    /// omitted but the redaction is recorded and remains verifiable.
    pub redact_keys: Option<Vec<String>>,
}

/// `audit_export` — produce a signed, offline-verifiable evidence artifact of an
/// entity's complete bi-temporal history (Issue #3351/#3358). [`ReadClass`]. HTTP
/// + MCP tool.
///
/// Forwards through `dispatch_tool_json("audit_export", ...)` (the only public
/// entry — its `handle_audit_export` is private), so the full contract is
/// preserved: the Ed25519 signing key is operator-provided out of band via
/// `ALETHEIADB_AUDIT_SIGNING_KEY` (a missing key is a structured
/// `FAILED_PRECONDITION`, never a silent unsigned export), the secret never
/// leaves the process (only the public key travels in the artifact), and
/// `redact_keys` omit their values while keeping the redaction verifiable.
#[post("/audit/export")]
#[api_doc(
    description = "Produce a signed, offline-verifiable audit artifact of an entity's complete bi-temporal history",
    mcp
)]
pub async fn audit_export(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<AuditExportBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("entity_type".to_string(), Value::from(req.entity_type));
    args.insert("entity_id".to_string(), Value::from(req.entity_id));
    insert_opt(&mut args, "database_id", req.database_id.map(Value::from));
    insert_opt(&mut args, "redact_keys", req.redact_keys.map(Value::from));
    tool_json(server.dispatch_tool_json("audit_export", Value::Object(args)))
}

/// Request body for [`verify_chain`] — mirrors the main-crate
/// `VerifyChainRequest`. Every field is optional: an empty body verifies the
/// **full** chain; `entity_kind` + `id` scope to one entity; `against` verifies
/// the current chain append-only-extends a previously exported head.
#[derive(Debug, Default, Deserialize)]
pub struct VerifyChainBody {
    /// For an entity-scoped verify, the entity kind: `node` or `edge` (requires
    /// `id`). Omit for a full-chain verify.
    pub entity_kind: Option<String>,
    /// For an entity-scoped verify, the entity id (requires `entity_kind`).
    pub id: Option<u64>,
    /// A previously-exported chain head (as returned by `export_chain_head`) to
    /// verify the current chain append-only-extends, detecting rollback
    /// (truncation) and fork (divergence).
    pub against: Option<Value>,
}

/// `verify_chain` — verify the tamper-evident provenance hash chain: full,
/// entity-scoped, or against an exported anchor (Issue #3351). [`ReadClass`].
/// HTTP + MCP tool.
///
/// Forwards through `dispatch_tool_json("verify_chain", ...)` (the only public
/// entry), so the full contract is preserved: the anchor > entity-scoped > full
/// precedence, the `{scope, passed, head_seq, head_digest, earliest_broken_seq,
/// reason, transactions_checked}` result shape, the `INVALID_ARGUMENT` on a bad
/// entity kind / malformed anchor, and — when the chain is not enabled for this
/// data dir — the structured `FAILED_PRECONDITION` (never a silent empty pass).
#[post("/chain/verify")]
#[api_doc(
    description = "Verify the tamper-evident provenance hash chain (full, entity-scoped, or against an exported anchor)",
    mcp
)]
pub async fn verify_chain(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<VerifyChainBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    insert_opt(&mut args, "entity_kind", req.entity_kind.map(Value::from));
    insert_opt(&mut args, "id", req.id.map(Value::from));
    insert_opt(&mut args, "against", req.against);
    tool_json(server.dispatch_tool_json("verify_chain", Value::Object(args)))
}

/// `export_chain_head` — export the current provenance-chain head as an external
/// anchor for offline storage and later fork/rollback detection via
/// `verify_chain`'s `against` argument (Issue #3351). [`ReadClass`]; takes no
/// arguments. HTTP + MCP tool.
///
/// Forwards through `dispatch_tool_json("export_chain_head", ...)` (the only
/// public entry), so the exported head payload is returned verbatim and — when
/// the chain is not enabled for this data dir — a structured
/// `FAILED_PRECONDITION` (never a fabricated head).
#[get("/chain/head")]
#[api_doc(
    description = "Export the current provenance-chain head as an external anchor for offline fork/rollback detection",
    mcp
)]
pub async fn export_chain_head(_auth: Authorized<ReadClass>, state: ServerState) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.dispatch_tool_json("export_chain_head", Value::Object(Map::new())))
}
