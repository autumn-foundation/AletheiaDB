// OWNED BY LANE B (security).
//
//! Per-MCP-tool access-class table (bypass-proof-by-construction).
//!
//! # Why a single source list (Lane B adversarial-review requirement)
//!
//! On the legacy MCP surface `authorize_tool` returns `Ok` for **unknown** tool
//! names (safe there only because the string dispatcher then errors on unknown).
//! On the autumn surface any dispatcher-routable tool name missing from the RBAC
//! class table would **bypass the class gate** — a privilege-escalation hole. We
//! close it two ways:
//!
//! 1. **The class lives ON the handler.** Every MCP-exposed handler declares its
//!    required class in its own signature as `Authorized<ReadClass>` /
//!    `Authorized<WriteClass>` / … — the one place a tool is defined. autumn
//!    replays `/mcp tools/call` through that same handler, so the per-request
//!    class gate (Lane B's enforcement in `Authorized<C>`) is carried by the
//!    routable handler itself and cannot be forgotten without deleting the
//!    extractor. This is the "unrepresentable, not just tested-for" half.
//!
//! 2. **One source list drives the class map.** autumn 0.5 derives the routable
//!    `/mcp` tool set from the `#[api_doc(mcp)]` handlers registered in
//!    `routes![..]` — a macro-expansion set we cannot *literally* fuse with a
//!    Rust data table. So we take the **documented fallback**: [`MCP_TOOL_CLASSES`]
//!    is the single list from which [`tool_access_class`] is derived, and the
//!    PR1 conformance test `mcp_routable_tools_are_all_classified`
//!    (`tests/server_parity.rs`) asserts the **live** routable set (from `/mcp`
//!    `tools/list`) equals this registry — so a routable tool that is not
//!    classified fails CI. Lane B asserts the same bidirectionally against
//!    `tests/parity/inventory.json`.
//!
//! This registry is anchored on `tests/parity/inventory.json` — the inventory's
//! `mcp.tools[].access_class` is the shared source of truth. It enumerates all
//! 61 tools upfront (coordinator-directed): `registry_matches_inventory_exactly`
//! checks it bidirectionally against the inventory, and
//! `mcp_routable_tools_are_all_classified` checks the live routable set is a
//! subset (every routable tool classified). Handlers become routable one slice
//! at a time during the port; the subset invariant keeps that honest.

use aletheiadb::auth::{AccessClass, Role};

