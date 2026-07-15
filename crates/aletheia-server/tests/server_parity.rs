//! Parity + surface tests for the production autumn-web 0.5 server (Issue #3524).
//!
//! Mirrors `crates/autumn-spike/tests/autumn_spike_parity.rs`: it drives the new
//! crate's autumn [`TestClient`] and asserts behavior against the **legacy**
//! surfaces sharing the same `Arc<AletheiaDB>` + `Arc<AuthStore>` —
//!
//! - the four ported HTTP routes vs the legacy 0.4 router
//!   ([`aletheiadb::http::build_test_router_with_auth`]), and
//! - the three node-read tools vs the legacy MCP server
//!   ([`aletheiadb::mcp::AletheiaMcpServer`]) —
//!
//! plus `/openapi.json` and the `/mcp` catalog. Deterministic bodies are
//! asserted byte-identical (parsed-JSON equality, `trace_id` stripped);
//! non-deterministic admin bodies (random key/id/timestamp) are shape-asserted.

use std::sync::Arc;

use aletheia_server::build_server_client;
use aletheia_server::security::rbac;
use aletheiadb::auth::{AccessClass, AuthMode, AuthStore, Role};
use aletheiadb::core::NodeId;
use aletheiadb::mcp::{AletheiaMcpServer, CountNodesRequest, GetNodeRequest, ListNodesRequest};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use autumn_web::test::TestClient;
use serde_json::{Value, json};

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";
const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";
const METRICS_TOKEN: &str = "metrics-token-abcdefghijklmnop";
const EMBEDDING: &[f32] = &[0.10, 0.20, 0.30, 0.40];

/// A shared DB holding one `Person` node (with a vector property) and a shared
/// auth store with admin / reader / metrics keys. Returns the node id.
fn fixture() -> (Arc<AletheiaDB>, Arc<AuthStore>, NodeId) {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30_i64)
                .insert_vector("embedding", EMBEDDING)
                .build(),
        )
        .expect("create node");

    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("admin", Role::Admin, ADMIN_TOKEN)
        .expect("insert admin key");
    store
        .insert_bootstrap_key("reader", Role::Reader, READER_TOKEN)
        .expect("insert reader key");
    store
        .insert_bootstrap_key("metrics", Role::Metrics, METRICS_TOKEN)
        .expect("insert metrics key");

    (db, store, node_id)
}

/// Drop the per-request `trace_id` so two responses can be compared.
fn strip_trace(mut v: Value) -> Value {
    if let Some(obj) = v.as_object_mut() {
        obj.remove("trace_id");
    }
    v
}

// ── legacy 0.4 HTTP router drivers ─────────────────────────────────────────

async fn legacy_router(db: Arc<AletheiaDB>, store: Arc<AuthStore>, mode: AuthMode) -> axum::Router {
    let app_state = aletheiadb::http::AppState::new(db);
    let auth_state = aletheiadb::http::AuthState::new(store, mode);
    let config = aletheiadb::http::ServerConfig::default();
    aletheiadb::http::build_test_router_with_auth(app_state, auth_state, &config)
        .expect("build 0.4 router")
}

async fn legacy_request(
    router: axum::Router,
    method: &str,
    uri: &str,
    body: Option<String>,
    bearer: Option<&str>,
) -> (u16, Value) {
    use tower::ServiceExt as _;
    let mut builder = axum::http::Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = builder
        .body(axum::body::Body::from(body.unwrap_or_default()))
        .expect("build request");
    let response = router.oneshot(request).await.expect("oneshot");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: Value = serde_json::from_slice(&bytes).expect("legacy body is JSON");
    (status, strip_trace(json))
}

// ── new-crate HTTP drivers ─────────────────────────────────────────────────

async fn new_get(client: &TestClient, uri: &str, bearer: Option<&str>) -> (u16, Value) {
    let mut req = client.get(uri);
    if let Some(token) = bearer {
        req = req.header("authorization", &format!("Bearer {token}"));
    }
    let resp = req.send().await;
    let status = resp.status.as_u16();
    let body: Value = serde_json::from_str(&resp.text()).expect("new body is JSON");
    (status, strip_trace(body))
}

async fn new_post(
    client: &TestClient,
    uri: &str,
    body: &Value,
    bearer: Option<&str>,
) -> (u16, Value) {
    let mut req = client.post(uri);
    if let Some(token) = bearer {
        req = req.header("authorization", &format!("Bearer {token}"));
    }
    let resp = req.json(body).send().await;
    let status = resp.status.as_u16();
    let body: Value = serde_json::from_str(&resp.text()).expect("new body is JSON");
    (status, strip_trace(body))
}

