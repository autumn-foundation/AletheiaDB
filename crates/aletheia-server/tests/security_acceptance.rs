//! B4 wiring acceptance suite (Issue #3524) — the seven non-regression ACs
//! (a–g) plus the attack cases, driven against the assembled autumn-web 0.5
//! server (`build_server_client*`) with the custom `/mcp` security gate, the
//! resource-limited read path, and the class-parameterized `Authorized<C>`
//! extractor.
//!
//! TDD note: every AC was written **attack-case-first** (the failing/negative
//! case precedes the positive), so a regression that reopens a hole fails here.
//!
//! - (a) startup refusal ....... `startup_*`
//! - (b) per-route + per-tool RBAC + extractor↔inventory conformance
//! - (c) constant-time verify ... `constant_time_*`
//! - (d) credentials never logged `credentials_never_*`
//! - (e) MCP catalog closed ..... `mcp_catalog_*`
//! - (f) structured error contract `error_contract_*`
//! - (g) revocation immediate ... `revocation_*`
//! - attack cases ............... `attack_*`

use std::sync::Arc;

use aletheia_server::security::resource_limits::{byte_cap_error, timeout_error};
use aletheia_server::{
    SecurityConfig, build_server_client, build_server_client_with_config, try_build_server_client,
};
use aletheiadb::auth::{AccessClass, AuthMode, AuthStore, Role};
use aletheiadb::core::NodeId;
use aletheiadb::http::AletheiaHttpError;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use autumn_web::test::TestClient;
use serde_json::{Value, json};

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";
const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";
const METRICS_TOKEN: &str = "metrics-token-abcdefghijklmnop";
const WRITER_TOKEN: &str = "writer-token-abcdefghijklmnop";
const EMBEDDING: &[f32] = &[0.10, 0.20, 0.30, 0.40];

// ── fixtures ────────────────────────────────────────────────────────────────

/// Shared DB (one `Person` node) + a store with admin/reader/metrics/writer keys.
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
    (db, keyed_store(), node_id)
}

fn keyed_store() -> Arc<AuthStore> {
    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("admin", Role::Admin, ADMIN_TOKEN)
        .expect("admin key");
    store
        .insert_bootstrap_key("reader", Role::Reader, READER_TOKEN)
        .expect("reader key");
    store
        .insert_bootstrap_key("metrics", Role::Metrics, METRICS_TOKEN)
        .expect("metrics key");
    store
        .insert_bootstrap_key("writer", Role::Writer, WRITER_TOKEN)
        .expect("writer key");
    store
}

// ── HTTP drivers ─────────────────────────────────────────────────────────────

async fn get_bearer(client: &TestClient, uri: &str, token: Option<&str>) -> (u16, Value) {
    let mut req = client.get(uri);
    if let Some(t) = token {
        req = req.header("authorization", &format!("Bearer {t}"));
    }
    let resp = req.send().await;
    body_of(resp).await
}

async fn get_header(client: &TestClient, uri: &str, name: &str, value: &str) -> (u16, Value) {
    let resp = client.get(uri).header(name, value).send().await;
    body_of(resp).await
}

async fn body_of(resp: autumn_web::test::TestResponse) -> (u16, Value) {
    let status = resp.status.as_u16();
    let text = resp.text();
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, body)
}

// ── /mcp drivers ─────────────────────────────────────────────────────────────

/// POST a JSON-RPC request to `/mcp` with an optional bearer token; returns the
/// **HTTP** status and the body (which is the JSON-RPC envelope on success, or
/// the AletheiaDB structured envelope on an envelope-level auth/authz refusal).
async fn mcp_post(client: &TestClient, rpc: &Value, token: Option<&str>) -> (u16, Value) {
    let mut req = client.post("/mcp");
    if let Some(t) = token {
        req = req.header("authorization", &format!("Bearer {t}"));
    }
    let resp = req.json(rpc).send().await;
    body_of(resp).await
}

fn rpc_initialize() -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {} } })
}
fn rpc_tools_list() -> Value {
    json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })
}
fn rpc_tools_call(name: &str, args: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": name, "arguments": args } })
}

/// Render an [`AletheiaHttpError`] through its shared `IntoResponse` — the exact
/// envelope both the HTTP and `/mcp` surfaces emit — into `(status, body)`.
async fn render_error(err: AletheiaHttpError) -> (u16, Value) {
    use axum::response::IntoResponse as _;
    let resp = err.into_response();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read error body");
    (
        status,
        serde_json::from_slice(&bytes).expect("error body JSON"),
    )
}

