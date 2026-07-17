//! Schema + stats + extent + batch tools: `get_schema`, `temporal_extent`,
//! `database_stats`, and the atomic multi-write `apply_batch` (Issue #3524, PR6).
//!
//! Each is a single `#[get(...)]`/`#[post(...)]` + `#[api_doc(description=...,
//! mcp)]` handler, so one definition surfaces on **HTTP**, **MCP-over-HTTP**
//! (`/mcp`), and **OpenAPI** at once. The response body is produced by the
//! *exact* main-crate MCP surface, so it is byte-identical to the legacy MCP
//! surface — asserted in the parity suite against the shared
//! [`AletheiaMcpServer`](aletheiadb::mcp::AletheiaMcpServer) over the same
//! `Arc<AletheiaDB>`.
//!
//! # Class diversity: the first Metrics tool + a Write in a read-heavy slice
//!
//! Unlike the earlier read-dominated clusters, this slice spans **three**
//! access classes, each verified against `tests/parity/inventory.json`:
//!
//! | Tool | Class | Budgetable (#3353) | Cursorable (#3360) | Forwarding |
//! |------|-------|--------------------|--------------------|------------|
//! | `get_schema` | [`ReadClass`] | **yes** | no | raw args → `dispatch_tool_json` under the B4 slow-read guard |
//! | `temporal_extent` | [`ReadClass`] | no | no | typed method, inline |
//! | `database_stats` | [`MetricsClass`] | no | no | typed method, inline |
//! | `apply_batch` | [`WriteClass`] | no | no | typed method, inline |
//!
//! The **single source of truth** for the budget is `aletheiadb::mcp`'s
//! `BUDGETABLE_READ_TOOLS` and for the cursor the `inject_cursor_schema_params`
//! set. Verified against source: **only `get_schema` is budgetable**, and **none
//! of the four is cursorable**. That drives two forwarding shapes:
//!
//! 1. **`get_schema`** — budgetable (#3353) + potentially slow (a bi-temporal
//!    `as_of_*` schema snapshot scans up to `max_schema_as_of_entities` version
//!    heads). Forwards the raw JSON arguments through
//!    [`AletheiaMcpServer::dispatch_tool_json`](aletheiadb::mcp::AletheiaMcpServer::dispatch_tool_json)
//!    (so the #3353 budget pipeline, read off the raw arguments in
//!    `dispatch_tool`, applies) via the process-lifetime shared
//!    [`ServerState::mcp_server`](crate::state::ServerState::mcp_server). It runs
//!    under the **exact** B4 resource-limits wiring `traverse` / `list_nodes` use
//!    — a bounded in-flight admission guard released by RAII on any exit, a
//!    per-query wall-clock [`run_with_timeout`], and a serialized-byte cap
//!    ([`check_byte_cap_of`]) — because the `as_of` scan is a potentially-slow
//!    read. It is registered in
//!    [`crate::edge_tools::DISPATCH_ROUTED_READ_TOOLS`] with the hardcoded literal
//!    `"get_schema"` so the shared `dispatch_pinned_names_match_routed_class`
//!    conformance test proves a read-gated handler can never pin a write tool's
//!    name.
//! 2. **`temporal_extent` / `database_stats` / `apply_batch`** — NOT budgetable,
//!    NOT cursorable. They forward through the exact public **typed** per-tool
//!    method (`AletheiaMcpServer::temporal_extent` / `::database_stats` /
//!    `::apply_batch`), inline (like `count_nodes` / the node writes) — zero
//!    reserialization, reusing only already-public symbols. `apply_batch`
//!    forwards the whole [`ApplyBatchRequest`] verbatim, so the entire #3231
//!    contract — ordered all-or-nothing commit, `$alias`/`$<index>` refs, static
//!    pre-transaction validation (forward/unknown/duplicate refs, malformed ops,
//!    over-cap rejection), per-op `details.failed_op_index`, the #3209 in-batch
//!    DETACH ledger, and the `ref_map` — is preserved with **zero**
//!    reimplementation.
//!
//! All four take a typed request/query struct (`#[derive(Deserialize)]`) so
//! `/openapi.json` gets a named, per-operation schema instead of the shared
//! generic `Value` a raw `Json<Value>` body would emit. `get_schema` and
//! `temporal_extent`'s bodies are all-optional (every field `Option<_>`);
//! `database_stats` takes no arguments; `apply_batch` forwards the main-crate
//! request type directly (its `operations` array stays raw JSON so a malformed
//! op is rejected with its precise index).
//!
//! The MCP tool result carries in-band errors (e.g. a per-op batch failure →
//! `{"error":{"code":...,"details":{"failed_op_index":...}}}`); we return that
//! JSON verbatim with a 200, matching the MCP method's `String` return exactly.

