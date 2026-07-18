//! Conformance and enforcement tests for MCP-surface authentication and
//! role-based authorization (Issue #3350, Phase 2).
//!
//! Structure:
//!
//! 1. **Inventory conformance** — the full advertised tool inventory (the
//!    same source of truth `tools/list` uses) is enumerated and every tool
//!    must have a classification; the classification table must not carry
//!    stale entries either. The documented matrix in
//!    `docs/guides/access-control-matrix.md` is mechanically compared
//!    against the code.
//! 2. **Role-behavior matrix** — every role × every tool: denials must be
//!    `PERMISSION_DENIED` (with `details.required_class` /
//!    `details.principal_role`); allowed combinations may fail for other
//!    reasons (e.g. `INVALID_ARGUMENT` on null args) but never with an
//!    auth error.
//! 3. **Authentication** — `Required` mode with a missing, invalid, or
//!    revoked session credential yields a uniform `UNAUTHENTICATED` error
//!    for every tool, including unknown tool names (no inventory leak),
//!    never echoing the credential. Revocation is re-checked per call.
//! 4. **Anonymous compatibility** — `AuthMode::Anonymous` (and the
//!    source-compatible `AletheiaMcpServer::new`) preserve full access.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use super::auth::{TOOL_ACCESS_CLASSES, tool_access_class};
use super::{AletheiaMcpServer, McpAuthConfig};
use crate::auth::{AccessClass, AuthMode, AuthStore, Role, SecretString};
use crate::db::AletheiaDB;

// ============================================================================
// Helpers
// ============================================================================

fn db() -> Arc<AletheiaDB> {
    Arc::new(AletheiaDB::new().expect("db init"))
}

/// A `Required`-mode server whose session credential resolves to a principal
/// with the given role. Returns the shared store handle and the principal id
/// so tests can revoke mid-session.
fn server_with_role(role: Role) -> (AletheiaMcpServer, Arc<AuthStore>, String) {
    let store = Arc::new(AuthStore::new());
    let (principal, key) = store
        .create_key(&format!("test-{role}"), role)
        .expect("create key");
    let server = AletheiaMcpServer::with_auth(
        db(),
        McpAuthConfig::new(AuthMode::Required, Arc::clone(&store))
            .with_credential(SecretString::new(key.as_str())),
    );
    (server, store, principal.id)
}

/// Dispatch through the same table `call_tool` uses; parse the JSON payload.
fn dispatch(
    server: &AletheiaMcpServer,
    tool: &str,
    args: serde_json::Value,
) -> (serde_json::Value, bool) {
    let result = server.dispatch_tool(tool, args);
    let is_error = result.is_error.unwrap_or(false);
    let text = result
        .content
        .first()
        .and_then(|c| c.as_text().map(|t| t.text.clone()))
        .expect("tool result should carry text content");
    let value = serde_json::from_str(&text).expect("tool response should be valid JSON");
    (value, is_error)
}

fn error_code(value: &serde_json::Value) -> Option<&str> {
    value.get("error")?.get("code")?.as_str()
}

const AUTH_ERROR_CODES: [&str; 2] = ["UNAUTHENTICATED", "PERMISSION_DENIED"];

// ============================================================================
// 1. Inventory conformance
// ============================================================================

/// AC3: every advertised tool (the `tools/list` source of truth) must be
/// classified into an `AccessClass`. A new tool added to `tool_definitions`
/// without a classification fails here.
#[test]
fn every_advertised_tool_has_a_classification() {
    let server = AletheiaMcpServer::new(db());
    let advertised = server.list_tools_for_test();
    assert!(!advertised.is_empty(), "tool inventory must not be empty");
    for tool in &advertised {
        assert!(
            tool_access_class(tool).is_some(),
            "advertised tool '{tool}' has no AccessClass classification — \
             add it to TOOL_ACCESS_CLASSES in src/mcp/auth.rs (and to \
             docs/guides/access-control-matrix.md)"
        );
    }
}

/// The classification table must not drift the other way either: an entry
/// for a tool that is no longer advertised is stale (a rename would
/// otherwise silently leave the new name unclassified-but-covered).
#[test]
fn classification_table_has_no_stale_entries() {
    let server = AletheiaMcpServer::new(db());
    let advertised = server.list_tools_for_test();
    for (tool, _) in TOOL_ACCESS_CLASSES {
        assert!(
            advertised.iter().any(|t| t == tool),
            "TOOL_ACCESS_CLASSES entry '{tool}' is not an advertised tool — remove or rename it"
        );
    }
    assert_eq!(
        advertised.len(),
        TOOL_ACCESS_CLASSES.len(),
        "advertised inventory and classification table must be the same size"
    );
}

/// The classification table is a map: duplicate names would make the
/// effective class order-dependent.
#[test]
fn classification_table_has_no_duplicates() {
    let mut seen = std::collections::BTreeSet::new();
    for (tool, _) in TOOL_ACCESS_CLASSES {
        assert!(seen.insert(*tool), "duplicate classification for '{tool}'");
    }
}

/// The documented matrix (docs/guides/access-control-matrix.md) must match
/// the code, both directions: every classified tool documented with the
/// right class, and no documented tool missing from the code.
#[test]
fn documented_matrix_matches_code() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/guides/access-control-matrix.md"
    );
    let doc = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read documented matrix at {path}: {e}"));
    let start = doc
        .find("<!-- mcp-tool-matrix:start -->")
        .expect("doc must carry the mcp-tool-matrix:start marker");
    let end = doc
        .find("<!-- mcp-tool-matrix:end -->")
        .expect("doc must carry the mcp-tool-matrix:end marker");
    let mut documented: BTreeMap<String, String> = BTreeMap::new();
    for line in doc[start..end].lines() {
        let line = line.trim();
        // Table rows look like: | `get_node` | read |
        if !line.starts_with("| `") {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        assert!(cells.len() >= 2, "malformed matrix row: {line}");
        documented.insert(cells[0].trim_matches('`').to_string(), cells[1].to_string());
    }

    for (tool, class) in TOOL_ACCESS_CLASSES {
        assert_eq!(
            documented.get(*tool).map(String::as_str),
            Some(class.to_string().as_str()),
            "documented matrix out of sync for tool '{tool}' (expected class '{class}') — \
             update docs/guides/access-control-matrix.md"
        );
    }
    assert_eq!(
        documented.len(),
        TOOL_ACCESS_CLASSES.len(),
        "documented matrix lists tools missing from the code table: {:?}",
        documented
            .keys()
            .filter(|k| tool_access_class(k).is_none())
            .collect::<Vec<_>>()
    );
}

// ============================================================================
// 2. Role-behavior matrix
// ============================================================================