// ════════════════════════════════════════════════════════════════════════════
// HTTP route parity: GET /status
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn status_success_byte_parity_required_mode() {
    let (db, store, _) = fixture();
    let client = build_server_client(db.clone(), store.clone(), AuthMode::Required);

    let (s_status, s_body) = new_get(&client, "/status", Some(READER_TOKEN)).await;
    let router = legacy_router(db, store, AuthMode::Required).await;
    let (l_status, l_body) =
        legacy_request(router, "GET", "/status", None, Some(READER_TOKEN)).await;

    assert_eq!(s_status, 200);
    assert_eq!(s_status, l_status);
    assert_eq!(s_body, json!({ "status": "healthy" }));
    assert_eq!(s_body, l_body, "/status body must equal legacy");
}

#[tokio::test]
async fn status_unauthenticated_401_byte_parity() {
    let (db, store, _) = fixture();
    let client = build_server_client(db.clone(), store.clone(), AuthMode::Required);

    let resp = client.get("/status").send().await;
    assert_eq!(resp.status.as_u16(), 401);
    assert_eq!(resp.header("www-authenticate"), Some("Bearer"));

    let (s_status, s_body) = new_get(&client, "/status", None).await;
    let router = legacy_router(db, store, AuthMode::Required).await;
    let (l_status, l_body) = legacy_request(router, "GET", "/status", None, None).await;
    assert_eq!(s_status, 401);
    assert_eq!(s_status, l_status);
    assert_eq!(s_body["error"]["code"], "UNAUTHENTICATED");
    assert_eq!(s_body, l_body, "uniform 401 must equal legacy");
}

#[tokio::test]
async fn status_success_anonymous_mode() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Anonymous);
    // Anonymous: no credential needed.
    let (status, body) = new_get(&client, "/status", None).await;
    assert_eq!(status, 200);
    assert_eq!(body, json!({ "status": "healthy" }));
}

// ════════════════════════════════════════════════════════════════════════════
// HTTP route parity: /admin/keys (create, list, revoke)
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn admin_list_keys_byte_parity_with_shared_store() {
    let (db, store, _) = fixture();
    let client = build_server_client(db.clone(), store.clone(), AuthMode::Required);

    let (s_status, s_body) = new_get(&client, "/admin/keys", Some(ADMIN_TOKEN)).await;
    let router = legacy_router(db, store, AuthMode::Required).await;
    let (l_status, l_body) =
        legacy_request(router, "GET", "/admin/keys", None, Some(ADMIN_TOKEN)).await;

    assert_eq!(s_status, 200);
    assert_eq!(s_status, l_status);
    assert_eq!(s_body["success"], true);
    // Shared store → identical masked principal list (sorted by id).
    assert_eq!(s_body, l_body, "masked key list must equal legacy");
    // Never leak key material.
    for k in s_body["data"]["keys"].as_array().unwrap() {
        assert!(k.get("key").is_none(), "list must not contain key material");
    }
}

#[tokio::test]
async fn admin_list_keys_unauthenticated_401_byte_parity() {
    let (db, store, _) = fixture();
    let client = build_server_client(db.clone(), store.clone(), AuthMode::Required);

    let (s_status, s_body) = new_get(&client, "/admin/keys", None).await;
    let router = legacy_router(db, store, AuthMode::Required).await;
    let (l_status, l_body) = legacy_request(router, "GET", "/admin/keys", None, None).await;
    assert_eq!(s_status, 401);
    assert_eq!(s_status, l_status);
    assert_eq!(s_body, l_body, "admin 401 must equal legacy");
}

#[tokio::test]
async fn admin_create_key_success_shape() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let (status, body) = new_post(
        &client,
        "/admin/keys",
        &json!({ "name": "svc-1", "role": "reader" }),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
    let data = &body["data"];
    assert!(data["id"].is_string());
    assert_eq!(data["name"], "svc-1");
    assert_eq!(data["role"], "reader");
    assert!(data["key_prefix"].is_string());
    assert!(data["created_at"].is_string());
    // Plaintext key returned exactly once, prefixed by its masked prefix.
    let key = data["key"].as_str().expect("plaintext key");
    let prefix = data["key_prefix"].as_str().unwrap();
    assert!(
        key.starts_with(prefix),
        "key {key} must extend prefix {prefix}"
    );
}