// ════════════════════════════════════════════════════════════════════════════
// AC (a) — startup refusal (attack case first: required + empty store)
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn startup_refuses_required_mode_with_empty_store() {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let empty = Arc::new(AuthStore::new());
    let cfg = SecurityConfig::new(empty, AuthMode::Required);
    let result = try_build_server_client(db, cfg);
    assert!(
        result.is_err(),
        "required mode with zero credentials must refuse to start"
    );
    let msg = result.err().unwrap();
    assert!(
        msg.contains("authentication is required"),
        "refusal message guides the operator: {msg}"
    );
}

#[tokio::test]
async fn startup_ok_required_mode_with_a_key() {
    let (db, store, _) = fixture();
    let cfg = SecurityConfig::new(store, AuthMode::Required);
    assert!(try_build_server_client(db, cfg).is_ok());
}

#[tokio::test]
async fn startup_ok_anonymous_mode_even_with_empty_store() {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let empty = Arc::new(AuthStore::new());
    let cfg = SecurityConfig::new(empty, AuthMode::Anonymous);
    assert!(
        try_build_server_client(db, cfg).is_ok(),
        "anonymous is an explicit opt-in and always starts"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// AC (b) — per-route + per-MCP-tool RBAC, and extractor↔inventory conformance
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn rbac_reader_reads_ok_but_admin_route_forbidden() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    // Read tool: reader (allows Read) → 200.
    let (status, _) = get_bearer(&client, "/nodes?label=Person", Some(READER_TOKEN)).await;
    assert_eq!(status, 200, "reader may read");

    // Admin route: reader (denies Admin) → 403 PERMISSION_DENIED.
    let (status, body) = get_bearer(&client, "/admin/keys", Some(READER_TOKEN)).await;
    assert_eq!(status, 403, "reader may not reach the admin surface");
    assert_eq!(body["code"], "PERMISSION_DENIED");
    assert_ne!(
        body["retriable"],
        json!(true),
        "authz denial is not retriable"
    );
}

#[tokio::test]
async fn rbac_metrics_token_only_reaches_metrics_class() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    // /status is MetricsClass → metrics token 200.
    let (status, _) = get_bearer(&client, "/status", Some(METRICS_TOKEN)).await;
    assert_eq!(status, 200, "metrics may probe /status");

    // /nodes is ReadClass → metrics (denies Read) 403.
    let (status, body) = get_bearer(&client, "/nodes", Some(METRICS_TOKEN)).await;
    assert_eq!(status, 403, "metrics may not read graph data");
    assert_eq!(body["code"], "PERMISSION_DENIED");

    // /admin/keys is AdminClass → metrics 403.
    let (status, _) = get_bearer(&client, "/admin/keys", Some(METRICS_TOKEN)).await;
    assert_eq!(status, 403, "metrics may not administer keys");
}

/// F4 attack case: a metrics token calling a **read** tool over `/mcp` is
/// refused at the envelope with `PERMISSION_DENIED` + `{required_class,
/// principal_role}` details — mapped from the RBAC `Denied` (ruling F4).
#[tokio::test]
async fn rbac_metrics_token_denied_read_tool_over_mcp_with_details() {
    let (db, store, node_id) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let rpc = rpc_tools_call("get_node", json!({ "id": node_id.as_u64() }));
    let (status, body) = mcp_post(&client, &rpc, Some(METRICS_TOKEN)).await;

    assert_eq!(status, 403, "metrics→get_node over /mcp is refused: {body}");
    assert_eq!(body["code"], "PERMISSION_DENIED");
    assert_eq!(body["retriable"], false);
    assert_eq!(body["details"]["required_class"], "read");
    assert_eq!(body["details"]["principal_role"], "metrics");
}

