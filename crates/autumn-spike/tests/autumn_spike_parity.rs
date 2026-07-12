//! Integration tests for the autumn-web 0.5.0 migration spike.
//!
//! Proves that the single `#[get("/nodes/{id}")] #[api_doc(mcp)]` handler
//! projects, with parity, to three surfaces — HTTP, MCP-over-HTTP (`/mcp`), and
//! OpenAPI (`/openapi.json`) — while preserving bearer auth (constant-time,
//! on-by-default), RBAC, and the structured error envelope. Both **success**
//! and **error** bodies are asserted byte-identical to the existing
//! `POST /query` GetNode surface (`build_test_router_with_auth`), the
//! per-request `trace_id` field stripped before comparison.

use std::sync::Arc;

use aletheia_autumn_spike::{DEMO_MIDDLEWARE_HEADER, build_spike_client};
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::core::NodeId;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use autumn_web::test::TestClient;
use serde_json::{Value, json};

const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";
const METRICS_TOKEN: &str = "metrics-token-abcdefghijklmnop";
const EMBEDDING: &[f32] = &[0.10, 0.20, 0.30, 0.40];

/// A shared DB holding one node **with a vector property** (Issue #3524 review:
/// exercise the non-eliding HTTP path), plus a shared auth store with a reader
/// key and a metrics-only key. Returns the created node's id.
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
        .insert_bootstrap_key("reader", Role::Reader, READER_TOKEN)
        .expect("insert reader key");
    store
        .insert_bootstrap_key("metrics", Role::Metrics, METRICS_TOKEN)
        .expect("insert metrics key");

    (db, store, node_id)
}

/// Drop the per-request `trace_id` (present only when OTel is active; differs
/// per request) so two responses can be compared for byte-parity.
fn strip_trace(mut v: Value) -> Value {
    if let Some(obj) = v.as_object_mut() {
        obj.remove("trace_id");
    }
    v
}

/// `GET /nodes/{id}` through the spike router → `(status, body_json)`.
async fn spike_get(client: &TestClient, id: &str, bearer: Option<&str>) -> (u16, Value) {
    let mut req = client.get(&format!("/nodes/{id}"));
    if let Some(token) = bearer {
        req = req.header("authorization", &format!("Bearer {token}"));
    }
    let resp = req.send().await;
    let status = resp.status.as_u16();
    let body: Value = serde_json::from_str(&resp.text()).expect("spike body is JSON");
    (status, strip_trace(body))
}

/// `POST /query {"operation":"get_node","node_id":N}` through the EXISTING
/// autumn-0.4 router → `(status, body_json)`. Shares the same `Arc<AletheiaDB>`
/// + `Arc<AuthStore>` so node bytes and error messages are identical.
async fn query_get_node(
    db: Arc<AletheiaDB>,
    store: Arc<AuthStore>,
    node_id: u64,
    bearer: Option<&str>,
) -> (u16, Value) {
    use tower::ServiceExt as _;

    let app_state = aletheiadb::http::AppState::new(db);
    let auth_state = aletheiadb::http::AuthState::new(store, AuthMode::Required);
    let config = aletheiadb::http::ServerConfig::default();
    let router = aletheiadb::http::build_test_router_with_auth(app_state, auth_state, &config)
        .expect("build 0.4 query router");

    let body = json!({ "operation": "get_node", "node_id": node_id }).to_string();
    let mut builder = axum::http::Request::builder()
        .method("POST")
        .uri("/query")
        .header("content-type", "application/json");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = builder
        .body(axum::body::Body::from(body))
        .expect("build request");

    let response = router.oneshot(request).await.expect("oneshot");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: Value = serde_json::from_slice(&bytes).expect("query body is JSON");
    (status, strip_trace(json))
}

// ── (a) Unauthenticated GET → 401 uniform UNAUTHENTICATED; parity vs /query ─
#[tokio::test]
async fn case_a_unauthenticated_returns_401_parity() {
    let (db, store, node_id) = fixture();
    let client = build_spike_client(db.clone(), store.clone(), AuthMode::Required);

    let resp = client
        .get(&format!("/nodes/{}", node_id.as_u64()))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 401);
    assert_eq!(resp.header("www-authenticate"), Some("Bearer"));

    let (s_status, s_body) = spike_get(&client, &node_id.as_u64().to_string(), None).await;
    let (q_status, q_body) = query_get_node(db, store, node_id.as_u64(), None).await;
    assert_eq!(s_status, 401);
    assert_eq!(s_status, q_status);
    assert_eq!(s_body["code"], "UNAUTHENTICATED");
    assert_eq!(
        s_body, q_body,
        "unauth error body must be byte-identical to POST /query"
    );
}