#[tokio::test]
async fn admin_create_key_unauthenticated_401_byte_parity() {
    let (db, store, _) = fixture();
    let client = build_server_client(db.clone(), store.clone(), AuthMode::Required);
    let body = json!({ "name": "x", "role": "reader" });

    let (s_status, s_body) = new_post(&client, "/admin/keys", &body, None).await;
    let router = legacy_router(db, store, AuthMode::Required).await;
    let (l_status, l_body) =
        legacy_request(router, "POST", "/admin/keys", Some(body.to_string()), None).await;
    assert_eq!(s_status, 401);
    assert_eq!(s_status, l_status);
    assert_eq!(s_body, l_body, "create-key 401 must equal legacy");
}

#[tokio::test]
async fn admin_revoke_key_behavior() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store.clone(), AuthMode::Required);

    // Mint a key to revoke via the store directly.
    let (principal, _key) = store.create_key("to-revoke", Role::Reader).expect("mint");

    let (status, body) = new_post(
        &client,
        "/admin/keys/revoke",
        &json!({ "id": principal.id }),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({ "success": true, "data": { "revoked": true, "id": principal.id } })
    );

    // Revoking an unknown id → 404.
    let (status, body) = new_post(
        &client,
        "/admin/keys/revoke",
        &json!({ "id": "does-not-exist" }),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, 404);
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "NOT_FOUND");
}

// ════════════════════════════════════════════════════════════════════════════
// Node-read proof slice: get_node / list_nodes / count_nodes vs legacy MCP
// ════════════════════════════════════════════════════════════════════════════

fn mcp_server(db: &Arc<AletheiaDB>) -> AletheiaMcpServer {
    AletheiaMcpServer::new(db.clone())
}