/// Every role × every advertised tool: allow/deny must match
/// `Role::allows(class)` exactly. Denials are `PERMISSION_DENIED` with
/// structured details; allowed calls may fail later for other reasons but
/// never with an auth error code.
#[test]
fn role_behavior_matrix_over_full_inventory() {
    for role in [Role::Admin, Role::Writer, Role::Reader, Role::Metrics] {
        let (server, _store, _id) = server_with_role(role);
        for tool in server.list_tools_for_test() {
            let class =
                tool_access_class(&tool).unwrap_or_else(|| panic!("tool '{tool}' unclassified"));
            let (value, is_error) = dispatch(&server, &tool, serde_json::Value::Null);
            if role.allows(class) {
                if is_error {
                    let code = error_code(&value).unwrap_or("<none>");
                    assert!(
                        !AUTH_ERROR_CODES.contains(&code),
                        "[{role} × {tool}] allowed combo must not fail with an auth \
                         error, got: {value}"
                    );
                }
            } else {
                assert!(
                    is_error,
                    "[{role} × {tool}] must be denied, got success: {value}"
                );
                assert_eq!(
                    error_code(&value),
                    Some("PERMISSION_DENIED"),
                    "[{role} × {tool}] denial must be PERMISSION_DENIED: {value}"
                );
                assert_eq!(
                    value["error"]["retriable"], false,
                    "[{role} × {tool}] PERMISSION_DENIED must not be retriable"
                );
                assert_eq!(
                    value["error"]["details"]["required_class"],
                    json!(class.to_string()),
                    "[{role} × {tool}] details must carry required_class: {value}"
                );
                assert_eq!(
                    value["error"]["details"]["principal_role"],
                    json!(role.to_string()),
                    "[{role} × {tool}] details must carry principal_role: {value}"
                );
            }
        }
    }
}

/// Reader: writes denied (with details), reads work.
#[test]
fn reader_denied_on_writes_allowed_on_reads() {
    let (server, _store, _id) = server_with_role(Role::Reader);
    for tool in ["create_node", "delete_node", "retract_edge"] {
        let (value, is_error) = dispatch(&server, tool, serde_json::Value::Null);
        assert!(is_error, "[{tool}] reader must be denied");
        assert_eq!(error_code(&value), Some("PERMISSION_DENIED"), "{value}");
        assert_eq!(value["error"]["details"]["required_class"], json!("write"));
    }
    // Denial happens BEFORE argument parsing/execution: null args would be
    // INVALID_ARGUMENT if the tool body ran.
    let (value, _) = dispatch(&server, "create_node", serde_json::Value::Null);
    assert_eq!(error_code(&value), Some("PERMISSION_DENIED"), "{value}");

    // Reads pass authorization (count_nodes succeeds outright; get_node with
    // empty args fails, but not with an auth error).
    let (value, is_error) = dispatch(&server, "count_nodes", json!({}));
    assert!(!is_error, "reader count_nodes must succeed: {value}");
    for tool in ["get_node", "query", "traverse"] {
        let (value, _) = dispatch(&server, tool, serde_json::Value::Null);
        if let Some(code) = error_code(&value) {
            assert!(
                !AUTH_ERROR_CODES.contains(&code),
                "[{tool}] reader read must not hit an auth error: {value}"
            );
        }
    }
}

/// Metrics: database_stats allowed, reads and writes denied.
#[test]
fn metrics_role_stats_only() {
    let (server, _store, _id) = server_with_role(Role::Metrics);
    let (value, is_error) = dispatch(&server, "database_stats", json!({}));
    assert!(!is_error, "metrics database_stats must succeed: {value}");

    for tool in ["get_node", "create_node"] {
        let (value, is_error) = dispatch(&server, tool, serde_json::Value::Null);
        assert!(is_error, "[{tool}] metrics role must be denied");
        assert_eq!(error_code(&value), Some("PERMISSION_DENIED"), "{value}");
    }
}

/// Writer: writes allowed end-to-end.
#[test]
fn writer_can_write() {
    let (server, _store, _id) = server_with_role(Role::Writer);
    let (value, is_error) = dispatch(
        &server,
        "create_node",
        json!({"label": "Person", "properties": {"name": "Alice"}}),
    );
    assert!(!is_error, "writer create_node must succeed: {value}");
    assert_eq!(value["label"], json!("Person"));
}

/// Admin: everything allowed (spot check on a write; the full sweep is in
/// the role matrix test).
#[test]
fn admin_can_do_everything_spot_check() {
    let (server, _store, _id) = server_with_role(Role::Admin);
    let (value, is_error) = dispatch(&server, "create_node", json!({"label": "T"}));
    assert!(!is_error, "admin create_node must succeed: {value}");
    let (value, is_error) = dispatch(&server, "database_stats", json!({}));
    assert!(!is_error, "admin database_stats must succeed: {value}");
}

// ============================================================================
// 3. Authentication (UNAUTHENTICATED)
// ============================================================================

/// Required mode + no credential: EVERY tool returns UNAUTHENTICATED with a
/// single uniform message and no details.
#[test]
fn required_without_credential_every_tool_unauthenticated() {
    let store = Arc::new(AuthStore::new());
    // The store is NOT empty (startup validation is a separate concern) —
    // there is simply no session credential.
    store
        .create_key("someone-else", Role::Admin)
        .expect("create key");
    let server = AletheiaMcpServer::with_auth(db(), McpAuthConfig::new(AuthMode::Required, store));

    let mut messages = std::collections::BTreeSet::new();
    for tool in server.list_tools_for_test() {
        let (value, is_error) = dispatch(&server, &tool, serde_json::Value::Null);
        assert!(is_error, "[{tool}] must be an error without a credential");
        assert_eq!(
            error_code(&value),
            Some("UNAUTHENTICATED"),
            "[{tool}] got: {value}"
        );
        assert_eq!(value["error"]["retriable"], false, "[{tool}] {value}");
        assert!(
            value["error"].get("details").is_none(),
            "[{tool}] UNAUTHENTICATED must carry no details: {value}"
        );
        messages.insert(value["error"]["message"].as_str().unwrap_or("").to_string());
    }
    assert_eq!(
        messages.len(),
        1,
        "UNAUTHENTICATED message must be uniform across all tools: {messages:?}"
    );
    assert_eq!(
        messages.into_iter().next().as_deref(),
        Some("authentication required")
    );
}

/// Required mode + invalid credential: uniform UNAUTHENTICATED, and the
/// presented credential is never echoed anywhere in the response.
#[test]
fn required_with_invalid_credential_uniform_and_no_echo() {
    let bogus = "aletheia_sk_definitely_not_a_real_key_xyz";
    let store = Arc::new(AuthStore::new());
    store.create_key("real", Role::Admin).expect("create key");
    let server = AletheiaMcpServer::with_auth(
        db(),
        McpAuthConfig::new(AuthMode::Required, store).with_credential(SecretString::new(bogus)),
    );

    for tool in ["get_node", "create_node", "database_stats", "query"] {
        let result = server.dispatch_tool(tool, serde_json::Value::Null);
        let text = result
            .content
            .first()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .expect("text content");
        assert!(
            !text.contains(bogus) && !text.contains("definitely_not_a_real_key"),
            "[{tool}] response must never echo the presented credential: {text}"
        );
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(error_code(&value), Some("UNAUTHENTICATED"), "{value}");
        assert_eq!(
            value["error"]["message"],
            json!("authentication required"),
            "message must not reveal whether the key exists: {value}"
        );
    }
}

