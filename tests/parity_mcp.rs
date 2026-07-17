//! MCP surface PARITY harness (black-box golden pins).
//!
//! Pins the *current, observed* behavior of the AletheiaDB MCP server
//! (`src/mcp/**`) so a future re-implementation must reproduce it **unchanged**.
//!
//! ## Reachability note (important for the design lane)
//!
//! The MCP server is driven here through its **public** black-box surface only:
//!
//!   - `AletheiaMcpServer::new(db)` / `::with_auth(db, cfg)` constructors, and
//!   - the per-tool typed methods (`server.get_node(GetNodeRequest{..}) ->
//!     String`, etc.), each returning the tool's response JSON as a string.
//!
//! The registry dispatcher (`dispatch_tool`), the advertised-tool accessor
//! (`list_tools_for_test`), the token-budget shaper (`apply_budget`), and the
//! auth gate (`SessionAuth::authorize_tool`) are all `pub(crate)` or reached
//! only via the `rmcp::ServerHandler` trait (`list_tools`/`call_tool`), which
//! is NOT nameable from an external test crate (rmcp is not a dev-dependency and
//! is not re-exported). Per the harness house rules we do NOT add `pub` to
//! production code to reach them. Consequently the following are pinned by the
//! EXISTING in-crate suites and only *referenced* in `tests/parity/inventory.json`
//! (not duplicated here):
//!
//!   - advertised-tool registry sweep (every advertised tool dispatchable and
//!     returns a well-formed structured error on invalid args):
//!     `src/mcp/tests.rs::every_advertised_tool_returns_structured_error_on_invalid_arguments`
//!   - RBAC class enforcement (uniform UNAUTHENTICATED, PERMISSION_DENIED
//!     matrix, every-advertised-tool-classified):
//!     `src/mcp/auth_tests.rs::every_advertised_tool_has_a_classification`
//!   - token-budget degradation ladder: `src/mcp/tests.rs` budget suite.
//!
//! What this file DOES pin publicly: the structured error-code vocabulary
//! (via the public `McpErrorCode` enum — the single source of truth for the
//! wire codes), the success + structured-error envelope shapes, the temporal
//! block on read responses, and the vector-elision default. It also pins the
//! full 58-tool inventory + access-class table as a golden constant that a
//! porter must keep in lockstep with the server.
//!
//! Run with:
//!   cargo test --features mcp-server --test parity_mcp

#![cfg(feature = "mcp-server")]

use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::mcp::{
    AletheiaMcpServer, CreateNodeRequest, GetNodeRequest, McpErrorCode, QueryLimitsConfig,
    QueryLimitsOverride, QueryRequest, TraverseRequest,
};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn server() -> AletheiaMcpServer {
    AletheiaMcpServer::new(Arc::new(AletheiaDB::new().expect("create DB")))
}

/// The 10 stable structured error codes, in the enum's declared order.
const ALL_CODES: [McpErrorCode; 10] = [
    McpErrorCode::NotFound,
    McpErrorCode::InvalidArgument,
    McpErrorCode::ConstraintViolation,
    McpErrorCode::FailedPrecondition,
    McpErrorCode::Conflict,
    McpErrorCode::Unavailable,
    McpErrorCode::Internal,
    McpErrorCode::Unauthenticated,
    McpErrorCode::PermissionDenied,
    McpErrorCode::ResourceExhausted,
];

fn code_strings() -> Vec<&'static str> {
    ALL_CODES.iter().map(|c| c.as_str()).collect()
}

/// Assert a response string is a well-formed structured MCP error:
/// `{error:{code ∈ enum, message: non-empty string, retriable: bool}}`.
/// Returns the `code` string.
fn assert_structured_error(resp: &str, ctx: &str) -> String {
    let v: Value = serde_json::from_str(resp).unwrap_or_else(|e| panic!("[{ctx}] not JSON: {e}"));
    let err = v
        .get("error")
        .unwrap_or_else(|| panic!("[{ctx}] missing `error`: {v}"));
    let code = err["code"]
        .as_str()
        .unwrap_or_else(|| panic!("[{ctx}] error.code not a string: {v}"))
        .to_string();
    assert!(
        code_strings().contains(&code.as_str()),
        "[{ctx}] error.code `{code}` not in the stable enum: {v}"
    );
    assert!(
        err["message"].as_str().is_some_and(|s| !s.is_empty()),
        "[{ctx}] error.message must be a non-empty string: {v}"
    );
    assert!(
        err["retriable"].is_boolean(),
        "[{ctx}] error.retriable must be present and boolean: {v}"
    );
    code
}

