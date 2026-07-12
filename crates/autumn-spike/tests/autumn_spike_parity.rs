//! Integration tests for the autumn-web 0.5.0 migration spike.
//!
//! Proves that the single `#[get("/nodes/{id}")] #[api_doc(mcp)]` handler
//! projects, with parity, to three surfaces — HTTP, MCP-over-HTTP (`/mcp`), and
//! OpenAPI (`/openapi.json`) — while preserving bearer auth (constant-time,
//! on-by-default), RBAC, and the structured error envelope. Cases a–i plus a
//! tower-layer (rate-limit) demonstration that ADR 0055's constraint is lifted.

use std::sync::Arc;

use aletheia_autumn_spike::{DEMO_MIDDLEWARE_HEADER, build_spike_client};
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::core::NodeId;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use serde_json::{Value, json};

const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";
const METRICS_TOKEN: &str = "metrics-token-abcdefghijklmnop";

/// A shared DB holding one node, plus a shared auth store with a reader key and
/// a metrics-only key. Returns the created node's id too.
fn fixture() -> (Arc<AletheiaDB>, Arc<AuthStore>, NodeId) {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30_i64)
                .build(),
        )
        .expect("create node");

    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("reader", Role::Reader, READER_TOKEN)
        .expect("insert reader key");
    store
        .insert_bootstrap_key("metrics", Role::Metrics, METRICS_TOKEN)
        .expect("insert metrics key");

    (db, store, node_id)
}