/// An unknown tool name must not bypass authentication: without a valid
/// credential the response is UNAUTHENTICATED, not the unknown-tool error
/// (no tool-inventory leak to unauthenticated callers). Once authenticated,
/// the normal unknown-tool NOT_FOUND is returned.
#[test]
fn unknown_tool_requires_authentication_first() {
    let store = Arc::new(AuthStore::new());
    store.create_key("real", Role::Admin).expect("create key");
    let server = AletheiaMcpServer::with_auth(
        db(),
        McpAuthConfig::new(AuthMode::Required, Arc::clone(&store)),
    );
    let (value, is_error) = dispatch(&server, "definitely_not_a_tool", serde_json::Value::Null);
    assert!(is_error);
    assert_eq!(
        error_code(&value),
        Some("UNAUTHENTICATED"),
        "unknown tool without credential must be UNAUTHENTICATED, not unknown-tool: {value}"
    );
    assert!(
        !value["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("Unknown tool"),
        "must not leak tool-name resolution to unauthenticated callers: {value}"
    );

    // Authenticated: the normal unknown-tool NOT_FOUND applies.
    let (server, _store, _id) = server_with_role(Role::Reader);
    let (value, is_error) = dispatch(&server, "definitely_not_a_tool", serde_json::Value::Null);
    assert!(is_error);
    assert_eq!(error_code(&value), Some("NOT_FOUND"), "{value}");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("Unknown tool"),
        "{value}"
    );
}

/// Revoking the session principal's key in the shared store takes effect on
/// the very next tool call (the credential is re-verified per call).
#[test]
fn revocation_takes_effect_on_next_call() {
    let (server, store, principal_id) = server_with_role(Role::Reader);

    let (value, is_error) = dispatch(&server, "count_nodes", json!({}));
    assert!(!is_error, "pre-revocation call must succeed: {value}");
    assert!(server.session_principal().is_some());

    assert!(store.revoke(&principal_id).expect("revoke"), "key existed");

    let (value, is_error) = dispatch(&server, "count_nodes", json!({}));
    assert!(is_error, "post-revocation call must fail: {value}");
    assert_eq!(error_code(&value), Some("UNAUTHENTICATED"), "{value}");
    assert!(server.session_principal().is_none());
}

// ============================================================================
// 4. Anonymous compatibility
// ============================================================================

/// Explicit `AuthMode::Anonymous` preserves full access for every tool.
#[test]
fn anonymous_mode_preserves_full_access() {
    let server = AletheiaMcpServer::with_auth(
        db(),
        McpAuthConfig::new(AuthMode::Anonymous, Arc::new(AuthStore::new())),
    );
    let (value, is_error) = dispatch(&server, "create_node", json!({"label": "T"}));
    assert!(!is_error, "anonymous create_node must succeed: {value}");
    let (value, is_error) = dispatch(&server, "database_stats", json!({}));
    assert!(!is_error, "anonymous database_stats must succeed: {value}");
    assert!(server.session_principal().is_none());
}

/// `AletheiaMcpServer::new` (the embedded/programmatic constructor) is
/// source-compatible: anonymous, full access, no auth errors anywhere.
#[test]
fn new_constructor_defaults_to_anonymous() {
    let server = AletheiaMcpServer::new(db());
    for tool in server.list_tools_for_test() {
        let (value, is_error) = dispatch(&server, &tool, serde_json::Value::Null);
        if is_error {
            let code = error_code(&value).unwrap_or("<none>");
            assert!(
                !AUTH_ERROR_CODES.contains(&code),
                "[{tool}] new(db) must never produce auth errors: {value}"
            );
        }
    }
}

// ============================================================================
// Startup validation
// ============================================================================

#[test]
fn validate_mcp_auth_startup_rejects_required_with_no_keys() {
    let store = AuthStore::new();
    let err = super::auth::validate_mcp_auth_startup(AuthMode::Required, &store)
        .expect_err("empty store + required mode must refuse startup");
    assert!(err.contains("ALETHEIADB_BOOTSTRAP_ADMIN_KEY"));
    assert!(err.contains("anonymous"));
}

#[test]
fn validate_mcp_auth_startup_accepts_required_with_a_key_and_anonymous() {
    let store = AuthStore::new();
    store.create_key("k", Role::Admin).expect("create key");
    assert!(super::auth::validate_mcp_auth_startup(AuthMode::Required, &store).is_ok());
    let empty = AuthStore::new();
    assert!(super::auth::validate_mcp_auth_startup(AuthMode::Anonymous, &empty).is_ok());
}

// ============================================================================
// 5. Provenance principal stamping (AC4, Issue #3350 Phase 3)
// ============================================================================

/// AC4: a write through an authenticated MCP session stamps the verified
/// principal's name into that version's provenance, COMPOSING with the
/// caller-supplied `source`/`confidence` (never replacing them), and the
/// stamp is answerable from history (visible on read responses and history
/// entries, not just an in-memory side effect).
#[test]
fn authenticated_write_stamps_principal_composing_with_source() {
    let (server, _store, _id) = server_with_role(Role::Writer);

    let (created, is_error) = dispatch(
        &server,
        "create_node",
        json!({
            "label": "Person",
            "properties": {"name": "Alice"},
            "provenance": {"source": "hr-system", "confidence": 0.9}
        }),
    );
    assert!(!is_error, "writer create_node must succeed: {created}");
    let node_id = created["id"].as_u64().expect("created node id");

    // Read back: provenance carries BOTH the caller's source semantics and
    // the server-stamped principal.
    let (node, is_error) = dispatch(&server, "get_node", json!({"node_id": node_id}));
    assert!(!is_error, "get_node must succeed: {node}");
    let provenance = &node["provenance"];
    assert_eq!(provenance["source"], json!("hr-system"), "{node}");
    assert_eq!(provenance["confidence"], json!(0.9), "{node}");
    assert_eq!(provenance["principal"], json!("test-writer"), "{node}");

    // Update creates a second version; its provenance is stamped too.
    let (updated, is_error) = dispatch(
        &server,
        "update_node",
        json!({
            "node_id": node_id,
            "properties": {"name": "Alice Smith"},
            "provenance": {"source": "manual-correction"}
        }),
    );
    assert!(!is_error, "update_node must succeed: {updated}");

    // "Who wrote this fact" is answerable from HISTORY: every version's
    // provenance bundle carries the principal.
    let (history, is_error) = dispatch(&server, "get_node_history", json!({"node_id": node_id}));
    assert!(!is_error, "get_node_history must succeed: {history}");
    let versions = history["versions"].as_array().expect("history versions");
    assert!(versions.len() >= 2, "expected >= 2 versions: {history}");
    for version in versions {
        assert_eq!(
            version["provenance"]["principal"],
            json!("test-writer"),
            "every historical version must carry the stamping principal: {version}"
        );
    }
    // The caller-supplied sources are preserved per version, proving the
    // stamp composed rather than replaced.
    let sources: Vec<&str> = versions
        .iter()
        .filter_map(|v| v["provenance"]["source"].as_str())
        .collect();
    assert!(sources.contains(&"hr-system"), "{history}");
    assert!(sources.contains(&"manual-correction"), "{history}");
}