/// Create a node via the public typed method, returning its parsed response.
fn create_node(server: &AletheiaMcpServer, body: Value) -> Value {
    let req: CreateNodeRequest =
        serde_json::from_value(body).expect("valid CreateNodeRequest json");
    let resp = server.create_node(req);
    let v: Value = serde_json::from_str(&resp).expect("json");
    assert!(v.get("error").is_none(), "create_node failed: {v}");
    v
}

// ===========================================================================
// Structured error-code vocabulary — the stability contract.
// ===========================================================================

/// PARITY: the wire representation of every `McpErrorCode` and its default
/// retriable classification. A port MUST reproduce these exact strings and
/// flags. Codes may be ADDED over time (extend this test), but an existing
/// code changing its spelling or retriable default is a breaking change.
#[test]
fn error_code_vocabulary_is_stable() {
    let pairs: Vec<(&str, bool)> = ALL_CODES
        .iter()
        .map(|c| (c.as_str(), c.default_retriable()))
        .collect();

    assert_eq!(
        pairs,
        vec![
            ("NOT_FOUND", false),
            ("INVALID_ARGUMENT", false),
            ("CONSTRAINT_VIOLATION", false),
            ("FAILED_PRECONDITION", false),
            ("CONFLICT", true),
            ("UNAVAILABLE", true),
            ("INTERNAL", false),
            ("UNAUTHENTICATED", false),
            ("PERMISSION_DENIED", false),
            // Issue #3368: per-query resource-limit breaches. Default
            // non-retriable; the read-only wall-clock-timeout case overrides
            // retriable:true explicitly at the call site.
            ("RESOURCE_EXHAUSTED", false),
        ],
        "MCP structured error vocabulary changed — a port must preserve these \
         codes + retriable defaults (keep tests/parity/inventory.json in sync)"
    );

    // Only Conflict/Unavailable are transient-by-default.
    assert!(McpErrorCode::Conflict.default_retriable());
    assert!(McpErrorCode::Unavailable.default_retriable());
    assert!(!McpErrorCode::NotFound.default_retriable());
    assert!(!McpErrorCode::Internal.default_retriable());
}

// ===========================================================================
// Success + structured-error envelope shapes (via public typed methods).
// ===========================================================================

/// PARITY: a create_node success response is a bare entity object carrying
/// `id`, `label`, `properties`, and a `temporal` block with open (null) upper
/// bounds and `is_current: true`. No `error` key on success.
#[test]
fn success_envelope_and_temporal_block() {
    let s = server();
    let v = create_node(
        &s,
        json!({ "label": "Person", "properties": { "name": "Alice" } }),
    );

    assert!(v["id"].as_u64().is_some(), "id must be a number: {v}");
    assert_eq!(v["label"], "Person");
    assert_eq!(v["properties"]["name"], "Alice");

    let t = v
        .get("temporal")
        .unwrap_or_else(|| panic!("read response must carry a `temporal` block: {v}"));
    // Lower bounds are RFC3339 strings; upper bounds are explicit JSON null.
    assert!(
        t["valid_from"].as_str().is_some(),
        "valid_from RFC3339 string: {t}"
    );
    assert!(
        t["transaction_from"].as_str().is_some(),
        "transaction_from RFC3339 string: {t}"
    );
    assert!(
        t.get("valid_to").is_some_and(Value::is_null),
        "valid_to must be present as null for an open interval: {t}"
    );
    assert!(
        t.get("transaction_to").is_some_and(Value::is_null),
        "transaction_to must be present as null for an open interval: {t}"
    );
    assert_eq!(
        t["is_current"],
        json!(true),
        "a live version is_current: {t}"
    );
}

/// PARITY: a lookup of a non-existent (but syntactically valid) node id yields
/// the structured error envelope with `code = NOT_FOUND`, `retriable = false`.
#[test]
fn not_found_structured_error() {
    let s = server();
    let req: GetNodeRequest = serde_json::from_value(json!({ "node_id": 999_999 })).unwrap();
    let resp = s.get_node(req);
    let code = assert_structured_error(&resp, "get_node/missing");
    assert_eq!(code, "NOT_FOUND");

    let v: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["error"]["retriable"], json!(false));
}