/// Extractor-class ↔ inventory conformance (load-bearing): for every routable
/// MCP tool, the class its handler's `Authorized<C>` actually **enforces**
/// (observed behaviorally — a role that permits the class succeeds, a role that
/// does not is refused) equals the class the inventory/registry assigns it. This
/// passes trivially for today's three read tools but would catch a future write
/// tool mistakenly shipped as `Authorized<ReadClass>`.
#[tokio::test]
async fn rbac_routable_tool_extractor_class_matches_registry() {
    use aletheia_server::security::rbac;

    let (db, store, node_id) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    // The routable tools and a representative call for each.
    let calls: &[(&str, &str)] = &[
        ("get_node", "/nodes/"),
        ("list_nodes", "/nodes"),
        ("count_nodes", "/nodes/count"),
    ];

    for (tool, base) in calls {
        let registry_class = rbac::tool_access_class(tool).expect("routable tool is classified");
        // All three are Read in this slice; the probe generalizes.
        assert_eq!(
            registry_class,
            AccessClass::Read,
            "{tool} registry class in this slice"
        );

        let uri = if *tool == "get_node" {
            format!("{base}{}", node_id.as_u64())
        } else {
            (*base).to_string()
        };

        // A role that PERMITS the registry class succeeds …
        let (ok_status, _) = get_bearer(&client, &uri, Some(READER_TOKEN)).await;
        assert_eq!(
            ok_status, 200,
            "{tool}: reader permits {registry_class} → 200"
        );
        // … a role that does NOT permit it is refused. Metrics denies Read, so a
        // 403 here confirms the handler enforces Read (not a weaker class).
        let (deny_status, deny_body) = get_bearer(&client, &uri, Some(METRICS_TOKEN)).await;
        assert_eq!(
            deny_status, 403,
            "{tool}: metrics denied {registry_class} → 403 (enforced class matches registry)"
        );
        assert_eq!(deny_body["code"], "PERMISSION_DENIED");
    }
}

// ════════════════════════════════════════════════════════════════════════════
// AC (c) — constant-time verify (usage assertion + timing probe)
// ════════════════════════════════════════════════════════════════════════════

/// The credential store delegates digest comparison to a constant-time
/// primitive. Asserted structurally against the shared `AuthStore` source so a
/// refactor that swaps `ct_eq` for `==` fails CI.
#[test]
fn constant_time_verify_uses_constant_time_primitive() {
    const STORE_SRC: &str = include_str!("../../../src/auth/store.rs");
    assert!(
        STORE_SRC.contains("ConstantTimeEq") && STORE_SRC.contains("ct_eq"),
        "AuthStore digest comparison must use subtle::ConstantTimeEq::ct_eq"
    );
}

#[test]
fn constant_time_verify_accepts_right_rejects_wrong_and_empty() {
    let store = keyed_store();
    assert!(store.verify(READER_TOKEN).is_some(), "right token verifies");
    assert!(
        store.verify("wrong-token").is_none(),
        "wrong token rejected"
    );
    assert!(store.verify("").is_none(), "empty token rejected");
}