/// Provenance forging: `ProvenanceRequest` has no `principal` field, so an
/// extra `"principal"` key in the caller's JSON is ignored by serde — the
/// stored principal is server-derived only (session identity or absent),
/// never the forged value.
#[test]
fn forged_principal_key_in_provenance_request_is_ignored() {
    // Anonymous session: forged key must not surface at all.
    let anon = AletheiaMcpServer::new(db());
    let (created, is_error) = dispatch(
        &anon,
        "create_node",
        json!({
            "label": "Doc",
            "provenance": {"source": "import", "principal": "forged-admin"}
        }),
    );
    assert!(!is_error, "{created}");
    let node_id = created["id"].as_u64().expect("node id");
    let (node, _) = dispatch(&anon, "get_node", json!({"node_id": node_id}));
    assert_eq!(node["provenance"]["source"], json!("import"), "{node}");
    assert_eq!(
        node["provenance"].get("principal"),
        None,
        "forged principal must not be stored: {node}"
    );

    // Authenticated session: stored principal is the SESSION's, not the
    // forged value.
    let (server, _store, _id) = server_with_role(Role::Writer);
    let (created, is_error) = dispatch(
        &server,
        "create_node",
        json!({
            "label": "Doc",
            "provenance": {"source": "import", "principal": "forged-admin"}
        }),
    );
    assert!(!is_error, "{created}");
    let node_id = created["id"].as_u64().expect("node id");
    let (node, _) = dispatch(&server, "get_node", json!({"node_id": node_id}));
    assert_eq!(
        node["provenance"]["principal"],
        json!("test-writer"),
        "principal must come from the verified session, never the caller: {node}"
    );
}

/// A write with NO caller-supplied provenance still records the principal
/// (a principal-only bundle) when the session is authenticated.
#[test]
fn authenticated_write_without_provenance_records_principal_only() {
    let (server, _store, _id) = server_with_role(Role::Writer);
    let (created, is_error) = dispatch(&server, "create_node", json!({"label": "Doc"}));
    assert!(!is_error, "{created}");
    let node_id = created["id"].as_u64().expect("id");

    let (node, is_error) = dispatch(&server, "get_node", json!({"node_id": node_id}));
    assert!(!is_error, "{node}");
    assert_eq!(
        node["provenance"]["principal"],
        json!("test-writer"),
        "{node}"
    );
    assert!(
        node["provenance"].get("source").is_none(),
        "no fabricated source: {node}"
    );
}

/// Anonymous-mode writes record NO principal: the field is absent (not an
/// empty string), and caller-supplied provenance is preserved verbatim.
#[test]
fn anonymous_write_records_no_principal() {
    let server = AletheiaMcpServer::new(db());
    let (created, is_error) = dispatch(
        &server,
        "create_node",
        json!({
            "label": "Person",
            "provenance": {"source": "csv-import"}
        }),
    );
    assert!(!is_error, "{created}");
    let node_id = created["id"].as_u64().expect("id");

    let (node, is_error) = dispatch(&server, "get_node", json!({"node_id": node_id}));
    assert!(!is_error, "{node}");
    assert_eq!(node["provenance"]["source"], json!("csv-import"), "{node}");
    assert!(
        node["provenance"].get("principal").is_none(),
        "anonymous write must not carry a principal field: {node}"
    );

    // And with no provenance at all, none is fabricated.
    let (created, is_error) = dispatch(&server, "create_node", json!({"label": "Doc"}));
    assert!(!is_error, "{created}");
    let node_id = created["id"].as_u64().expect("id");
    let (node, is_error) = dispatch(&server, "get_node", json!({"node_id": node_id}));
    assert!(!is_error, "{node}");
    assert!(
        node.get("provenance").is_none(),
        "anonymous write without provenance must record none: {node}"
    );
}

/// Edge writes are stamped the same way as node writes.
#[test]
fn authenticated_edge_write_stamps_principal() {
    let (server, _store, _id) = server_with_role(Role::Writer);
    let (a, _) = dispatch(&server, "create_node", json!({"label": "Person"}));
    let (b, _) = dispatch(&server, "create_node", json!({"label": "Person"}));
    let (edge, is_error) = dispatch(
        &server,
        "create_edge",
        json!({
            "source_id": a["id"], "target_id": b["id"], "label": "KNOWS",
            "provenance": {"source": "social-import"}
        }),
    );
    assert!(!is_error, "{edge}");
    let edge_id = edge["id"].as_u64().expect("edge id");

    let (read, is_error) = dispatch(&server, "get_edge", json!({"edge_id": edge_id}));
    assert!(!is_error, "{read}");
    assert_eq!(
        read["provenance"]["source"],
        json!("social-import"),
        "{read}"
    );
    assert_eq!(
        read["provenance"]["principal"],
        json!("test-writer"),
        "{read}"
    );
}

/// `apply_batch` (Issue #3231) create/update ops route through the same
/// `parse_opt_provenance` helper as the single-op tools, so an authenticated
/// batch stamps the session principal on every created/updated version --
/// composing with (never replacing) a caller-supplied `source`, and
/// recording a principal-only bundle when the op carries no provenance.
#[test]
fn authenticated_apply_batch_stamps_principal_on_creates_and_updates() {
    let (server, _store, _id) = server_with_role(Role::Writer);

    let (batch, is_error) = dispatch(
        &server,
        "apply_batch",
        json!({"operations": [
            {"op": "create_node", "label": "Person", "ref": "a",
             "properties": {"name": "Alice"},
             "provenance": {"source": "batch-import"}},
            {"op": "create_node", "label": "Person", "ref": "b",
             "properties": {"name": "Bob"}},
            {"op": "create_edge", "source_id": "$a", "target_id": "$b",
             "label": "KNOWS", "provenance": {"source": "batch-import"}},
        ]}),
    );
    assert!(!is_error, "{batch}");
    let a_id = batch["ref_map"]["a"].as_u64().expect("a id");
    let b_id = batch["ref_map"]["b"].as_u64().expect("b id");
    let edge_id = batch["results"][2]["edge_id"].as_u64().expect("edge id");

    // Caller source composes with the stamped principal.
    let (a, _) = dispatch(&server, "get_node", json!({"node_id": a_id}));
    assert_eq!(a["provenance"]["source"], json!("batch-import"), "{a}");
    assert_eq!(a["provenance"]["principal"], json!("test-writer"), "{a}");

    // No caller provenance still records a principal-only bundle.
    let (b, _) = dispatch(&server, "get_node", json!({"node_id": b_id}));
    assert_eq!(b["provenance"]["principal"], json!("test-writer"), "{b}");
    assert!(b["provenance"].get("source").is_none(), "{b}");

    // Batch edge creates are stamped like node creates.
    let (edge, _) = dispatch(&server, "get_edge", json!({"edge_id": edge_id}));
    assert_eq!(
        edge["provenance"]["source"],
        json!("batch-import"),
        "{edge}"
    );
    assert_eq!(
        edge["provenance"]["principal"],
        json!("test-writer"),
        "{edge}"
    );

    // Batch update ops stamp the new version too.
    let (batch, is_error) = dispatch(
        &server,
        "apply_batch",
        json!({"operations": [
            {"op": "update_node", "node_id": a_id,
             "properties": {"name": "Alice II"},
             "provenance": {"source": "batch-correction"}},
            {"op": "update_edge", "edge_id": edge_id,
             "properties": {"since": 2024}},
        ]}),
    );
    assert!(!is_error, "{batch}");
    let (a, _) = dispatch(&server, "get_node", json!({"node_id": a_id}));
    assert_eq!(a["provenance"]["source"], json!("batch-correction"), "{a}");
    assert_eq!(a["provenance"]["principal"], json!("test-writer"), "{a}");
    let (edge, _) = dispatch(&server, "get_edge", json!({"edge_id": edge_id}));
    assert_eq!(
        edge["provenance"]["principal"],
        json!("test-writer"),
        "{edge}"
    );
}

