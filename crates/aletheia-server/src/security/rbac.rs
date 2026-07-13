// OWNED BY LANE B (security). Scaffold only — do not implement here.
//
//! Per-MCP-tool access-class table (stub).
//!
//! Maps an MCP tool name to the [`AccessClass`] it requires. PR1 provides a
//! small self-contained map covering the node-read proof slice; Lane B owns the
//! full table **and** its enforcement.
//!
//! Lane B: anchor this on tests/parity/inventory.json (do not widen legacy
//! TOOL_ACCESS_CLASSES) — the inventory's `mcp.tools[].access_class` is the
//! shared source of truth, checked bidirectionally + for totality against the
//! live registry, so this table must be derived from / verified against
//! inventory.json rather than the main crate's `pub(crate)` constant.

use aletheiadb::auth::AccessClass;

/// The access class required by an MCP tool, or `None` if unknown to this stub.
///
/// PR1 scope: the three node-read tools surfaced on `/mcp`. Lane B replaces this
/// with the full inventory-anchored table and wires enforcement into the
/// dispatch/replay path.
#[must_use]
pub fn tool_access_class(name: &str) -> Option<AccessClass> {
    match name {
        // Node-read proof slice (Issue #3524 PR1).
        "get_node" | "list_nodes" | "count_nodes" => Some(AccessClass::Read),
        // TODO(Lane B): complete this from tests/parity/inventory.json.
        _ => None,
    }
}
