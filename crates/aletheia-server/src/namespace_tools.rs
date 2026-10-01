//! Namespace management tools: `create_namespace`, `list_namespaces`, and
//! `describe_namespace` (Issue #3349, PR3b).
//!
//! Each is a single `#[get(...)]`/`#[post(...)]` + `#[api_doc(description=...,
//! mcp)]` handler, so one definition surfaces on **HTTP**, **MCP-over-HTTP**
//! (`/mcp`), and **OpenAPI** at once. The response body is produced by the
//! *exact* main-crate MCP surface (the typed `AletheiaMcpServer` methods
//! `create_namespace` / `list_namespaces` / `describe_namespace`), so it is
//! byte-identical to the legacy MCP surface — zero reserialization, reusing only
//! already-public symbols.
//!
//! | Tool | Class | Budgetable (#3353) | Cursorable (#3360) | Forwarding |
//! |------|-------|--------------------|--------------------|------------|
//! | `create_namespace` | [`WriteClass`] | no | no | typed method, inline |
//! | `list_namespaces` | [`ReadClass`] | no | no | typed method, inline |
//! | `describe_namespace` | [`ReadClass`] | no | no | typed method, inline |
//!
//! None is budgetable / cursorable, so all three forward through the exact
//! public **typed** per-tool method inline (like `count_nodes` /
//! `temporal_extent` / the node writes) — they are NOT registered in
//! [`crate::edge_tools::ALL_DISPATCH_ROUTED`] because they do not pin a literal
//! tool name through `dispatch_tool_json`. Their class is carried on the
//! handler's own `Authorized<C>` gate and pinned by
//! `tests/parity/inventory.json` (`registry_matches_inventory_exactly` +
//! `mcp_catalog_equals_inventory_exactly`).
//!
//! The `NamespaceError` → structured #3234 code mapping (`AlreadyExists` →
//! `CONFLICT`, `InvalidName`/`ReservedPropertyKey`/`Immutable` →
//! `INVALID_ARGUMENT`, `NotFound` → `NOT_FOUND` with `details.namespace`) is
//! entirely inside the forwarded main-crate method, so it is preserved verbatim.

use crate::security::{Authorized, ReadClass, WriteClass};
use crate::state::ServerState;
use aletheiadb::mcp::{
    AletheiaMcpServer, CreateNamespaceRequest, DescribeNamespaceRequest, ListNamespacesRequest,
};
use autumn_web::prelude::{Json, get, post};
use serde_json::Value;

/// Parse an MCP tool method's JSON string result into a [`Value`] for the HTTP
/// response. A non-JSON result (should not happen) degrades to a JSON string.
fn tool_json(s: String) -> Json<Value> {
    Json(serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)))
}

// ════════════════════════════════════════════════════════════════════════════
// create_namespace — [`WriteClass`] (same class as create_node). Typed
// forwarding to the legacy `create_namespace` method, inline.
// ════════════════════════════════════════════════════════════════════════════

/// `create_namespace` — register an agent-scoped namespace so it is
/// listable/describable even when empty (Issue #3349). [`WriteClass`]. HTTP +
/// MCP tool. Returns the created `{name, description, created_at}`; a duplicate
/// name (or the implicit `default`) is a `CONFLICT`, a malformed name or the
/// reserved `all` selector is `INVALID_ARGUMENT`.
#[post("/namespaces")]
#[api_doc(
    description = "Register an agent-scoped namespace (creatable/listable even when empty), with an optional description",
    mcp
)]
pub async fn create_namespace(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<CreateNamespaceRequest>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.create_namespace(req))
}

// ════════════════════════════════════════════════════════════════════════════
// list_namespaces — [`ReadClass`], no arguments. Typed forwarding to the legacy
// `list_namespaces` method, inline.
// ════════════════════════════════════════════════════════════════════════════

/// `list_namespaces` — list every registered namespace (the implicit `default`
/// first, then others in creation order) with per-namespace current-state
/// node/edge counts (Issue #3349). [`ReadClass`]; takes no arguments. HTTP + MCP
/// tool. Forwards through the typed `AletheiaMcpServer::list_namespaces` method
/// inline (per-namespace counts are O(1) membership-index reads).
#[get("/namespaces")]
#[api_doc(
    description = "List all registered namespaces (default first) with per-namespace node/edge counts",
    mcp
)]
pub async fn list_namespaces(_auth: Authorized<ReadClass>, state: ServerState) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.list_namespaces(ListNamespacesRequest {}))
}

// ════════════════════════════════════════════════════════════════════════════
// describe_namespace — [`ReadClass`], but a **POST** with a JSON body (mirroring
// the ReadClass `find_nodes_at_time` finder): a required scalar argument does
// not round-trip through the MCP projection of a GET `Query` extractor, so — as
// with `find_nodes_at_time` — the argument rides a JSON body (`{body:{name}}` on
// the MCP surface). The class is carried by the `Authorized<ReadClass>` gate, so
// the HTTP method is orthogonal to the access class. Typed forwarding, inline.
// ════════════════════════════════════════════════════════════════════════════

/// `describe_namespace` — describe a single namespace by name (Issue #3349).
/// [`ReadClass`]. HTTP + MCP tool. Returns
/// `{name, description, created_at, node_count, edge_count}`; an unregistered
/// name returns `NOT_FOUND` with `details.namespace`. Forwards through the typed
/// `AletheiaMcpServer::describe_namespace` method inline.
#[post("/namespaces/describe")]
#[api_doc(
    description = "Describe a single namespace by name (metadata + per-namespace node/edge counts)",
    mcp
)]
pub async fn describe_namespace(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<DescribeNamespaceRequest>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.describe_namespace(req))
}