// ============================================================================
// 5b. Destructive-op principal stamping (Issue #3427, Phase C route-through)
//
// The four destructive tools (delete_node, delete_edge, retract_node,
// retract_edge) stamp the AUTHENTICATED SESSION principal onto the
// tombstone / retraction version's provenance -- server-derived and
// unforgeable, matching the create/update convention. A deleted/retracted
// node is gone from current state, so the principal is asserted end-to-end
// through the read path (get_node_history / get_edge_history), on the last
// (tombstone/retraction) version.
// ============================================================================

/// The last version in a node history response (the tombstone / retraction
/// version for a destructive op).
fn last_node_version(server: &AletheiaMcpServer, node_id: u64) -> serde_json::Value {
    let (history, is_error) = dispatch(server, "get_node_history", json!({ "node_id": node_id }));
    assert!(!is_error, "get_node_history must succeed: {history}");
    let versions = history["versions"].as_array().expect("history versions");
    versions.last().expect("at least one version").clone()
}

/// The last version in an edge history response.
fn last_edge_version(server: &AletheiaMcpServer, edge_id: u64) -> serde_json::Value {
    let (history, is_error) = dispatch(server, "get_edge_history", json!({ "edge_id": edge_id }));
    assert!(!is_error, "get_edge_history must succeed: {history}");
    let versions = history["versions"].as_array().expect("history versions");
    versions.last().expect("at least one version").clone()
}

/// Authenticated `delete_node` stamps the session principal onto the
/// tombstone version's provenance.
#[test]
fn authenticated_delete_node_stamps_principal() {
    let (server, _store, _id) = server_with_role(Role::Writer);
    let (created, _) = dispatch(&server, "create_node", json!({"label": "Doc"}));
    let node_id = created["id"].as_u64().expect("node id");

    let (deleted, is_error) = dispatch(&server, "delete_node", json!({"node_id": node_id}));
    assert!(!is_error, "delete_node must succeed: {deleted}");

    let tombstone = last_node_version(&server, node_id);
    assert_eq!(
        tombstone["provenance"]["principal"],
        json!("test-writer"),
        "delete tombstone must carry the session principal: {tombstone}"
    );
}

/// Authenticated `delete_edge` stamps the session principal onto the
/// tombstone version's provenance.
#[test]
fn authenticated_delete_edge_stamps_principal() {
    let (server, _store, _id) = server_with_role(Role::Writer);
    let (a, _) = dispatch(&server, "create_node", json!({"label": "Person"}));
    let (b, _) = dispatch(&server, "create_node", json!({"label": "Person"}));
    let (edge, _) = dispatch(
        &server,
        "create_edge",
        json!({"source_id": a["id"], "target_id": b["id"], "label": "KNOWS"}),
    );
    let edge_id = edge["id"].as_u64().expect("edge id");

    let (deleted, is_error) = dispatch(&server, "delete_edge", json!({"edge_id": edge_id}));
    assert!(!is_error, "delete_edge must succeed: {deleted}");

    let tombstone = last_edge_version(&server, edge_id);
    assert_eq!(
        tombstone["provenance"]["principal"],
        json!("test-writer"),
        "delete-edge tombstone must carry the session principal: {tombstone}"
    );
}

/// Authenticated `retract_node` stamps the session principal onto the
/// retraction version's provenance.
#[test]
fn authenticated_retract_node_stamps_principal() {
    let (server, _store, _id) = server_with_role(Role::Writer);
    let (created, _) = dispatch(&server, "create_node", json!({"label": "Doc"}));
    let node_id = created["id"].as_u64().expect("node id");

    let (retracted, is_error) = dispatch(&server, "retract_node", json!({"node_id": node_id}));
    assert!(!is_error, "retract_node must succeed: {retracted}");

    let retraction = last_node_version(&server, node_id);
    assert_eq!(
        retraction["provenance"]["principal"],
        json!("test-writer"),
        "retraction version must carry the session principal: {retraction}"
    );
}

/// Authenticated `retract_edge` stamps the session principal onto the
/// retraction version's provenance.
#[test]
fn authenticated_retract_edge_stamps_principal() {
    let (server, _store, _id) = server_with_role(Role::Writer);
    let (a, _) = dispatch(&server, "create_node", json!({"label": "Person"}));
    let (b, _) = dispatch(&server, "create_node", json!({"label": "Person"}));
    let (edge, _) = dispatch(
        &server,
        "create_edge",
        json!({"source_id": a["id"], "target_id": b["id"], "label": "KNOWS"}),
    );
    let edge_id = edge["id"].as_u64().expect("edge id");

    let (retracted, is_error) = dispatch(&server, "retract_edge", json!({"edge_id": edge_id}));
    assert!(!is_error, "retract_edge must succeed: {retracted}");

    let retraction = last_edge_version(&server, edge_id);
    assert_eq!(
        retraction["provenance"]["principal"],
        json!("test-writer"),
        "edge retraction version must carry the session principal: {retraction}"
    );
}