use crate::edge_tools::insert_budget;
use crate::security::resource_limits::{check_byte_cap_of, run_with_timeout};
use crate::security::{Authorized, MetricsClass, ReadClass, ServerSecurityState, WriteClass};
use crate::state::ServerState;
use aletheiadb::http::AletheiaHttpError;
use aletheiadb::mcp::{
    AletheiaMcpServer, ApplyBatchRequest, DatabaseStatsRequest, TemporalExtentRequest,
};
use autumn_web::prelude::{Json, Query, get, post};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Parse an MCP tool method's JSON string result into a [`Value`] for the HTTP
/// response. A non-JSON result (should not happen) degrades to a JSON string.
fn tool_json(s: String) -> Json<Value> {
    Json(serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)))
}

// ════════════════════════════════════════════════════════════════════════════
// get_schema — [`ReadClass`], budgetable (#3353), NOT cursorable, potentially
// slow (a bi-temporal `as_of_*` schema snapshot scans up to
// `max_schema_as_of_entities` version heads). Forwards raw arguments through
// `dispatch_tool_json` (budget applies) under the B4 resource-limits wiring.
// ════════════════════════════════════════════════════════════════════════════

/// Query options for [`get_schema`] — the optional bi-temporal `as_of_*`
/// coordinates plus the #3353 token budget. Every field optional so a plain
/// current-state schema request (`GET /schema`) still deserializes;
/// `get_schema` never errors on missing arguments (an empty database returns an
/// empty schema).
#[derive(Debug, Default, Deserialize)]
pub struct GetSchemaQuery {
    /// #3225-convention valid time (ISO 8601 / RFC 3339 or microseconds since
    /// epoch): switches to a bi-temporal schema snapshot. When omitted while
    /// `as_of_transaction_time` is set, defaults to the current time.
    pub as_of_valid_time: Option<String>,
    /// Transaction time (ISO 8601 / RFC 3339 or microseconds since epoch):
    /// switches to a bi-temporal schema snapshot. When omitted while
    /// `as_of_valid_time` is set, defaults to the current time.
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
}

/// `get_schema` — discover the graph's schema: distinct node labels and edge
/// types, their counts, and the property keys observed on each, optionally as of
/// a bi-temporal point. [`ReadClass`]; budgetable (#3353), not cursorable. HTTP +
/// MCP tool.
///
/// Forwards raw arguments through `dispatch_tool_json` (so the #3353 budget
/// applies) under the B4 resource-limits wiring (in-flight guard, wall-clock
/// timeout, byte cap), because a bi-temporal `as_of_*` snapshot is a
/// potentially-slow read (scans up to `max_schema_as_of_entities` version heads).
#[get("/schema")]
#[api_doc(
    description = "Discover node labels, edge types, and property keys with counts, optionally as of a bi-temporal point",
    mcp
)]
pub async fn get_schema(
    _auth: Authorized<ReadClass>,
    security: ServerSecurityState,
    state: ServerState,
    Query(opts): Query<GetSchemaQuery>,
) -> Result<Json<Value>, AletheiaHttpError> {
    // B4 resource-limits wiring (#3542 / #3550), identical to `traverse` /
    // `list_nodes`: a bounded in-flight admission guard (503 UNAVAILABLE at
    // capacity, released by RAII on any exit), a per-query wall-clock timeout
    // (429 on deadline), and a serialized-byte cap on the exact wire response
    // (413). Generous defaults make this a no-op on the parity path.
    let limits = security.limits();
    let _guard = security.in_flight().try_acquire()?;

    // Route through the shared server's `dispatch_tool_json` so the #3353 token
    // budget (read off the raw arguments) applies — the typed `get_schema`
    // method bypasses that pipeline.
    let server = state.mcp_server();
    let mut args = Map::new();
    if let Some(v) = opts.as_of_valid_time {
        args.insert("as_of_valid_time".to_string(), Value::from(v));
    }
    if let Some(v) = opts.as_of_transaction_time {
        args.insert("as_of_transaction_time".to_string(), Value::from(v));
    }
    insert_budget(
        &mut args,
        opts.max_response_tokens,
        opts.max_response_bytes,
        opts.priority_properties,
    );
    let args = Value::Object(args);

    let out = run_with_timeout(limits.timeout, false, async move {
        server.dispatch_tool_json("get_schema", args)
    })
    .await?;

    let value = serde_json::from_str::<Value>(&out).unwrap_or(Value::String(out));
    // Cap against the exact serialized response bytes (undercount-proof).
    check_byte_cap_of(&value, limits.max_response_bytes)?;
    Ok(Json(value))
}

// ════════════════════════════════════════════════════════════════════════════
// temporal_extent — [`ReadClass`], NOT budgetable, NOT cursorable. O(1)
// write-time-maintained aggregate reads → typed forwarding to the legacy method,
// inline (no guard, no dispatch_tool_json).
// ════════════════════════════════════════════════════════════════════════════