// ── (b) Reader token → 200, body BYTE-IDENTICAL to POST /query (with vector) ─
#[tokio::test]
async fn case_b_reader_get_is_byte_identical_to_query() {
    let (db, store, node_id) = fixture();
    let client = build_spike_client(db.clone(), store.clone(), AuthMode::Required);

    let (s_status, s_body) =
        spike_get(&client, &node_id.as_u64().to_string(), Some(READER_TOKEN)).await;
    let (q_status, q_body) = query_get_node(db, store, node_id.as_u64(), Some(READER_TOKEN)).await;
    assert_eq!(s_status, 200);
    assert_eq!(s_status, q_status);
    assert_eq!(s_body, q_body, "success body must equal POST /query");
    // The `{success,data}` envelope is reused, and the vector is returned RAW
    // (this HTTP path intentionally does NOT elide — #3220 elision lives only on
    // the MCP-stdio node serializer).
    assert_eq!(s_body["success"], true);
    assert!(
        s_body["data"]["properties"]["embedding"].is_array(),
        "embedding must be a raw float array, not an elided descriptor: {}",
        s_body["data"]["properties"]["embedding"]
    );
}

// ── (c) Metrics-only token (lacks Read) → 403 PERMISSION_DENIED; parity ─────
#[tokio::test]
async fn case_c_metrics_forbidden_parity() {
    let (db, store, node_id) = fixture();
    let client = build_spike_client(db.clone(), store.clone(), AuthMode::Required);

    let (s_status, s_body) =
        spike_get(&client, &node_id.as_u64().to_string(), Some(METRICS_TOKEN)).await;
    let (q_status, q_body) = query_get_node(db, store, node_id.as_u64(), Some(METRICS_TOKEN)).await;
    assert_eq!(s_status, 403);
    assert_eq!(s_status, q_status);
    assert_eq!(s_body["code"], "PERMISSION_DENIED");
    // HTTP's flat envelope carries NO `details` (it is thinner than MCP's #3234
    // envelope — see the spike doc). Parity holds by construction.
    assert!(s_body.get("details").is_none());
    assert_eq!(
        s_body, q_body,
        "403 error body must be byte-identical to POST /query"
    );
}

// ── (d) Missing node → structured NOT_FOUND; parity ────────────────────────
#[tokio::test]
async fn case_d_missing_node_not_found_parity() {
    let (db, store, _node_id) = fixture();
    let client = build_spike_client(db.clone(), store.clone(), AuthMode::Required);

    let (s_status, s_body) = spike_get(&client, "999999", Some(READER_TOKEN)).await;
    let (q_status, q_body) = query_get_node(db, store, 999_999, Some(READER_TOKEN)).await;
    assert_eq!(s_status, 404);
    assert_eq!(s_status, q_status);
    assert_eq!(s_body["success"], false);
    assert_eq!(
        s_body, q_body,
        "404 error body must be byte-identical to POST /query"
    );
}

// ── (e) id > MAX_VALID_ID → INVALID_ARGUMENT; parity ───────────────────────
#[tokio::test]
async fn case_e_overflow_id_invalid_argument_parity() {
    let (db, store, _node_id) = fixture();
    let client = build_spike_client(db.clone(), store.clone(), AuthMode::Required);

    let (s_status, s_body) = spike_get(&client, &u64::MAX.to_string(), Some(READER_TOKEN)).await;
    let (q_status, q_body) = query_get_node(db, store, u64::MAX, Some(READER_TOKEN)).await;
    assert_eq!(s_status, 400);
    assert_eq!(s_status, q_status);
    assert_eq!(
        s_body, q_body,
        "400 overflow body must be byte-identical to POST /query"
    );
}

// ── (e2) Non-numeric id → structured INVALID_ARGUMENT envelope (not axum's
//         default plain-text 400) ─────────────────────────────────────────
#[tokio::test]
async fn case_e2_non_numeric_id_structured_envelope() {
    let (db, store, _node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    let (status, body) = spike_get(&client, "abc", Some(READER_TOKEN)).await;
    assert_eq!(status, 400);
    // Flat envelope, not axum's plain-text default.
    assert_eq!(body["success"], false);
    assert!(
        body["error"].as_str().unwrap().contains("invalid node id"),
        "structured body expected: {body}"
    );
}

// ── (f) /mcp tools/list (authenticated) advertises get_node + id schema ────
#[tokio::test]
async fn case_f_mcp_tools_list_advertises_get_node() {
    let (db, store, _node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    let rpc = json!({ "jsonrpc": "2.0", "id": 11, "method": "tools/list" });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_str(&resp.text()).expect("json");
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 11, "request id must be echoed");

    let tools = body["result"]["tools"].as_array().expect("tools array");
    let tool = tools
        .iter()
        .find(|t| t["name"] == "get_node")
        .expect("get_node tool present");
    assert_eq!(tool["inputSchema"]["properties"]["id"]["type"], "string");
    let required = tool["inputSchema"]["required"]
        .as_array()
        .expect("required");
    assert!(required.iter().any(|r| r == "id"));
}

// ── (f-neg) /mcp catalog requires a credential (auth-on-by-default parity) ─
#[tokio::test]
async fn case_f_neg_tools_list_requires_token() {
    let (db, store, _node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    let rpc = json!({ "jsonrpc": "2.0", "id": 12, "method": "tools/list" });
    // No bearer → the native RequireApiToken layer gating /mcp rejects.
    let resp = client.post("/mcp").json(&rpc).send().await;
    assert_eq!(
        resp.status.as_u16(),
        401,
        "unauthenticated /mcp catalog must be rejected: {}",
        resp.text()
    );
}

// ── (g) /mcp tools/call → payload equal to the HTTP GET body (vector incl.) ─
#[tokio::test]
async fn case_g_mcp_tools_call_matches_http_get() {
    let (db, store, node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    let http_body = client
        .get(&format!("/nodes/{}", node_id.as_u64()))
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .send()
        .await
        .text();

    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 13,
        "method": "tools/call",
        "params": { "name": "get_node", "arguments": { "id": node_id.as_u64().to_string() } }
    });
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&rpc)
        .send()
        .await;
    let body: Value = serde_json::from_str(&resp.text()).expect("json");
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 13, "request id must be echoed");

    let result = &body["result"];
    assert_eq!(result["isError"], false, "tool call must succeed: {body}");
    let text = result["content"][0]["text"].as_str().expect("tool text");
    let tool_json: Value = serde_json::from_str(text).expect("tool text is JSON");
    let http_json: Value = serde_json::from_str(&http_body).expect("http body is JSON");
    assert_eq!(
        tool_json, http_json,
        "tools/call payload must equal HTTP GET"
    );
    // Vector still raw through the replay path.
    assert!(tool_json["data"]["properties"]["embedding"].is_array());
}