/// A detach (cascade) `delete_node` attributes EVERY tombstone it creates --
/// the node AND every co-deleted edge -- with the session principal.
#[test]
fn authenticated_detach_delete_stamps_principal_on_co_deleted_edges() {
    let (server, _store, _id) = server_with_role(Role::Writer);
    let (a, _) = dispatch(&server, "create_node", json!({"label": "Person"}));
    let (b, _) = dispatch(&server, "create_node", json!({"label": "Person"}));
    let a_id = a["id"].as_u64().expect("a id");
    let (edge, _) = dispatch(
        &server,
        "create_edge",
        json!({"source_id": a["id"], "target_id": b["id"], "label": "KNOWS"}),
    );
    let edge_id = edge["id"].as_u64().expect("edge id");

    let (deleted, is_error) = dispatch(
        &server,
        "delete_node",
        json!({"node_id": a_id, "detach": true}),
    );
    assert!(!is_error, "detach delete_node must succeed: {deleted}");
    assert_eq!(deleted["edges_removed"], json!(1), "{deleted}");

    // The node's tombstone carries the principal.
    let node_tombstone = last_node_version(&server, a_id);
    assert_eq!(
        node_tombstone["provenance"]["principal"],
        json!("test-writer"),
        "node tombstone must carry the principal: {node_tombstone}"
    );
    // AND the co-deleted edge's tombstone carries the SAME principal.
    let edge_tombstone = last_edge_version(&server, edge_id);
    assert_eq!(
        edge_tombstone["provenance"]["principal"],
        json!("test-writer"),
        "co-deleted edge tombstone must carry the principal: {edge_tombstone}"
    );
}

/// Anonymous-mode destructive ops record NO principal: the tombstone /
/// retraction version's provenance carries no principal field.
#[test]
fn anonymous_destructive_ops_record_no_principal() {
    let server = AletheiaMcpServer::new(db());

    // delete_node
    let (n, _) = dispatch(&server, "create_node", json!({"label": "Doc"}));
    let nid = n["id"].as_u64().expect("node id");
    let (_, is_error) = dispatch(&server, "delete_node", json!({"node_id": nid}));
    assert!(!is_error);
    let tombstone = last_node_version(&server, nid);
    assert!(
        tombstone
            .get("provenance")
            .and_then(|p| p.get("principal"))
            .is_none(),
        "anonymous delete must not record a principal: {tombstone}"
    );

    // retract_edge
    let (a, _) = dispatch(&server, "create_node", json!({"label": "Person"}));
    let (b, _) = dispatch(&server, "create_node", json!({"label": "Person"}));
    let (edge, _) = dispatch(
        &server,
        "create_edge",
        json!({"source_id": a["id"], "target_id": b["id"], "label": "KNOWS"}),
    );
    let eid = edge["id"].as_u64().expect("edge id");
    let (_, is_error) = dispatch(&server, "retract_edge", json!({"edge_id": eid}));
    assert!(!is_error);
    let retraction = last_edge_version(&server, eid);
    assert!(
        retraction
            .get("provenance")
            .and_then(|p| p.get("principal"))
            .is_none(),
        "anonymous retract must not record a principal: {retraction}"
    );
}

/// A caller attempting to smuggle a `provenance` / `principal` field into a
/// destructive-op request must NOT have it honored: these tools take no
/// caller provenance (serde ignores the unknown field), and the recorded
/// principal is always the verified SESSION principal, never the forged
/// value. Unforgeability holds regardless of what the request JSON contains.
#[test]
fn forged_principal_on_destructive_op_is_ignored() {
    let (server, _store, _id) = server_with_role(Role::Writer);
    let (created, _) = dispatch(&server, "create_node", json!({"label": "Doc"}));
    let node_id = created["id"].as_u64().expect("node id");

    // Inject bogus provenance/principal fields the tool schema does not accept.
    let (deleted, is_error) = dispatch(
        &server,
        "delete_node",
        json!({
            "node_id": node_id,
            "provenance": {"source": "forged", "principal": "forged-admin"},
            "principal": "forged-admin"
        }),
    );
    assert!(!is_error, "delete_node must still succeed: {deleted}");

    let tombstone = last_node_version(&server, node_id);
    assert_eq!(
        tombstone["provenance"]["principal"],
        json!("test-writer"),
        "principal must be the verified session's, never the forged value: {tombstone}"
    );
    // The forged `source` must not have been honored either (these tools
    // carry no caller provenance at all).
    assert!(
        tombstone["provenance"].get("source").is_none(),
        "destructive tools accept no caller provenance source: {tombstone}"
    );
}

// ============================================================================
// AccessClass sanity: the classification only uses classes the role matrix
// covers (guards against a future class being added without matrix review).
// ============================================================================

/// Independent snapshot of the privileged classifications, as hard-coded
/// literals. The other conformance tests compare the table, the dispatch
/// inventory, and the docs matrix against *each other* — a closed loop that
/// would not notice a reviewed-in mistake propagated to all three. This
/// test is the loop-breaker: reclassifying any tool's privilege level (or
/// adding a Write/Metrics tool) must touch this literal list too.
#[test]
fn write_and_metrics_sets_match_hardcoded_snapshot() {
    const EXPECTED_WRITE: [&str; 15] = [
        "create_node",
        "update_node",
        "delete_node",
        "delete_node_cascade",
        "retract_node",
        "create_edge",
        "update_edge",
        "delete_edge",
        "retract_edge",
        "apply_batch",
        "enable_vector_index",
        "enable_unique_constraint",
        "create_node_with_embedding",
        "update_node_embedding",
        // Namespace creation (Issue #3349, PR3b).
        "create_namespace",
    ];
    const EXPECTED_METRICS: [&str; 1] = ["database_stats"];
    // GDPR crypto-shred designation & erasure (Issue #3359, Slice 4b) — the
    // first Admin-class MCP tools.
    const EXPECTED_ADMIN: [&str; 2] = ["designate_subject", "erase_subject"];

    let mut actual_write: Vec<&str> = TOOL_ACCESS_CLASSES
        .iter()
        .filter(|(_, class)| *class == AccessClass::Write)
        .map(|(tool, _)| *tool)
        .collect();
    actual_write.sort_unstable();
    let mut expected_write = EXPECTED_WRITE.to_vec();
    expected_write.sort_unstable();
    assert_eq!(
        actual_write, expected_write,
        "the Write tool set changed — this is a privilege-level change; \
         review it deliberately, then update this snapshot"
    );

    let actual_metrics: Vec<&str> = TOOL_ACCESS_CLASSES
        .iter()
        .filter(|(_, class)| *class == AccessClass::Metrics)
        .map(|(tool, _)| *tool)
        .collect();
    assert_eq!(
        actual_metrics,
        EXPECTED_METRICS.to_vec(),
        "the Metrics tool set changed — review deliberately, then update \
         this snapshot"
    );

    let mut actual_admin: Vec<&str> = TOOL_ACCESS_CLASSES
        .iter()
        .filter(|(_, class)| *class == AccessClass::Admin)
        .map(|(tool, _)| *tool)
        .collect();
    actual_admin.sort_unstable();
    let mut expected_admin = EXPECTED_ADMIN.to_vec();
    expected_admin.sort_unstable();
    assert_eq!(
        actual_admin, expected_admin,
        "the Admin tool set changed — this is a privilege-level change; \
         review it deliberately, then update this snapshot"
    );

    // Everything else must be Read (no fifth class sneaking in).
    assert_eq!(
        TOOL_ACCESS_CLASSES.len(),
        actual_write.len() + actual_metrics.len() + actual_admin.len() + 45,
        "Read tool count changed (expected 45); if a tool was added or \
         removed, re-verify its classification and update this count"
    );
}

