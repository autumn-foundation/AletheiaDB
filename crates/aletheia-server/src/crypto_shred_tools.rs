//! GDPR crypto-shred tools: `designate_subject` and `erase_subject`
//! (Issue #3359, Slice 4b) — the **first Admin-class MCP tools**.
//!
//! Each is a single `#[post(...)]` + `#[api_doc(description=..., mcp)]` handler,
//! so one definition surfaces on **HTTP**, **MCP-over-HTTP** (`/mcp`), and
//! **OpenAPI** at once. The response body is produced by the *exact* main-crate
//! MCP surface (the typed `AletheiaMcpServer` methods `designate_subject` /
//! `erase_subject`), so it is byte-identical to the legacy MCP surface — zero
//! reserialization, reusing only already-public symbols.
//!
//! | Tool | Class | Budgetable (#3353) | Cursorable (#3360) | Forwarding |
//! |------|-------|--------------------|--------------------|------------|
//! | `designate_subject` | [`AdminClass`] | no | no | typed method, inline |
//! | `erase_subject` | [`AdminClass`] | no | no | typed method, inline |
//!
//! Neither is budgetable / cursorable, so both forward through the exact public
//! **typed** per-tool method inline (like `create_namespace` / `count_nodes`) —
//! they are NOT registered in [`crate::edge_tools::ALL_DISPATCH_ROUTED`] because
//! they do not pin a literal tool name through `dispatch_tool_json`. Their class
//! is carried on the handler's own `Authorized<AdminClass>` gate and pinned by
//! `tests/parity/inventory.json` (`registry_matches_inventory_exactly` +
//! `mcp_catalog_equals_inventory_exactly`).
//!
//! The `CryptoShredError` → structured #3234 code mapping (`INVALID_ARGUMENT`,
//! `FAILED_PRECONDITION`, `CONFLICT`) is entirely inside the forwarded
//! main-crate method, so it is preserved verbatim. The erase response carries
//! only the subject id, entity count, timestamp, and signature/pubkey hex —
//! never key material or plaintext.

use crate::security::{AdminClass, Authorized};
use crate::state::ServerState;
use aletheiadb::mcp::{AletheiaMcpServer, DesignateSubjectRequest, EraseSubjectRequest};
use autumn_web::prelude::{Json, post};
use serde_json::Value;

/// Parse an MCP tool method's JSON string result into a [`Value`] for the HTTP
/// response. A non-JSON result (should not happen) degrades to a JSON string.
fn tool_json(s: String) -> Json<Value> {
    Json(serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)))
}

// ════════════════════════════════════════════════════════════════════════════
// designate_subject — [`AdminClass`]. Typed forwarding to the legacy
// `designate_subject` method, inline.
// ════════════════════════════════════════════════════════════════════════════

/// `designate_subject` — designate a GDPR erasure subject over one or more
/// targets (whole nodes/edges and/or specific property keys) (Issue #3359).
/// [`AdminClass`]. HTTP + MCP tool. Requires encryption configured, else
/// `FAILED_PRECONDITION`. Returns `{success, subject_id, targets_designated}`.
#[post("/subjects/designate")]
#[api_doc(
    description = "ADMIN. Designate a GDPR erasure subject over one or more targets (whole nodes/edges and/or specific property keys). Requires encryption configured.",
    mcp
)]
pub async fn designate_subject(
    _auth: Authorized<AdminClass>,
    state: ServerState,
    Json(req): Json<DesignateSubjectRequest>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.designate_subject(req))
}

// ════════════════════════════════════════════════════════════════════════════
// erase_subject — [`AdminClass`]. Typed forwarding to the legacy `erase_subject`
// method, inline.
// ════════════════════════════════════════════════════════════════════════════

/// `erase_subject` — irreversibly erase a designated GDPR subject and return a
/// signed erasure attestation (Issue #3359). [`AdminClass`]. HTTP + MCP tool.
/// Destroys the subject's key material so its sealed payload becomes permanently
/// undecryptable; the response carries only the subject id, entity count,
/// timestamp, and signature/pubkey hex — never key material or plaintext.
/// Erasing an undesignated subject is `FAILED_PRECONDITION`.
#[post("/subjects/erase")]
#[api_doc(
    description = "ADMIN. Irreversibly erase a designated GDPR subject: destroy its key material so its sealed payload becomes permanently undecryptable, and return a signed erasure attestation (never any property content or key material).",
    mcp
)]
pub async fn erase_subject(
    _auth: Authorized<AdminClass>,
    state: ServerState,
    Json(req): Json<EraseSubjectRequest>,
) -> Json<Value> {
    let server = AletheiaMcpServer::new(state.db_arc());
    tool_json(server.erase_subject(req))
}