#[tokio::test]
async fn get_node_byte_parity_with_legacy_mcp() {
    let (db, store, node_id) = fixture();
    let client = build_server_client(db.clone(), store, AuthMode::Required);

    let (status, body) = new_get(
        &client,
        &format!("/nodes/{}", node_id.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);

    let legacy: Value = serde_json::from_str(&mcp_server(&db).get_node(GetNodeRequest {
        node_id: node_id.as_u64(),
        include_vectors: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(
        body, legacy,
        "get_node HTTP body must equal legacy MCP method"
    );
    // Read responses carry a temporal block; the vector is elided by default.
    assert!(
        body["temporal"].is_object(),
        "temporal block present: {body}"
    );
    assert_eq!(body["properties"]["embedding"]["elided"], true);
}

#[tokio::test]
async fn get_node_include_vectors_parity() {
    let (db, store, node_id) = fixture();
    let client = build_server_client(db.clone(), store, AuthMode::Required);

    let (_s, body) = new_get(
        &client,
        &format!("/nodes/{}?include_vectors=true", node_id.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    let legacy: Value = serde_json::from_str(&mcp_server(&db).get_node(GetNodeRequest {
        node_id: node_id.as_u64(),
        include_vectors: Some(true),
    }))
    .expect("legacy mcp json");
    assert_eq!(body, legacy);
    assert!(
        body["properties"]["embedding"].is_array(),
        "include_vectors=true returns the raw array"
    );
}

#[tokio::test]
async fn get_node_anonymous_mode_parity() {
    let (db, store, node_id) = fixture();
    let client = build_server_client(db.clone(), store, AuthMode::Anonymous);
    // Anonymous: no credential.
    let (status, body) = new_get(&client, &format!("/nodes/{}", node_id.as_u64()), None).await;
    assert_eq!(status, 200);
    let legacy: Value = serde_json::from_str(&mcp_server(&db).get_node(GetNodeRequest {
        node_id: node_id.as_u64(),
        include_vectors: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(body, legacy);
}

#[tokio::test]
async fn get_node_unauthenticated_401() {
    let (db, store, node_id) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let resp = client
        .get(&format!("/nodes/{}", node_id.as_u64()))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 401);
    assert_eq!(resp.header("www-authenticate"), Some("Bearer"));
}

#[tokio::test]
async fn list_nodes_byte_parity_with_legacy_mcp() {
    let (db, store, _) = fixture();
    let client = build_server_client(db.clone(), store, AuthMode::Required);

    let (status, body) = new_get(&client, "/nodes?label=Person", Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    let legacy: Value = serde_json::from_str(&mcp_server(&db).list_nodes(ListNodesRequest {
        label: Some("Person".to_string()),
        property_key: None,
        property_value: None,
        limit: None,
        offset: None,
        include_vectors: None,
    }))
    .expect("legacy mcp json");
    assert_eq!(
        body, legacy,
        "list_nodes HTTP body must equal legacy MCP method"
    );
}

#[tokio::test]
async fn count_nodes_byte_parity_with_legacy_mcp() {
    let (db, store, _) = fixture();
    let client = build_server_client(db.clone(), store, AuthMode::Required);

    let (status, body) = new_get(&client, "/nodes/count", Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    let legacy: Value =
        serde_json::from_str(&mcp_server(&db).count_nodes(CountNodesRequest { label: None }))
            .expect("legacy mcp json");
    assert_eq!(
        body, legacy,
        "count_nodes HTTP body must equal legacy MCP method"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// OpenAPI + /mcp catalog
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn openapi_serves_and_documents_get_node() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let resp = client.get("/openapi.json").send().await;
    assert_eq!(resp.status.as_u16(), 200);
    let spec: Value = resp.json();
    let op = &spec["paths"]["/nodes/{id}"]["get"];
    assert!(
        !op.is_null(),
        "GET /nodes/{{id}} documented: {}",
        spec["paths"]
    );
    let params = op["parameters"].as_array().expect("parameters");
    let id_param = params
        .iter()
        .find(|p| p["name"] == "id" && p["in"] == "path")
        .expect("id path param");
    assert_eq!(id_param["required"], true);
}

#[tokio::test]
async fn mcp_tools_list_advertises_three_node_tools_with_read_class() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let rpc = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_str(&resp.text()).expect("json");
    assert_eq!(body["id"], 1);
    let tools = body["result"]["tools"].as_array().expect("tools array");

    for name in ["get_node", "list_nodes", "count_nodes"] {
        assert!(
            tools.iter().any(|t| t["name"] == name),
            "/mcp must advertise {name}: {body}"
        );
        // The RBAC stub classifies each node-read tool as Read.
        assert_eq!(
            rbac::tool_access_class(name),
            Some(AccessClass::Read),
            "{name} access class"
        );
    }

    // get_node's inputSchema carries the `id` param, required.
    let get_node = tools.iter().find(|t| t["name"] == "get_node").unwrap();
    assert!(get_node["inputSchema"]["properties"]["id"].is_object());
    assert!(
        get_node["inputSchema"]["required"]
            .as_array()
            .expect("required")
            .iter()
            .any(|r| r == "id")
    );
}

/// Bypass-proofing (Lane B adversarial-review requirement): every tool the
/// dispatcher will actually route (the live `/mcp` `tools/list`) must be
/// classified in the RBAC registry `MCP_TOOL_CLASSES` — `routable ⊆ registry`,
/// STRICT. A routable tool missing from the registry would bypass the class
/// gate on the autumn surface (a privilege-escalation hole), so this test fails
/// on any routable-but-unclassified tool: a new `#[api_doc(mcp)]` handler cannot
/// ship class-ungated.
///
/// The reverse inclusion (registry ⊆ routable) is deliberately NOT asserted
/// (coordinator-directed): the registry is inventory-anchored to all 46 tools
/// upfront (`registry_matches_inventory_exactly` in `tests/security_rbac.rs`
/// pins it to `tests/parity/inventory.json`), while handlers become routable one
/// slice at a time during the autumn port. The registry legitimately contains
/// not-yet-routable classified entries; it never contains a routable
/// unclassified one.
#[tokio::test]
async fn mcp_routable_tools_are_all_classified() {
    use std::collections::BTreeSet;

    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let rpc = json!({ "jsonrpc": "2.0", "id": 42, "method": "tools/list" });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_str(&resp.text()).expect("json");

    let routable: BTreeSet<String> = body["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().expect("tool name").to_string())
        .collect();
    let registered: BTreeSet<String> = rbac::MCP_TOOL_CLASSES
        .iter()
        .map(|(n, _)| (*n).to_string())
        .collect();

    // routable ⊆ registry: no routable tool may lack a class (the escalation
    // hole). Reported precisely so a drift names the offending tools.
    let unclassified: BTreeSet<&String> = routable.difference(&registered).collect();
    assert!(
        unclassified.is_empty(),
        "every live /mcp routable tool must be classified in the RBAC registry \
         (routable-but-unclassified = class-gate bypass); unclassified={unclassified:?}"
    );
    // And every routable name resolves to a concrete class (defensive: None at
    // the dispatch gate must be treated as reject, never allow).
    for name in &routable {
        assert!(
            rbac::tool_access_class(name).is_some(),
            "routable tool {name} has no class"
        );
    }
}

/// SECURITY. The budgetable edge reads forward RAW ARGUMENTS to
/// `AletheiaMcpServer::dispatch_tool_json` with a **hardcoded literal** tool
/// name pinned to the route (e.g. the `get_edge` handler pins `"get_edge"`). A
/// read-gated (`Authorized<ReadClass>`) handler that pinned a WRITE tool's name
/// would route a caller past a read gate into a write tool — privilege
/// escalation. This test makes that impossible: for every pinned entry in
/// `edge_tools::DISPATCH_ROUTED_READ_TOOLS`, the pinned name must (1) be a
/// **routed** tool on `/mcp` (pinned ↔ routed) and (2) resolve in the RBAC
/// registry to **exactly** the [`AccessClass`] of the handler's `Authorized<C>`
/// gate (routed ↔ class). Since every dispatch-routed read is `ReadClass`, any
/// drift to a non-Read (e.g. write) pinned name fails here.
#[tokio::test]
async fn dispatch_pinned_names_match_routed_class() {
    use aletheia_server::edge_tools::{ALL_DISPATCH_ROUTED, DISPATCH_ROUTED_READ_TOOLS};
    use std::collections::BTreeSet;

    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    // The set of tool names actually routed on `/mcp`.
    let rpc = json!({ "jsonrpc": "2.0", "id": 77, "method": "tools/list" });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_str(&resp.text()).expect("json");
    let routable: BTreeSet<String> = body["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().expect("tool name").to_string())
        .collect();

    // Iterate the SUPERSET `ALL_DISPATCH_ROUTED` so EVERY dispatch_tool_json call
    // site is covered — the budgetable/cursorable slow reads AND the three
    // non-budgetable provenance-hash-chain tools (`audit_export`, `verify_chain`,
    // `export_chain_head`, Issue #3524 PR7) that also pin a literal name.
    assert!(
        !ALL_DISPATCH_ROUTED.is_empty(),
        "the pinned-name table must not be empty"
    );
    for (pinned, class) in ALL_DISPATCH_ROUTED {
        // (1) pinned ↔ routed: the literal a handler forwards must be a real,
        // routed tool name (never a typo or an off-surface name).
        assert!(
            routable.contains(*pinned),
            "pinned dispatch name {pinned:?} is not routed on /mcp: {routable:?}"
        );
        // (2) routed ↔ class: the RBAC-registry class of the pinned name must
        // equal the handler's declared Authorized<C> class. A read-gated
        // handler pinning a write tool's name would trip this (Write != Read).
        assert_eq!(
            rbac::tool_access_class(pinned),
            Some(*class),
            "pinned dispatch name {pinned:?} registry class must equal the handler's \
             Authorized<C> class ({class:?}) — a read handler pinning a write name is escalation"
        );
    }

    // Subset invariant: every budgetable-routed entry must appear in the superset,
    // so `DISPATCH_ROUTED_READ_TOOLS` and `ALL_DISPATCH_ROUTED` can never drift
    // (a budgetable read added to one but not the other would fail here).
    for entry in DISPATCH_ROUTED_READ_TOOLS {
        assert!(
            ALL_DISPATCH_ROUTED.contains(entry),
            "DISPATCH_ROUTED_READ_TOOLS entry {entry:?} is missing from the \
             ALL_DISPATCH_ROUTED superset — the two tables have drifted"
        );
    }
}

#[tokio::test]
async fn mcp_tools_call_get_node_matches_http() {
    let (db, store, node_id) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let http_body = client
        .get(&format!("/nodes/{}", node_id.as_u64()))
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .send()
        .await
        .text();

    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "get_node", "arguments": { "id": node_id.as_u64() } }
    });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    let body: Value = serde_json::from_str(&resp.text()).expect("json");
    assert_eq!(body["id"], 2);
    let result = &body["result"];
    assert_eq!(result["isError"], false, "tools/call must succeed: {body}");
    let text = result["content"][0]["text"].as_str().expect("tool text");
    let tool_json: Value = serde_json::from_str(text).expect("tool text JSON");
    let http_json: Value = serde_json::from_str(&http_body).expect("http JSON");
    assert_eq!(
        tool_json, http_json,
        "tools/call payload must equal HTTP GET"
    );
}

#[tokio::test]
async fn mcp_catalog_requires_token() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let rpc = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" });
    let resp = client.post("/mcp").json(&rpc).send().await;
    assert_eq!(
        resp.status.as_u16(),
        401,
        "unauthenticated /mcp catalog must be rejected: {}",
        resp.text()
    );
}