#[test]
fn classification_uses_known_classes_only() {
    for (tool, class) in TOOL_ACCESS_CLASSES {
        assert!(
            matches!(
                class,
                AccessClass::Read | AccessClass::Write | AccessClass::Metrics | AccessClass::Admin
            ),
            "tool '{tool}' uses class {class}; only Read/Write/Metrics/Admin are \
             valid MCP access classes — review the matrix (and docs) before adding \
             a new one"
        );
    }
}

// ============================================================================
// 6. Live tool-inventory golden (drift detection for the external mirror)
//
// The external `tests/parity_mcp.rs::tool_inventory_golden_is_stable` test can
// only validate a hardcoded 63-tool constant against itself, because the live
// registry (`list_tools_for_test` / `TOOL_ACCESS_CLASSES`) is `pub(crate)` and
// unreachable from an external test crate. This in-crate test closes that gap:
// it derives the LIVE advertised `(tool_name, access_class)` set from the
// registry and asserts it equals a hardcoded golden snapshot of the 63 pairs.
// A tool that is added, removed, renamed, or reclassified FAILS here — the
// authoritative drift detector the external mirror points back to.
// ============================================================================

/// AC3 (drift): the LIVE advertised MCP tool inventory — every name paired
/// with the `AccessClass` the registry assigns it — must equal this hardcoded
/// golden set of exactly 63 pairs. Adding, dropping, renaming, or
/// reclassifying a tool changes the live set and fails this assertion; update
/// the golden here AND the external mirror (`tests/parity_mcp.rs`) +
/// `tests/parity/inventory.json` deliberately when that happens.
#[test]
fn live_tool_inventory_matches_golden() {
    /// The 63 `(tool_name, access_class)` pairs the server is expected to
    /// advertise, derived from the current live `TOOL_ACCESS_CLASSES`.
    const GOLDEN: [(&str, AccessClass); 63] = [
        // Read (45)
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
        // Embedding generation & text semantic search (Issue #2906).
        ("embed_query", AccessClass::Read),
        ("embed_text", AccessClass::Read),
        ("semantic_search", AccessClass::Read),
        // Semantic-search analysis tools (Issue #2907).
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
        // Namespace discovery (Issue #3349, PR3b).
        ("list_namespaces", AccessClass::Read),
        ("describe_namespace", AccessClass::Read),
        // Metrics (1)
        ("database_stats", AccessClass::Metrics),
        // Write (15)
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
        ("create_node_with_embedding", AccessClass::Write),
        ("update_node_embedding", AccessClass::Write),
        // Namespace creation (Issue #3349, PR3b).
        ("create_namespace", AccessClass::Write),
        // Admin (2) — GDPR crypto-shred designation & erasure (Issue #3359).
        ("designate_subject", AccessClass::Admin),
        ("erase_subject", AccessClass::Admin),
    ];

    // Golden as a set of (name, class-string) pairs.
    let golden: std::collections::BTreeSet<(String, String)> = GOLDEN
        .iter()
        .map(|(name, class)| ((*name).to_string(), class.to_string()))
        .collect();
    assert_eq!(golden.len(), 63, "golden must be 63 unique tool names");

    // Live set derived from the advertised registry + classification table.
    let server = AletheiaMcpServer::new(db());
    let live: std::collections::BTreeSet<(String, String)> = server
        .list_tools_for_test()
        .into_iter()
        .map(|tool| {
            let class = tool_access_class(&tool)
                .unwrap_or_else(|| panic!("advertised tool '{tool}' is unclassified"))
                .to_string();
            (tool, class)
        })
        .collect();

    assert_eq!(
        live, golden,
        "the LIVE advertised MCP tool inventory drifted from the golden \
         63-pair set — a tool was added, removed, renamed, or reclassified. \
         Update GOLDEN here, tests/parity_mcp.rs::TOOL_INVENTORY, and \
         tests/parity/inventory.json deliberately."
    );
}

// ============================================================================
// 7. GDPR crypto-shred admin-gated tools (Issue #3359, Slice 4b)
//
// `designate_subject` / `erase_subject` are the FIRST Admin-class MCP tools.
// These tests prove: (a) an admin key can designate then erase and receives a
// verifiable signed attestation carrying NO key material or plaintext; (b) a
// reader-key AND a writer-key caller are BOTH denied PERMISSION_DENIED on BOTH
// tools (the load-bearing admin-gating proof); (c) erasing an undesignated
// subject is FAILED_PRECONDITION; (d) malformed args are INVALID_ARGUMENT.
// ============================================================================

/// Build a persistent, encryption-configured database rooted at `root` so the
/// crypto-shred key hierarchy is available (an unencrypted `AletheiaDB::new()`
/// returns `FAILED_PRECONDITION` — `EncryptionNotConfigured`).
fn encrypted_db(root: &std::path::Path) -> Arc<AletheiaDB> {
    use crate::config::{AletheiaDBConfig, WalConfigBuilder};
    use crate::encryption::config::EncryptionConfig;
    use crate::storage::index_persistence::PersistenceConfig;

    let key_file = root.join("mek.key");
    crate::encryption::FileKeyProvider::generate_key_file(&key_file).expect("generate key file");
    let config = AletheiaDBConfig::builder()
        .wal(WalConfigBuilder::new().wal_dir(root.join("wal")).build())
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: root.join("data"),
            load_on_startup: true,
            ..Default::default()
        })
        .encryption(EncryptionConfig::file_based(&key_file))
        .build();
    Arc::new(AletheiaDB::with_unified_config(config).expect("encrypted db init"))
}

/// A `Required`-mode server over the given (encrypted) db whose session
/// credential resolves to a principal with `role`.
fn server_over_db_with_role(db: Arc<AletheiaDB>, role: Role) -> AletheiaMcpServer {
    let store = Arc::new(AuthStore::new());
    let (_principal, key) = store
        .create_key(&format!("test-{role}"), role)
        .expect("create key");
    AletheiaMcpServer::with_auth(
        db,
        McpAuthConfig::new(AuthMode::Required, Arc::clone(&store))
            .with_credential(SecretString::new(key.as_str())),
    )
}