/// The single source of truth pairing every MCP-exposed handler name with the
/// [`AccessClass`] it requires. Both [`tool_access_class`] and the conformance
/// tests derive from THIS list, so the routable-name set and the class-table
/// set cannot silently drift into two independently-maintained lists.
///
/// This registry is **inventory-anchored**: it enumerates all 61 tools from
/// `tests/parity/inventory.json` (`mcp.tools[].access_class`) upfront, mirroring
/// the legacy `src/mcp/auth.rs::TOOL_ACCESS_CLASSES` verbatim. The conformance
/// test `registry_matches_inventory_exactly` (`tests/security_rbac.rs`) proves
/// the registry equals the inventory exactly (both directions, name + class),
/// and `mcp_routable_tools_are_all_classified` (`tests/server_parity.rs`) proves
/// the **live** routable set is a subset of this registry (every routable tool
/// is classified — a routable-but-unclassified tool would bypass the class gate).
/// During the port the registry may legitimately hold entries whose handlers are
/// not yet routable; it never holds a routable tool that is unclassified.
pub const MCP_TOOL_CLASSES: &[(&str, AccessClass)] = &[
    // ---- Read: lookups, traversals, history, schema/extent discovery, and the
    // read-only declarative `query` tool.
    ("get_node", AccessClass::Read),
    ("list_nodes", AccessClass::Read),
    ("count_nodes", AccessClass::Read),
    ("get_edge", AccessClass::Read),
    ("list_edges", AccessClass::Read),
    ("count_edges", AccessClass::Read),
    ("get_outgoing_edges", AccessClass::Read),
    ("get_incoming_edges", AccessClass::Read),
    ("traverse", AccessClass::Read),
    ("find_similar", AccessClass::Read),
    // Embedding generation & text semantic search (Issue #2906) — read-only.
    ("embed_query", AccessClass::Read),
    ("embed_text", AccessClass::Read),
    ("semantic_search", AccessClass::Read),
    // Semantic-search analysis tools (Issue #2907) — read-only.
    ("semantic_path", AccessClass::Read),
    ("concept_analogy", AccessClass::Read),
    ("concept_mean", AccessClass::Read),
    ("find_duplicate_candidates", AccessClass::Read),
    ("semantic_horizon", AccessClass::Read),
    ("context_aspects", AccessClass::Read),
    ("list_vector_indexes", AccessClass::Read),
    ("list_unique_constraints", AccessClass::Read),
    ("get_node_at_time", AccessClass::Read),
    ("get_edge_at_time", AccessClass::Read),
    ("find_nodes_at_time", AccessClass::Read),
    ("list_changes", AccessClass::Read),
    ("await_changes", AccessClass::Read),
    ("get_node_at_valid_time", AccessClass::Read),
    ("get_node_at_transaction_time", AccessClass::Read),
    ("get_node_history", AccessClass::Read),
    ("diff_node_versions", AccessClass::Read),
    ("get_edge_at_valid_time", AccessClass::Read),
    ("get_edge_at_transaction_time", AccessClass::Read),
    ("get_edge_history", AccessClass::Read),
    ("diff_edge_versions", AccessClass::Read),
    ("hybrid_query", AccessClass::Read),
    ("query", AccessClass::Read),
    ("get_schema", AccessClass::Read),
    ("temporal_extent", AccessClass::Read),
    ("lineage_upstream", AccessClass::Read),
    ("lineage_downstream", AccessClass::Read),
    ("audit_export", AccessClass::Read),
    ("verify_chain", AccessClass::Read),
    ("export_chain_head", AccessClass::Read),
    // Namespace discovery (Issue #3349, PR3b) — read-only.
    ("list_namespaces", AccessClass::Read),
    ("describe_namespace", AccessClass::Read),
    // ---- Metrics: operational health/stats.
    ("database_stats", AccessClass::Metrics),
    // ---- Write: graph mutations plus index/constraint state changes.
    ("create_node", AccessClass::Write),
    ("update_node", AccessClass::Write),
    ("delete_node", AccessClass::Write),
    ("delete_node_cascade", AccessClass::Write),
    ("retract_node", AccessClass::Write),
    ("create_edge", AccessClass::Write),
    ("update_edge", AccessClass::Write),
    ("delete_edge", AccessClass::Write),
    ("retract_edge", AccessClass::Write),
    ("apply_batch", AccessClass::Write),
    ("enable_vector_index", AccessClass::Write),
    ("enable_unique_constraint", AccessClass::Write),
    // Embedding-backed writes (Issue #2906).
    ("create_node_with_embedding", AccessClass::Write),
    ("update_node_embedding", AccessClass::Write),
    // Namespace creation (Issue #3349, PR3b) — a write.
    ("create_namespace", AccessClass::Write),
    // ---- Admin: none yet. Key lifecycle is served by the HTTP admin surface
    // over the shared persisted store (no Admin-class MCP tools).
];

/// The access class required by an MCP tool, or `None` if the name is not a
/// registered (classified) tool. Derived from [`MCP_TOOL_CLASSES`].
///
/// A `None` here at the dispatch gate MUST be treated as "reject" (not "allow"),
/// so an unregistered/unknown routable name can never bypass the class gate.
#[must_use]
pub fn tool_access_class(name: &str) -> Option<AccessClass> {
    MCP_TOOL_CLASSES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, class)| *class)
}

/// A role-vs-tool authorization denial: the class the tool requires and the
/// role that was refused. Pure data (no transport coupling); the extractor
/// layer maps this into an [`AletheiaHttpError::PermissionDenied`] with the
/// byte-identical legacy message.
///
/// [`AletheiaHttpError::PermissionDenied`]: aletheiadb::http::AletheiaHttpError::PermissionDenied
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Denied {
    /// The access class the tool requires.
    pub required_class: AccessClass,
    /// The role that was refused.
    pub principal_role: Role,
}

/// Authorize a `role` to invoke `tool` by name (the MCP name-based decision).
///
/// Adapted to the autumn surface's **"unknown == reject"** contract (unlike the
/// legacy `SessionAuth::authorize_tool`, which returned `Ok` for unknown names
/// and relied on the string dispatcher to error afterwards): an unregistered
/// routable name has no class, so it can never bypass the class gate. Known
/// tools are `Ok` iff `role.allows(class)`.
///
/// The primary per-request enforcement is carried by the `Authorized<C>`
/// extractor (which reads the class from the handler's own type parameter, so a
/// handler cannot ship class-ungated); this name-based helper is the decision
/// for any dispatcher that pre-checks by tool name.
///
/// # Errors
///
/// Returns [`Denied`] when the role does not permit the tool's class, or when
/// the tool is unknown (rejected as the most-restrictive [`AccessClass::Admin`]).
pub fn authorize_tool(role: Role, tool: &str) -> Result<(), Denied> {
    match tool_access_class(tool) {
        Some(class) if role.allows(class) => Ok(()),
        Some(class) => Err(Denied {
            required_class: class,
            principal_role: role,
        }),
        // Unknown/unregistered routable name: reject (never allow). No concrete
        // class exists, so report the most-restrictive class as the requirement.
        None => Err(Denied {
            required_class: AccessClass::Admin,
            principal_role: role,
        }),
    }
}