/// PARITY: an out-of-range node id (beyond `MAX_VALID_ID`) is rejected as a
/// caller-fault structured error rather than panicking or succeeding. The code
/// is DETERMINISTIC: `NodeId::new` rejects the overflow id and `get_node` maps
/// that to `INVALID_ARGUMENT` (never `NOT_FOUND` — the id never resolves to a
/// lookup), non-retriable. A port must reproduce this exact classification.
#[test]
fn out_of_range_id_is_non_retriable_structured_error() {
    let s = server();
    let req: GetNodeRequest = serde_json::from_value(json!({ "node_id": u64::MAX })).unwrap();
    let resp = s.get_node(req);
    let code = assert_structured_error(&resp, "get_node/overflow");

    let v: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(
        v["error"]["retriable"],
        json!(false),
        "an out-of-range id is caller-fault, never retriable: {v}"
    );
    // Exact, deterministic classification — the id-validation rejection.
    assert_eq!(
        code, "INVALID_ARGUMENT",
        "an overflow id is an id-validation rejection → INVALID_ARGUMENT: {v}"
    );

    // Exact key-set pin on the error object: an id-validation error carries
    // exactly `{code, message, retriable}` (no `details` on this path). An
    // added/renamed error field is caught here.
    let err_keys: std::collections::BTreeSet<&str> = v["error"]
        .as_object()
        .expect("error is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        err_keys,
        std::collections::BTreeSet::from(["code", "message", "retriable"]),
        "id-validation error object must be exactly {{code, message, retriable}}: {v}"
    );
}

// ===========================================================================
// Vector elision default (Issue #3220).
// ===========================================================================

/// PARITY: an embedding-valued property is elided by default to a
/// `{type:"vector", dim, elided:true}` descriptor, and `include_vectors:true`
/// restores the full float array losslessly.
#[test]
fn vectors_elided_by_default_and_restorable() {
    let s = server();
    let embedding: Vec<f32> = (0..8).map(|i| i as f32 * 0.25).collect();
    let created = create_node(
        &s,
        json!({ "label": "Document", "properties": { "embedding": embedding.clone() } }),
    );
    let node_id = created["id"].as_u64().expect("node id");

    // Default: elided descriptor.
    let req: GetNodeRequest = serde_json::from_value(json!({ "node_id": node_id })).unwrap();
    let elided: Value = serde_json::from_str(&s.get_node(req)).unwrap();
    assert_eq!(
        elided["properties"]["embedding"],
        json!({ "type": "vector", "dim": 8, "elided": true }),
        "embedding must be elided by default: {elided}"
    );

    // include_vectors:true: full array restored.
    let req: GetNodeRequest =
        serde_json::from_value(json!({ "node_id": node_id, "include_vectors": true })).unwrap();
    let full: Value = serde_json::from_str(&s.get_node(req)).unwrap();
    let got: Vec<f32> = full["properties"]["embedding"]
        .as_array()
        .expect("full array")
        .iter()
        .map(|v| v.as_f64().unwrap() as f32)
        .collect();
    assert_eq!(got, embedding, "include_vectors must be lossless");
}

// ===========================================================================
// Representative black-box roundtrip through several tools — proves the public
// typed entry points are dispatchable and return well-formed JSON.
// ===========================================================================

/// PARITY: a small graph built and traversed through public typed methods
/// yields well-formed, non-error JSON (no envelope drift on the happy path).
#[test]
fn representative_tool_roundtrip_is_wellformed() {
    let s = server();
    let a = create_node(
        &s,
        json!({ "label": "Person", "properties": { "name": "A" } }),
    );
    let b = create_node(
        &s,
        json!({ "label": "Person", "properties": { "name": "B" } }),
    );
    let (a_id, b_id) = (a["id"].as_u64().unwrap(), b["id"].as_u64().unwrap());

    // create_edge via JSON-built request.
    let edge_req: aletheiadb::mcp::CreateEdgeRequest = serde_json::from_value(json!({
        "source_id": a_id, "target_id": b_id, "label": "KNOWS"
    }))
    .unwrap();
    let edge: Value = serde_json::from_str(&s.create_edge(edge_req)).unwrap();
    assert!(edge.get("error").is_none(), "create_edge failed: {edge}");

    // traverse from A over KNOWS.
    let tr_req: TraverseRequest = serde_json::from_value(json!({
        "start_node_id": a_id, "edge_label": "KNOWS", "depth": 1
    }))
    .unwrap();
    let traversed: Value = serde_json::from_str(&s.traverse(tr_req)).unwrap();
    assert!(
        traversed.get("error").is_none(),
        "traverse failed: {traversed}"
    );

    // The traversal must actually REACH node B — not merely return a
    // well-formed empty page. Assert B is present in the results.
    let results = traversed["results"]
        .as_array()
        .unwrap_or_else(|| panic!("traverse response must carry a `results` array: {traversed}"));
    assert_eq!(
        traversed["count"].as_u64(),
        Some(1),
        "one-hop KNOWS traversal from A reaches exactly B: {traversed}"
    );
    let reached_b = results
        .iter()
        .any(|r| r["node"]["id"].as_u64() == Some(b_id));
    assert!(
        reached_b,
        "traversal over KNOWS must reach node B (id={b_id}): {traversed}"
    );
}