#[test]
fn crypto_shred_admin_can_designate_then_erase_with_verifiable_attestation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = encrypted_db(tmp.path());

    // Seed a node to designate.
    let node_id = db
        .create_node(
            "Person",
            crate::core::property::PropertyMapBuilder::new()
                .insert("name", "Alice")
                .build(),
        )
        .expect("create node")
        .as_u64();
    let public_key = db.erasure_attestation_public_key();

    let server = server_over_db_with_role(Arc::clone(&db), Role::Admin);

    // Designate the whole node under a subject.
    let (value, is_error) = dispatch(
        &server,
        "designate_subject",
        json!({
            "subject_id": "user-42",
            "targets": [{ "entity_kind": "node", "id": node_id }],
        }),
    );
    assert!(!is_error, "admin designate_subject must succeed: {value}");
    assert_eq!(value["success"], json!(true));
    assert_eq!(value["subject_id"], json!("user-42"));
    assert_eq!(value["targets_designated"], json!(1));

    // Erase the subject and validate the signed attestation.
    let (att, is_error) = dispatch(&server, "erase_subject", json!({ "subject_id": "user-42" }));
    assert!(!is_error, "admin erase_subject must succeed: {att}");
    assert_eq!(att["success"], json!(true));
    assert_eq!(att["subject_id"], json!("user-42"));
    assert_eq!(att["entity_count"], json!(1));
    assert!(
        att["timestamp"].is_string(),
        "timestamp must be RFC3339: {att}"
    );
    assert!(
        att["timestamp_micros"].is_i64(),
        "timestamp_micros must be int: {att}"
    );

    // Reconstruct the attestation from the hex fields and verify the signature.
    let sig_hex = att["signature"].as_str().expect("signature hex");
    let pk_hex = att["signer_public_key"]
        .as_str()
        .expect("signer_public_key hex");
    let sig_bytes = crate::core::hex::decode(sig_hex).expect("valid signature hex");
    let signature: [u8; 64] = sig_bytes.as_slice().try_into().expect("64-byte signature");
    let signer_public_key =
        crate::audit::AuditPublicKey::from_hex(pk_hex).expect("valid public key hex");
    // The returned signer key must match the db's attestation public key.
    assert_eq!(
        pk_hex,
        public_key.to_hex(),
        "attestation signer must match db key"
    );
    let reconstructed = crate::db::ErasureAttestation {
        subject_id: att["subject_id"].as_str().unwrap().to_string(),
        entity_count: att["entity_count"].as_u64().unwrap() as u32,
        timestamp_micros: att["timestamp_micros"].as_i64().unwrap(),
        signature,
        signer_public_key,
    };
    assert!(
        reconstructed.verify(),
        "the returned attestation must verify against its embedded signer key: {att}"
    );

    // Security: the response carries NO key material or plaintext — only the
    // documented attestation fields.
    let obj = att.as_object().expect("attestation is a JSON object");
    let allowed: std::collections::BTreeSet<&str> = [
        "success",
        "subject_id",
        "entity_count",
        "timestamp",
        "timestamp_micros",
        "signature",
        "signer_public_key",
    ]
    .into_iter()
    .collect();
    for key in obj.keys() {
        assert!(
            allowed.contains(key.as_str()),
            "erase_subject response must not carry an undocumented field '{key}': {att}"
        );
    }
    // The seeded plaintext must never appear anywhere in the response.
    assert!(
        !att.to_string().contains("Alice"),
        "attestation must not leak designated plaintext: {att}"
    );
}

#[test]
fn crypto_shred_reader_and_writer_denied_on_both_tools() {
    // Admin-gating is independent of encryption state (the class gate runs
    // before the tool body), so an unencrypted db is sufficient here.
    for role in [Role::Reader, Role::Writer] {
        let server = server_over_db_with_role(db(), role);
        for tool in ["designate_subject", "erase_subject"] {
            let (value, is_error) = dispatch(
                &server,
                tool,
                json!({ "subject_id": "s", "targets": [{ "entity_kind": "node", "id": 1 }] }),
            );
            assert!(is_error, "[{role} × {tool}] must be denied, got: {value}");
            assert_eq!(
                error_code(&value),
                Some("PERMISSION_DENIED"),
                "[{role} × {tool}] denial must be PERMISSION_DENIED: {value}"
            );
            assert_eq!(
                value["error"]["retriable"], false,
                "[{role} × {tool}] PERMISSION_DENIED must not be retriable: {value}"
            );
            assert_eq!(
                value["error"]["details"]["required_class"],
                json!("admin"),
                "[{role} × {tool}] required_class must be admin: {value}"
            );
        }
    }
}

#[test]
fn crypto_shred_erase_undesignated_is_failed_precondition() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = encrypted_db(tmp.path());
    let server = server_over_db_with_role(db, Role::Admin);

    let (value, is_error) = dispatch(&server, "erase_subject", json!({ "subject_id": "never" }));
    assert!(
        is_error,
        "erasing an undesignated subject must fail: {value}"
    );
    assert_eq!(
        error_code(&value),
        Some("FAILED_PRECONDITION"),
        "undesignated erase must be FAILED_PRECONDITION: {value}"
    );
    assert_eq!(value["error"]["retriable"], false, "{value}");
}

#[test]
fn crypto_shred_malformed_args_are_invalid_argument() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = encrypted_db(tmp.path());
    let server = server_over_db_with_role(db, Role::Admin);

    // Bad entity_kind.
    let (value, is_error) = dispatch(
        &server,
        "designate_subject",
        json!({ "subject_id": "s", "targets": [{ "entity_kind": "vertex", "id": 1 }] }),
    );
    assert!(is_error, "bad entity_kind must fail: {value}");
    assert_eq!(error_code(&value), Some("INVALID_ARGUMENT"), "{value}");

    // Missing subject_id (schema violation).
    let (value, is_error) = dispatch(
        &server,
        "designate_subject",
        json!({ "targets": [{ "entity_kind": "node", "id": 1 }] }),
    );
    assert!(is_error, "missing subject_id must fail: {value}");
    assert_eq!(error_code(&value), Some("INVALID_ARGUMENT"), "{value}");

    // Empty targets (API-level InvalidArgument).
    let (value, is_error) = dispatch(
        &server,
        "designate_subject",
        json!({ "subject_id": "s", "targets": [] }),
    );
    assert!(is_error, "empty targets must fail: {value}");
    assert_eq!(error_code(&value), Some("INVALID_ARGUMENT"), "{value}");
}

/// A `designate_subject` request whose `targets` array exceeds the 1000-target
/// cap is rejected with `INVALID_ARGUMENT` (limit echoed) BEFORE any registry
/// mutation, mirroring the `apply_batch` DoS guard (Issue #3701 hardening).
#[test]
fn crypto_shred_over_cap_targets_are_invalid_argument() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = encrypted_db(tmp.path());
    let server = server_over_db_with_role(db, Role::Admin);

    let targets: Vec<serde_json::Value> = (0..1001)
        .map(|i| json!({ "entity_kind": "node", "id": i }))
        .collect();
    let (value, is_error) = dispatch(
        &server,
        "designate_subject",
        json!({ "subject_id": "s", "targets": targets }),
    );
    assert!(is_error, "over-cap targets must be rejected: {value}");
    assert_eq!(error_code(&value), Some("INVALID_ARGUMENT"), "{value}");
    assert_eq!(value["error"]["retriable"], false, "{value}");
    assert_eq!(value["error"]["details"]["limit"], json!(1000), "{value}");
    assert_eq!(
        value["error"]["details"]["submitted"],
        json!(1001),
        "{value}"
    );
}
