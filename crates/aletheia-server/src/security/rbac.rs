// OWNED BY LANE B (security). Scaffold only — do not implement here.
//
//! Per-MCP-tool access-class table (bypass-proof-by-construction stub).
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
//! Lane B: anchor this registry on tests/parity/inventory.json (do not widen
//! legacy TOOL_ACCESS_CLASSES) — the inventory's `mcp.tools[].access_class` is
//! the shared source of truth, checked bidirectionally + for totality against
//! the live registry, so this table must be derived from / verified against
//! inventory.json rather than the main crate's `pub(crate)` constant. Extend the
//! list below as slices 2–8 add handlers; the conformance test keeps it honest.

use aletheiadb::auth::AccessClass;

/// The single source of truth pairing every MCP-exposed handler name with the
/// [`AccessClass`] it requires. Both [`tool_access_class`] and the PR1
/// conformance test derive from THIS list, so the routable-name set and the
/// class-table set are one list by construction (they cannot silently drift
/// into two independently-maintained lists).
///
/// PR1 scope: the three node-read tools surfaced on `/mcp`.
pub const MCP_TOOL_CLASSES: &[(&str, AccessClass)] = &[
    ("get_node", AccessClass::Read),
    ("list_nodes", AccessClass::Read),
    ("count_nodes", AccessClass::Read),
    // TODO(Lane B): extend from tests/parity/inventory.json as slices 2–8 land;
    // the conformance test asserts this stays == the live routable set.
];

/// The access class required by an MCP tool, or `None` if the name is not a
/// registered (routable + classified) tool. Derived from [`MCP_TOOL_CLASSES`].
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