// ── (h) /mcp tools/call auth failures → #3234 code / envelope rejection ─────
#[tokio::test]
async fn case_h_mcp_tools_call_auth_failure_reflected() {
    let (db, store, node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    let call = |id: i64| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": "get_node", "arguments": { "id": node_id.as_u64().to_string() } }
        })
    };

    // Metrics token: passes the /mcp envelope, fails RBAC on replay → the tool
    // result carries the underlying handler's flat error body → parse #3234 code.
    let resp = client
        .post("/mcp")
        .header("authorization", &format!("Bearer {METRICS_TOKEN}"))
        .json(&call(14))
        .send()
        .await;
    let body: Value = serde_json::from_str(&resp.text()).expect("json");
    assert_eq!(body["id"], 14);
    assert_eq!(
        body["result"]["isError"], true,
        "must be a tool error: {body}"
    );
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .expect("tool text");
    // Text is `handler returned HTTP 403: {<flat error body>}` — parse the JSON.
    let start = text.find('{').expect("embedded error json");
    let inner: Value = serde_json::from_str(&text[start..]).expect("inner error json");
    assert_eq!(
        inner["code"], "PERMISSION_DENIED",
        "tool error must carry the #3234 code, not a substring: {text}"
    );

    // No token: the native /mcp gate rejects before any tool dispatch → 401.
    let resp = client.post("/mcp").json(&call(15)).send().await;
    assert_eq!(
        resp.status.as_u16(),
        401,
        "unauthenticated tools/call must be rejected at the envelope: {}",
        resp.text()
    );
}

// ── (i) /openapi.json: /nodes/{id} path, id param, 200 → ApiResponse ───────
#[tokio::test]
async fn case_i_openapi_documents_the_route() {
    let (db, store, _node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    let resp = client.get("/openapi.json").send().await;
    assert_eq!(resp.status.as_u16(), 200);
    let spec: Value = resp.json();

    let op = &spec["paths"]["/nodes/{id}"]["get"];
    assert!(
        !op.is_null(),
        "GET /nodes/{{id}} must be documented: {}",
        spec["paths"]
    );

    // `id` path param, present + required.
    let params = op["parameters"].as_array().expect("parameters");
    let id_param = params
        .iter()
        .find(|p| p["name"] == "id" && p["in"] == "path")
        .expect("id path param");
    assert_eq!(id_param["required"], true);

    // 200 response references the `{success,data}` ApiResponse envelope.
    assert_eq!(
        op["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ApiResponse"
    );
    assert!(
        spec["components"]["schemas"]["ApiResponse"].is_object(),
        "ApiResponse component must be present"
    );
}

// ── Arbitrary tower-layer demo (ADR 0055 sealed-IntoAppLayer bound lifted) ──
#[tokio::test]
async fn case_arbitrary_tower_layer_mounts() {
    let (db, store, node_id) = fixture();
    let client = build_spike_client(db, store, AuthMode::Required);

    // The app mounts a general `tower::Layer` (a tower-http SetResponseHeaderLayer)
    // via autumn's `.layer()`. In 0.4 the sealed `IntoAppLayer` bound rejected
    // such layers (ADR 0055); its response header proves 0.5 accepts any
    // `tower::Layer<Route>` — the same shape tower_governor's per-IP GovernorLayer
    // has, so HTTP-layer rate limiting is now mountable.
    let resp = client
        .get(&format!("/nodes/{}", node_id.as_u64()))
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .send()
        .await;
    assert_eq!(resp.header(DEMO_MIDDLEWARE_HEADER), Some("active"));
}