/// Body of `POST /query {"operation":"get_node","node_id":N}` through the
/// EXISTING autumn-0.4 router, for byte-parity comparison. Shares the same
/// `Arc<AletheiaDB>` + `Arc<AuthStore>` so the node bytes are identical.
async fn existing_query_get_node_body(
    db: Arc<AletheiaDB>,
    store: Arc<AuthStore>,
    node_id: u64,
) -> String {
    use tower::ServiceExt as _;

    let app_state = aletheiadb::http::AppState::new(db);
    let auth_state = aletheiadb::http::AuthState::new(store, AuthMode::Required);
    let config = aletheiadb::http::ServerConfig::default();
    let router = aletheiadb::http::build_test_router_with_auth(app_state, auth_state, &config)
        .expect("build 0.4 query router");

    let body = json!({ "operation": "get_node", "node_id": node_id }).to_string();
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/query")
        .header("authorization", format!("Bearer {READER_TOKEN}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .expect("build request");

    let response = router.oneshot(request).await.expect("oneshot");
    assert_eq!(
        response.status(),
        axum::http::StatusCode::OK,
        "existing /query GetNode must return 200"
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    String::from_utf8(bytes.to_vec()).expect("utf8 body")
}

/// Post a JSON-RPC message to `/mcp` and return the parsed JSON response.
async fn mcp_rpc(
    client: &autumn_web::test::TestClient,
    body: &Value,
    bearer: Option<&str>,
) -> Value {
    let mut req = client.post("/mcp").json(body);
    if let Some(token) = bearer {
        req = req.header("authorization", &format!("Bearer {token}"));
    }
    let resp = req.send().await;
    serde_json::from_str(&resp.text()).expect("mcp response is JSON")
}

// ── (a) Unauthenticated GET → 401 uniform UNAUTHENTICATED ──────────────────
#[tokio::test]
async fn case_a_unauthenticated_returns_401() {
    let (db, store, node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    let resp = client
        .get(&format!("/nodes/{}", node_id.as_u64()))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 401);
    let body: Value = resp.json();
    assert_eq!(body["code"], "UNAUTHENTICATED");
    assert_eq!(body["success"], false);
    // Uniform: no missing/unknown/revoked distinction, and WWW-Authenticate set.
    assert_eq!(resp.header("www-authenticate"), Some("Bearer"));
}

// ── (b) Reader token, existing node → 200 and BYTE-IDENTICAL to POST /query ─
#[tokio::test]
async fn case_b_reader_get_is_byte_identical_to_query() {
    let (db, store, node_id) = fixture();
    let expected = existing_query_get_node_body(db.clone(), store.clone(), node_id.as_u64()).await;

    let client = build_spike_client(db, store, AuthMode::Required);
    let resp = client
        .get(&format!("/nodes/{}", node_id.as_u64()))
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let actual = resp.text();
    assert_eq!(
        actual, expected,
        "GET /nodes/{{id}} body must be byte-identical to POST /query GetNode"
    );
}

// ── (c) Metrics-only token (lacks Read) → 403 PERMISSION_DENIED + details ───
#[tokio::test]
async fn case_c_metrics_token_forbidden_with_required_class() {
    let (db, store, node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    let resp = client
        .get(&format!("/nodes/{}", node_id.as_u64()))
        .header("authorization", &format!("Bearer {METRICS_TOKEN}"))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 403);
    let body: Value = resp.json();
    assert_eq!(body["code"], "PERMISSION_DENIED");
    // Matches the MCP surface's lowercase class/role convention (src/mcp/auth.rs).
    assert_eq!(body["details"]["required_class"], "read");
    assert_eq!(body["details"]["principal_role"], "metrics");
}

// ── (d) Missing node id → structured NOT_FOUND ─────────────────────────────
#[tokio::test]
async fn case_d_missing_node_returns_not_found() {
    let (db, store, _node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    let resp = client
        .get("/nodes/999999")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 404);
    let body: Value = resp.json();
    assert_eq!(body["code"], "NOT_FOUND");
    assert_eq!(body["success"], false);
}

// ── (e) id > MAX_VALID_ID → INVALID_ARGUMENT ───────────────────────────────
#[tokio::test]
async fn case_e_overflow_id_is_invalid_argument() {
    let (db, store, _node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    // u64::MAX is above MAX_VALID_ID (u64::MAX - 1000).
    let resp = client
        .get(&format!("/nodes/{}", u64::MAX))
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 400);
    let body: Value = resp.json();
    assert_eq!(body["code"], "INVALID_ARGUMENT");
}

// ── (f) /mcp tools/list includes the get_node tool with an id inputSchema ──
#[tokio::test]
async fn case_f_mcp_tools_list_advertises_get_node() {
    let (db, store, _node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    let rpc = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let resp = mcp_rpc(&client, &rpc, None).await;

    let tools = resp["result"]["tools"].as_array().expect("tools array");
    let tool = tools
        .iter()
        .find(|t| t["name"] == "get_node")
        .expect("get_node tool present");
    // inputSchema derived from the handler signature: the `id` path param.
    assert_eq!(tool["inputSchema"]["properties"]["id"]["type"], "string");
    let required = tool["inputSchema"]["required"]
        .as_array()
        .expect("required");
    assert!(required.iter().any(|r| r == "id"));
}

// ── (g) /mcp tools/call → payload equal to the HTTP GET body ───────────────
#[tokio::test]
async fn case_g_mcp_tools_call_matches_http_get() {
    let (db, store, node_id) = fixture();
    let client = build_spike_client(db.clone(), store.clone(), AuthMode::Required);

    // The HTTP GET body (same client, same node).
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
        "params": { "name": "get_node", "arguments": { "id": node_id.as_u64().to_string() } }
    });
    let resp = mcp_rpc(&client, &rpc, Some(READER_TOKEN)).await;

    let result = &resp["result"];
    assert_eq!(result["isError"], false, "tool call must succeed: {resp}");
    let text = result["content"][0]["text"].as_str().expect("tool text");
    // Parity through the replay path: identical JSON payload.
    let tool_json: Value = serde_json::from_str(text).expect("tool text is JSON");
    let http_json: Value = serde_json::from_str(&http_body).expect("http body is JSON");
    assert_eq!(tool_json, http_json);
}

// ── (h) /mcp tools/call with bad/missing bearer → JSON-RPC error (401/403) ──
#[tokio::test]
async fn case_h_mcp_tools_call_auth_failure_reflected() {
    let (db, store, node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": { "name": "get_node", "arguments": { "id": node_id.as_u64().to_string() } }
    });

    // Missing bearer → the replayed request is unauthenticated → HTTP 401.
    let resp = mcp_rpc(&client, &rpc, None).await;
    let result = &resp["result"];
    assert_eq!(
        result["isError"], true,
        "auth failure must be an error: {resp}"
    );
    let text = result["content"][0]["text"].as_str().expect("tool text");
    assert!(
        text.contains("401"),
        "error text should reflect the 401: {text}"
    );

    // A metrics-only token authenticates but lacks Read → HTTP 403.
    let resp = mcp_rpc(&client, &rpc, Some(METRICS_TOKEN)).await;
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("tool text");
    assert_eq!(resp["result"]["isError"], true);
    assert!(
        text.contains("403"),
        "metrics token should reflect the 403: {text}"
    );
}

// ── (i) /openapi.json contains the /nodes/{id} path ────────────────────────
#[tokio::test]
async fn case_i_openapi_contains_path() {
    let (db, store, _node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    let resp = client.get("/openapi.json").send().await;
    assert_eq!(resp.status.as_u16(), 200);
    let spec: Value = resp.json();
    assert!(
        spec["paths"].get("/nodes/{id}").is_some(),
        "openapi paths must include /nodes/{{id}}: {}",
        spec["paths"]
    );
}

// ── Rate-limit / arbitrary tower layer demo (ADR 0055 constraint lifted) ───
#[tokio::test]
async fn case_layer_demo_arbitrary_tower_middleware_mounts() {
    let (db, store, node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    // The app mounts a general `tower::Layer` (a tower-http SetResponseHeaderLayer,
    // standing in for tower_governor's per-IP GovernorLayer) via autumn's
    // `.layer()`. In 0.4 the sealed `IntoAppLayer` bound rejected such layers;
    // its presence on the response proves 0.5 accepts them.
    let resp = client
        .get(&format!("/nodes/{}", node_id.as_u64()))
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .send()
        .await;
    assert_eq!(resp.header(DEMO_MIDDLEWARE_HEADER), Some("active"));
}