// ===========================================================================
// Golden tool inventory — the 58-tool advertised set + access classes.
//
// This is the one place the FULL registry is pinned from an external test:
// because the advertised list (`tool_definitions`) is not reachable through the
// public API, we mirror it as a golden constant a porter must keep in lockstep
// with the server and with tests/parity/inventory.json. The in-crate registry
// sweep (src/mcp/tests.rs) enforces the server side; this enforces the mirror.
// ===========================================================================

/// (tool_name, access_class) for every advertised MCP tool, per
/// `src/mcp/auth.rs::TOOL_ACCESS_CLASSES`. Access class ∈ {read, write, metrics}
/// (MCP advertises no admin-class tools).
const TOOL_INVENTORY: [(&str, &str); 58] = [
    ("get_node", "read"),
    ("create_node", "write"),
    ("update_node", "write"),
    ("delete_node", "write"),
    ("retract_node", "write"),
    ("retract_edge", "write"),
    ("delete_node_cascade", "write"),
    ("list_nodes", "read"),
    ("count_nodes", "read"),
    ("get_edge", "read"),
    ("create_edge", "write"),
    ("update_edge", "write"),
    ("delete_edge", "write"),
    ("apply_batch", "write"),
    ("list_edges", "read"),
    ("count_edges", "read"),
    ("get_outgoing_edges", "read"),
    ("get_incoming_edges", "read"),
    ("traverse", "read"),
    ("find_similar", "read"),
    ("embed_query", "read"),
    ("embed_text", "read"),
    ("semantic_search", "read"),
    ("create_node_with_embedding", "write"),
    ("update_node_embedding", "write"),
    ("semantic_path", "read"),
    ("concept_analogy", "read"),
    ("concept_mean", "read"),
    ("find_duplicate_candidates", "read"),
    ("semantic_horizon", "read"),
    ("context_aspects", "read"),
    ("enable_vector_index", "write"),
    ("list_vector_indexes", "read"),
    ("enable_unique_constraint", "write"),
    ("list_unique_constraints", "read"),
    ("get_node_at_time", "read"),
    ("get_edge_at_time", "read"),
    ("find_nodes_at_time", "read"),
    ("list_changes", "read"),
    ("await_changes", "read"),
    ("get_node_at_valid_time", "read"),
    ("get_node_at_transaction_time", "read"),
    ("get_node_history", "read"),
    ("diff_node_versions", "read"),
    ("get_edge_at_valid_time", "read"),
    ("get_edge_at_transaction_time", "read"),
    ("get_edge_history", "read"),
    ("diff_edge_versions", "read"),
    ("hybrid_query", "read"),
    ("query", "read"),
    ("get_schema", "read"),
    ("temporal_extent", "read"),
    ("lineage_upstream", "read"),
    ("lineage_downstream", "read"),
    ("audit_export", "read"),
    ("verify_chain", "read"),
    ("export_chain_head", "read"),
    ("database_stats", "metrics"),
];

/// PARITY (external mirror, NOT a live drift detector): this constant is a
/// cross-crate reference copy of the 58-tool inventory. Because the live
/// registry (`list_tools_for_test` / `TOOL_ACCESS_CLASSES`) is `pub(crate)`
/// and unreachable from this external test crate, this test only validates the
/// mirror's internal consistency (58 tools, unique names, MCP-legal classes,
/// exactly one metrics tool) — it does NOT read the server, so it cannot by
/// itself catch a tool added/removed/renamed/reclassified in the registry.
///
/// Live drift detection is performed in-crate by
/// `src/mcp/auth_tests.rs::live_tool_inventory_matches_golden`, which derives
/// the advertised `(name, class)` set from the registry and asserts it equals
/// an identical hardcoded golden. Keep this mirror, that in-crate golden, and
/// `tests/parity/inventory.json` in lockstep when the inventory changes.
#[test]
fn tool_inventory_golden_is_stable() {
    assert_eq!(TOOL_INVENTORY.len(), 58, "MCP advertises exactly 58 tools");

    // Names unique.
    let mut names: Vec<&str> = TOOL_INVENTORY.iter().map(|(n, _)| *n).collect();
    let count = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), count, "tool names must be unique");

    // Access classes are drawn from the MCP-legal set (no admin tools).
    for (name, class) in TOOL_INVENTORY {
        assert!(
            matches!(class, "read" | "write" | "metrics"),
            "tool `{name}` has an unexpected access class `{class}`"
        );
    }

    // Exactly one metrics tool, and it is database_stats.
    let metrics: Vec<&str> = TOOL_INVENTORY
        .iter()
        .filter(|(_, c)| *c == "metrics")
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(metrics, vec!["database_stats"]);
}