/// Optional timing-dispersion probe. `#[ignore]` by default (timing tests are
/// environment-sensitive); the structural usage assertion above is the always-on
/// guarantee.
#[test]
#[ignore = "timing-sensitive; run explicitly with --ignored"]
fn constant_time_timing_probe() {
    use std::time::Instant;
    let store = keyed_store();
    let sample = |tok: &str| {
        let start = Instant::now();
        for _ in 0..2_000 {
            let _ = store.verify(tok);
        }
        start.elapsed()
    };
    // A near-miss (correct length, wrong last char) vs a total mismatch should
    // not differ by more than a loose factor if comparison is constant-time.
    let near = sample("reader-token-abcdefghijklmnoq");
    let far = sample("z");
    let ratio = near.as_secs_f64() / far.as_secs_f64().max(1e-9);
    assert!(
        (0.2..5.0).contains(&ratio),
        "verify timing dispersion suspiciously large: near={near:?} far={far:?}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// AC (d) — credentials never logged / echoed
// ════════════════════════════════════════════════════════════════════════════

/// A distinctive credential presented on a failing request must never appear in
/// the response body (nor, by construction of the uniform 401, be echoed back).
/// Covers both bearer and `x-api-key` transports and 401/403/200 responses.
#[tokio::test]
async fn credentials_never_appear_in_response_bodies() {
    let (db, store, node_id) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let secret = "SUPER-SECRET-do-not-log-abcdefghij"; // an unknown (wrong) token

    // Wrong bearer → 401; body must not echo the secret.
    let (status, body) = get_bearer(&client, "/nodes", Some(secret)).await;
    assert_eq!(status, 401);
    assert!(
        !body.to_string().contains(secret),
        "401 body leaked the credential: {body}"
    );

    // Wrong x-api-key → 401; body must not echo the secret.
    let (status, body) = get_header(&client, "/nodes", "x-api-key", secret).await;
    assert_eq!(status, 401);
    assert!(
        !body.to_string().contains(secret),
        "x-api-key leaked: {body}"
    );

    // A VALID reader token on a 403 (admin route) must not be echoed either.
    let (status, body) = get_bearer(&client, "/admin/keys", Some(READER_TOKEN)).await;
    assert_eq!(status, 403);
    assert!(
        !body.to_string().contains(READER_TOKEN),
        "403 body leaked a valid credential: {body}"
    );

    // A successful read must not echo the credential.
    let (status, body) = get_bearer(
        &client,
        &format!("/nodes/{}", node_id.as_u64()),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        !body.to_string().contains(READER_TOKEN),
        "200 body leaked the credential"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// AC (e) — MCP catalog closed (attack: no credential)
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn mcp_catalog_initialize_requires_credential() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let (status, _) = mcp_post(&client, &rpc_initialize(), None).await;
    assert_eq!(status, 401, "unauthenticated initialize must be rejected");
}

#[tokio::test]
async fn mcp_catalog_tools_list_requires_credential() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let (status, _) = mcp_post(&client, &rpc_tools_list(), None).await;
    assert_eq!(status, 401, "unauthenticated tools/list must be rejected");
}

#[tokio::test]
async fn mcp_catalog_tools_call_requires_credential() {
    let (db, store, node_id) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let rpc = rpc_tools_call("get_node", json!({ "id": node_id.as_u64() }));
    let (status, _) = mcp_post(&client, &rpc, None).await;
    assert_eq!(status, 401, "unauthenticated tools/call must be rejected");
}

/// Positive: a valid reader token reaches the catalog and dispatches a read tool.
#[tokio::test]
async fn mcp_catalog_reachable_with_valid_credential() {
    let (db, store, node_id) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let (status, body) = mcp_post(&client, &rpc_tools_list(), Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    assert!(body["result"]["tools"].is_array(), "catalog served: {body}");

    let rpc = rpc_tools_call("get_node", json!({ "id": node_id.as_u64() }));
    let (status, body) = mcp_post(&client, &rpc, Some(READER_TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["isError"], false, "tools/call ok: {body}");
}

/// `x-api-key` reaches the `/mcp` catalog (the gate is credential-transport
/// agnostic). As of the B4 finalize, an `x-api-key`-only `tools/call` also
/// dispatches: the gate normalizes the verified credential onto
/// `authorization: Bearer` before the replay (only `authorization` is in autumn
/// 0.5's `FORWARDED_HEADERS`), so the replayed handler re-verifies it — see
/// `tests/security_mcp_gate.rs::x_api_key_tools_call_authenticates_via_normalization`.
/// This test pins the simpler catalog reach (`tools/list` needs no replay).
#[tokio::test]
async fn mcp_catalog_reachable_with_x_api_key() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let resp = client
        .post("/mcp")
        .header("x-api-key", READER_TOKEN)
        .json(&rpc_tools_list())
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200, "x-api-key opens the catalog");
}

// ════════════════════════════════════════════════════════════════════════════
// AC (f) — structured error contract on BOTH surfaces (code/retriable/details)
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn error_contract_http_401_shape() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let (status, body) = get_bearer(&client, "/nodes", None).await;
    assert_eq!(status, 401);
    assert_eq!(body["code"], "UNAUTHENTICATED");
    assert_eq!(body["success"], false);
    assert_ne!(body["retriable"], json!(true), "auth failure not retriable");
}

#[tokio::test]
async fn error_contract_http_403_shape() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let (status, body) = get_bearer(&client, "/admin/keys", Some(READER_TOKEN)).await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "PERMISSION_DENIED");
    assert_ne!(
        body["retriable"],
        json!(true),
        "authz failure not retriable"
    );
}

/// HTTP 413 end-to-end: a tiny byte budget makes `list_nodes` exceed the
/// `ResultBytes` cap, rendering the shared `RESOURCE_EXHAUSTED` / non-retriable
/// envelope with `details`.
#[tokio::test]
async fn error_contract_http_413_byte_cap() {
    let (db, store, _) = fixture();
    let cfg = SecurityConfig::new(store, AuthMode::Required).with_max_response_bytes(8);
    let client = build_server_client_with_config(db, cfg);

    let (status, body) = get_bearer(&client, "/nodes?label=Person", Some(READER_TOKEN)).await;
    assert_eq!(
        status, 413,
        "8-byte budget forces a payload-too-large: {body}"
    );
    assert_eq!(body["code"], "RESOURCE_EXHAUSTED");
    assert_eq!(body["retriable"], false);
    assert_eq!(body["details"]["dimension"], "result_bytes");
    assert_eq!(body["details"]["limit"], 8);
}

#[tokio::test]
async fn error_contract_mcp_401_shape() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let (status, body) = mcp_post(&client, &rpc_tools_list(), None).await;
    assert_eq!(status, 401);
    assert_eq!(
        body["code"], "UNAUTHENTICATED",
        "/mcp 401 is enveloped: {body}"
    );
    assert_eq!(body["success"], false);
}