/// Query options for [`temporal_extent`] — the optional `by_label` breakdown
/// flag. Optional so a bare `GET /temporal_extent` returns the overall bounds.
#[derive(Debug, Default, Deserialize)]
pub struct TemporalExtentQuery {
    /// When `true`, additionally return the same `{valid_time, transaction_time}`
    /// bounds per node label and per edge type. Defaults to `false` (overall
    /// bounds only). Per-label bounds are hot-tier-only.
    pub by_label: Option<bool>,
}

/// `temporal_extent` — report the dataset's queryable bi-temporal extent: the
/// earliest/latest valid-time and transaction-time coordinates across recorded
/// history (including expired/superseded versions), as RFC3339 strings, so a
/// caller can calibrate `AS OF` queries. [`ReadClass`]; not budgetable, not
/// cursorable. HTTP + MCP tool. Forwards through the typed
/// `AletheiaMcpServer::temporal_extent` method inline (O(1) aggregate reads).
#[get("/temporal_extent")]
#[api_doc(
    description = "Report the dataset's queryable bi-temporal extent (earliest/latest valid and transaction time)",
    mcp
)]
pub async fn temporal_extent(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Query(opts): Query<TemporalExtentQuery>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.temporal_extent(TemporalExtentRequest {
        by_label: opts.by_label,
    }))
}

// ════════════════════════════════════════════════════════════════════════════
// database_stats — [`MetricsClass`] (the first Metrics tool in the migration —
// NOT ReadClass; verified against inventory `metrics`), NOT budgetable, NOT
// cursorable, no arguments. Typed forwarding to the legacy method, inline.
// ════════════════════════════════════════════════════════════════════════════

/// `database_stats` — a holistic database statistics snapshot in one call:
/// current graph size, bi-temporal depth (version counts + anchor/delta
/// compression), hot/warm/cold storage-tier distribution, and WAL durability
/// state. [`MetricsClass`] (not [`ReadClass`]); takes no arguments; every field
/// is an O(1)/cached counter read. HTTP + MCP tool. Forwards through the typed
/// `AletheiaMcpServer::database_stats` method inline.
#[get("/database_stats")]
#[api_doc(
    description = "Holistic database snapshot: current size, bi-temporal depth, storage tiers, and WAL state",
    mcp
)]
pub async fn database_stats(auth: Authorized<MetricsClass>, state: ServerState) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    // Admin-gate the changefeed per-principal identity breakdown (Issue #3678):
    // `database_stats` is `MetricsClass`, so a monitoring/reader credential
    // reaches this route, but only an admin caller may see the roster of other
    // principals' ids + live subscription counts. The scalar aggregates stay
    // available to every metrics-tier caller. The freshly-constructed
    // `AletheiaMcpServer` is anonymous, so admin-ness MUST come from this
    // route's own authenticated principal, not the (absent) MCP session.
    let include_per_principal = auth
        .principal()
        .role
        .allows(aletheiadb::auth::AccessClass::Admin);
    tool_json(server.database_stats_with_visibility(DatabaseStatsRequest {}, include_per_principal))
}

// ════════════════════════════════════════════════════════════════════════════
// apply_batch — [`WriteClass`], NOT budgetable, NOT cursorable. Typed forwarding
// to the legacy `apply_batch` (a single all-or-nothing `WriteTransaction`). The
// full #3231 contract is preserved by forwarding the request verbatim — the
// handler reimplements NONE of the batch validation, ref resolution, per-op
// error reporting, or DETACH ledger.
// ════════════════════════════════════════════════════════════════════════════

/// `apply_batch` — apply an ordered batch of write operations
/// (`create_node`/`create_edge`/`update_node`/`update_edge`/`delete_node`/
/// `delete_edge`) atomically, committing **all-or-nothing** in one
/// `WriteTransaction`. [`WriteClass`]. HTTP + MCP tool.
///
/// Forwards the whole [`ApplyBatchRequest`] to the exact legacy
/// `AletheiaMcpServer::apply_batch` typed method and returns its `String`
/// verbatim, so every #3231 contract is preserved byte-for-byte with **zero**
/// reserialization: `$alias`/`$<index>` local refs; static pre-transaction
/// validation of forward/unknown/duplicate refs, malformed ops, and over-cap
/// batches (default 1000 ops); per-operation `details.failed_op_index` (`null`
/// for commit-phase / over-cap failures); the #3209 in-batch DETACH contract
/// against committed AND batch-created edges; and the success shape (per-op
/// results in input order + the `ref_map` alias→committed-id). The `operations`
/// array stays raw JSON in the request type so a malformed operation is rejected
/// with its precise array index before any transaction opens.
#[post("/batch")]
#[api_doc(
    description = "Apply an ordered batch of write operations atomically (all-or-nothing), with local $ref endpoints",
    mcp
)]
pub async fn apply_batch(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<ApplyBatchRequest>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.apply_batch(req))
}