// ===========================================================================
// Per-query resource limits on the `query` tool (Issue #3368).
//
// A port MUST reproduce: an over-ceiling per-call override → INVALID_ARGUMENT;
// a result-byte-cap breach → RESOURCE_EXHAUSTED (non-retriable); a row-cap
// override → a SUCCESS truncated with `truncated:true` (the #3226 completeness
// signal), never an error. AQL is used so the pins hold in cypher-less builds.
// ===========================================================================

fn seed_widget(server: &AletheiaMcpServer, name: &str) {
    create_node(
        server,
        json!({ "label": "Widget", "properties": { "name": name } }),
    );
}

fn run_query(server: &AletheiaMcpServer, req: QueryRequest) -> Value {
    serde_json::from_str(&server.query(req)).expect("query response is JSON")
}

/// PARITY: an over-ceiling `limits` override is a structured INVALID_ARGUMENT
/// with `details.dimension`, rejected before execution and non-retriable.
#[test]
fn query_over_ceiling_override_is_invalid_argument() {
    let s = server();
    let v = run_query(
        &s,
        QueryRequest {
            namespace: None,
            language: "aql".into(),
            query: "MATCH (n) RETURN n".into(),
            params: None,
            limit: None,
            limits: Some(QueryLimitsOverride {
                timeout_ms: Some(u64::MAX),
                ..Default::default()
            }),
        },
    );
    let code = assert_structured_error(&serde_json::to_string(&v).unwrap(), "over-ceiling");
    assert_eq!(code, "INVALID_ARGUMENT");
    assert_eq!(v["error"]["retriable"].as_bool(), Some(false), "{v}");
    assert_eq!(
        v["error"]["details"]["dimension"].as_str(),
        Some("wall_clock_timeout"),
        "{v}"
    );
}

/// PARITY: a response over the result-byte cap fails closed with a
/// non-retriable RESOURCE_EXHAUSTED carrying `details.consumed > limit`.
#[test]
fn query_byte_cap_is_resource_exhausted() {
    let s = server().with_query_limits(QueryLimitsConfig {
        default_max_response_bytes: 40,
        ..QueryLimitsConfig::default()
    });
    seed_widget(&s, "alpha");
    let v = run_query(
        &s,
        QueryRequest {
            namespace: None,
            language: "aql".into(),
            query: "MATCH (n:Widget) RETURN n".into(),
            params: None,
            limit: None,
            limits: None,
        },
    );
    let code = assert_structured_error(&serde_json::to_string(&v).unwrap(), "byte-cap");
    assert_eq!(code, "RESOURCE_EXHAUSTED");
    assert_eq!(v["error"]["retriable"].as_bool(), Some(false), "{v}");
    assert_eq!(
        v["error"]["details"]["dimension"].as_str(),
        Some("result_bytes")
    );
    assert!(
        v["error"]["details"]["consumed"].as_u64().unwrap()
            > v["error"]["details"]["limit"].as_u64().unwrap()
    );
}

/// PARITY: a row-cap override truncates with the `truncated:true` completeness
/// signal — a SUCCESS, not an error (the list-like alternative to fail-closed).
#[test]
fn query_row_cap_override_truncates_successfully() {
    let s = server();
    for name in ["a", "b", "c"] {
        seed_widget(&s, name);
    }
    let v = run_query(
        &s,
        QueryRequest {
            namespace: None,
            language: "aql".into(),
            query: "MATCH (n:Widget) RETURN n".into(),
            params: None,
            limit: None,
            limits: Some(QueryLimitsOverride {
                max_result_rows: Some(1),
                ..Default::default()
            }),
        },
    );
    assert!(
        v.get("error").is_none(),
        "row cap is a truncated success: {v}"
    );
    assert_eq!(v["row_count"].as_u64(), Some(1), "{v}");
    assert_eq!(v["truncated"].as_bool(), Some(true), "{v}");
}

/// PARITY: RESOURCE_EXHAUSTED is in the stable code vocabulary and is
/// non-retriable by default (the wall-clock-timeout case overrides to
/// retriable at its call site; the byte cap keeps the default).
#[test]
fn resource_exhausted_code_is_in_vocabulary() {
    assert!(code_strings().contains(&"RESOURCE_EXHAUSTED"));
    assert!(!McpErrorCode::ResourceExhausted.default_retriable());
}