#[tokio::test]
async fn error_contract_mcp_403_shape_with_details() {
    let (db, store, node_id) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let rpc = rpc_tools_call("get_node", json!({ "id": node_id.as_u64() }));
    let (status, body) = mcp_post(&client, &rpc, Some(METRICS_TOKEN)).await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "PERMISSION_DENIED");
    assert_eq!(body["retriable"], false);
    assert_eq!(body["details"]["required_class"], "read");
    assert_eq!(body["details"]["principal_role"], "metrics");
}

/// The transient-vs-fault `retriable` matrix on the shared envelope both
/// surfaces reuse: a **read** timeout is 429/retriable, a **write** timeout is
/// 429/non-retriable (may have committed), a byte cap is 413/non-retriable, and
/// an in-flight-capacity rejection is 503/retriable.
#[tokio::test]
async fn error_contract_retriable_matrix_on_shared_envelope() {
    use std::time::Duration;

    let (s, b) = render_error(timeout_error(Duration::from_millis(100), false)).await;
    assert_eq!(s, 429);
    assert_eq!(b["code"], "RESOURCE_EXHAUSTED");
    assert_eq!(b["retriable"], true, "read timeout is retriable");

    let (s, b) = render_error(timeout_error(Duration::from_millis(100), true)).await;
    assert_eq!(s, 429);
    assert_eq!(b["retriable"], false, "write timeout is NOT retriable");

    let (s, b) = render_error(byte_cap_error(1024, 4096)).await;
    assert_eq!(s, 413);
    assert_eq!(b["code"], "RESOURCE_EXHAUSTED");
    assert_eq!(b["retriable"], false);

    let (s, b) = render_error(AletheiaHttpError::InFlightCapacityExceeded { cap: 4 }).await;
    assert_eq!(s, 503);
    assert_eq!(b["code"], "UNAVAILABLE");
    assert_eq!(b["retriable"], true, "capacity overload is retriable");
}

// ════════════════════════════════════════════════════════════════════════════
// AC (g) — revocation is immediate (no cached verify across requests)
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn revocation_takes_effect_on_the_next_request() {
    let (db, store, node_id) = fixture();
    // Mint a fresh reader key through the store.
    let (principal, key) = store
        .create_key("ephemeral", Role::Reader)
        .expect("mint key");
    let client = build_server_client(db, store.clone(), AuthMode::Required);
    let uri = format!("/nodes/{}", node_id.as_u64());

    // Works before revocation.
    let (status, _) = get_bearer(&client, &uri, Some(&key)).await;
    assert_eq!(status, 200, "freshly-minted key reads");

    // Revoke, then the very next request is 401 — proving no per-request cache
    // survives across requests (the layer/extractor re-verify every time).
    assert!(store.revoke(&principal.id).expect("revoke"));
    let (status, body) = get_bearer(&client, &uri, Some(&key)).await;
    assert_eq!(status, 401, "revoked key is rejected immediately: {body}");
    assert_eq!(body["code"], "UNAUTHENTICATED");
}

// ════════════════════════════════════════════════════════════════════════════
// Attack cases (separate negative tests)
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn attack_wrong_token_http_and_mcp_are_401() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let (status, _) = get_bearer(&client, "/nodes", Some("not-a-real-token")).await;
    assert_eq!(status, 401, "HTTP wrong token → 401");
    let (status, _) = mcp_post(&client, &rpc_tools_list(), Some("not-a-real-token")).await;
    assert_eq!(status, 401, "/mcp wrong token → 401");
}

#[tokio::test]
async fn attack_right_token_wrong_class_is_403() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    // Writer may write/read but not administer keys.
    let (status, body) = get_bearer(&client, "/admin/keys", Some(WRITER_TOKEN)).await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "PERMISSION_DENIED");
}

#[tokio::test]
async fn attack_missing_auth_is_401() {
    let (db, store, _) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);
    let (status, _) = get_bearer(&client, "/nodes", None).await;
    assert_eq!(status, 401);
}

/// In-flight cap exhaustion yields the retriable 503 envelope. Driven at the
/// limiter primitive (the wiring the read handler mounts) so it is deterministic
/// without racing real concurrency.
#[tokio::test]
async fn attack_in_flight_cap_exhaustion_is_503() {
    use aletheia_server::security::InFlightLimiter;
    let limiter = InFlightLimiter::new(1);
    let _held = limiter.try_acquire().expect("first slot admitted");
    let err = limiter
        .try_acquire()
        .expect_err("second slot rejected at cap");
    let (status, body) = render_error(err).await;
    assert_eq!(status, 503);
    assert_eq!(body["code"], "UNAVAILABLE");
    assert_eq!(body["retriable"], true);
}
