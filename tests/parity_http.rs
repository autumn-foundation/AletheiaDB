//! HTTP surface PARITY harness (black-box golden pins).
//!
//! This file pins the *current, observed* behavior of the AletheiaDB HTTP
//! server (`src/http/**`) so that a future re-implementation on another web
//! framework must reproduce it **unchanged**. Every assertion here is a
//! contract, not an aspiration: if one fails after a port, the port changed
//! observable behavior and the change must be justified — not the test.
//!
//! Scope: the FULL registered route inventory (exactly 6 routes assembled by
//! `handlers::all_routes()`), the success/error envelopes, the auth contract
//! (401 uniform / 403 role denial), and the per-query + payload resource limits
//! (413 / 422). Driven black-box through the public `build_test_router` /
//! `build_test_router_with_auth` (autumn `TestApp`, Pattern A) plus one
//! real-TCP-socket boot of the production `run_server` (Pattern B) for the
//! request-body-limit 413, which only exists in the production layer stack.
//!
//! Companion machine-readable inventory: `tests/parity/inventory.json`.
//!
//! Run with:
//!   cargo test --features http-server --test parity_http
//!
//! Deliberately NOT re-tested here (already pinned elsewhere; referenced in the
//! inventory to avoid duplication):
//!   - 429 RESOURCE_EXHAUSTED wall-clock timeout: cannot be forced
//!     deterministically over the wire (see `tests/http_query_limits.rs` header
//!     + the `enforce_query_limits` unit tests in `src/http/handlers.rs`).
//!   - OTel `trace_id` body field / `x-trace-id` header population: requires the
//!     `observability`/`otel` features (pinned by `tests/http_otel_tracing.rs`).
//!     Under this file's feature set the field is simply absent — see
//!     `query_error_envelope_has_no_trace_id_without_observability`.

#![cfg(feature = "http-server")]

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aletheiadb::AletheiaDB;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::http::{
    AppState, AuthState, QueryLimitsConfig, RowOverflowPolicy, ServerConfig, build_test_router,
    build_test_router_with_auth, run_server,
};
use autumn_web::test::{TestApp, TestClient};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Fixtures / helpers (mirror the established patterns in tests/http_*.rs)
// ---------------------------------------------------------------------------

/// Anonymous-mode client over the real router (auth disabled).
fn anon_client() -> (TestClient, Arc<AletheiaDB>) {
    let db = Arc::new(AletheiaDB::new().expect("create DB"));
    let state = AppState::new(db.clone());
    let config = ServerConfig::builder()
        .auth_mode(AuthMode::Anonymous)
        .build();
    let router = build_test_router(state, &config).expect("build router");
    (TestApp::from_router(router), db)
}

/// Anonymous-mode client with explicit per-query limits.
fn anon_client_with_limits(limits: QueryLimitsConfig) -> (TestClient, Arc<AletheiaDB>) {
    let db = Arc::new(AletheiaDB::new().expect("create DB"));
    let state = AppState::new(db.clone());
    let config = ServerConfig::builder()
        .auth_mode(AuthMode::Anonymous)
        .query_limits(limits)
        .build();
    let router = build_test_router(state, &config).expect("build router");
    (TestApp::from_router(router), db)
}

/// Required-auth client over the real router, returning the shared key store.
fn required_client() -> (TestClient, Arc<AuthStore>, Arc<AletheiaDB>) {
    let db = Arc::new(AletheiaDB::new().expect("create DB"));
    let state = AppState::new(db.clone());
    let store = Arc::new(AuthStore::new());
    let auth = AuthState::new(store.clone(), AuthMode::Required);
    let config = ServerConfig::default();
    let router = build_test_router_with_auth(state, auth, &config).expect("build router");
    (TestApp::from_router(router), store, db)
}

fn mint(store: &AuthStore, name: &str, role: Role) -> String {
    let (_p, key) = store.create_key(name, role).expect("mint key");
    key.to_string()
}

async fn post_query(client: &TestClient, body: &Value) -> (u16, Value) {
    let resp = client.post("/query").json(body).send().await;
    (
        resp.status.as_u16(),
        serde_json::from_slice::<Value>(&resp.body).unwrap_or(Value::Null),
    )
}

async fn post_query_key(client: &TestClient, key: &str, body: &Value) -> (u16, Value) {
    let resp = client
        .post("/query")
        .header("Authorization", &format!("Bearer {key}"))
        .json(body)
        .send()
        .await;
    (
        resp.status.as_u16(),
        serde_json::from_slice::<Value>(&resp.body).unwrap_or(Value::Null),
    )
}

fn seed(db: &AletheiaDB, n: usize) {
    for i in 0..n {
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("idx", i as i64).build(),
        )
        .expect("seed node");
    }
}

fn find_people() -> Value {
    json!({ "operation": "find_node", "label": "Person" })
}

// ===========================================================================
// Route inventory — the whole surface is exactly 6 routes.
// ===========================================================================

/// PARITY: the registered route inventory is a fixed, known set. A port that
/// adds, drops, or renames a route (or changes its method) fails here.
#[tokio::test]
async fn route_inventory_is_the_known_six() {
    let routes = aletheiadb::http::handlers::all_routes();
    let mut got: Vec<(String, String)> = routes
        .iter()
        .map(|r| (r.method.to_string(), r.path.to_string()))
        .collect();
    got.sort();

    let mut want: Vec<(String, String)> = vec![
        ("GET".into(), "/status".into()),
        ("POST".into(), "/query".into()),
        ("POST".into(), "/admin/keys".into()),
        ("GET".into(), "/admin/keys".into()),
        ("POST".into(), "/admin/keys/revoke".into()),
        ("POST".into(), "/admin/promote".into()),
    ];
    want.sort();

    assert_eq!(
        got, want,
        "HTTP route inventory changed — a framework port must preserve exactly \
         these 6 routes (and update tests/parity/inventory.json)"
    );
}

// ===========================================================================
// GET /status — Metrics class, liveness probe.
// ===========================================================================

/// PARITY: `/status` returns 200 with the exact liveness body. The top-level
/// key set is pinned exactly (`{status}` only) so a port that ADDS a field
/// (e.g. `version`, `uptime`) is caught here, not silently tolerated.
#[tokio::test]
async fn status_success_shape() {
    let (client, _db) = anon_client();
    let resp = client.get("/status").send().await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert_eq!(body, json!({ "status": "healthy" }));

    // Exact top-level key-set pin (an ADDED field breaks this).
    let keys: std::collections::BTreeSet<&str> = body
        .as_object()
        .expect("status body is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from(["status"]),
        "/status success body must carry exactly {{status}}: {body}"
    );
}

/// PARITY: `/status` in required mode with no credential is the uniform 401.
#[tokio::test]
async fn status_requires_credential_in_required_mode() {
    let (client, _store, _db) = required_client();
    let resp = client.get("/status").send().await;
    assert_eq!(resp.status.as_u16(), 401);
    assert_eq!(resp.header("www-authenticate"), Some("Bearer"));
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "UNAUTHENTICATED");
}

// ===========================================================================
// POST /query — success + error envelopes, auth, limits.
// ===========================================================================

/// PARITY: the `/query` success envelope is `{success:true, data:[...]}` with
/// `error`/`truncated` OMITTED when not applicable.
#[tokio::test]
async fn query_success_envelope_shape() {
    let (client, db) = anon_client();
    seed(&db, 3);

    let (status, body) = post_query(&client, &find_people()).await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
    assert_eq!(body["data"].as_array().map(Vec::len), Some(3));
    assert!(body.get("error").is_none(), "success omits `error`: {body}");
    assert!(
        body.get("truncated").is_none(),
        "no truncation omits `truncated`: {body}"
    );
}

/// PARITY: a not-found lookup is a 404 whose body carries the unified nested
/// error envelope — `{error:{code, message, retriable, details?}}` (Issue
/// #3234), byte-shape-identical to the MCP surface. `code = NOT_FOUND`,
/// `retriable = false`, no `details`, and the flat `success` field is gone.
#[tokio::test]
async fn query_not_found_minimal_error_envelope() {
    let (client, _db) = anon_client();
    let (status, body) = post_query(
        &client,
        &json!({ "operation": "get_node", "node_id": 999_999 }),
    )
    .await;
    assert_eq!(status, 404);
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "NOT_FOUND");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "error.message must be a non-empty string: {body}"
    );
    assert_eq!(body["error"]["retriable"], false);
    assert!(
        body["error"].get("details").is_none(),
        "404 carries no `details`: {body}"
    );

    // Exact top-level key-set pin: the unified error envelope is EXACTLY
    // `{error}` (no `trace_id` without the observability feature). An ADDED
    // top-level field (e.g. a leaked flat `code`/`success`) is caught here.
    let keys: std::collections::BTreeSet<&str> = body
        .as_object()
        .expect("error body is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from(["error"]),
        "unified 404 error envelope must carry exactly {{error}} at top level: {body}"
    );
}

/// PARITY: a malformed operation payload is a clean 400 client error (never a
/// 500). A nested-object property is rejected before any DB work (BadRequest →
/// 400) and carries the unified nested envelope `{error:{code, message,
/// retriable}}` with `code = INVALID_ARGUMENT`, `retriable = false`, and no
/// `details` (consistent with the not-found case).
#[tokio::test]
async fn query_malformed_payload_is_400_not_500() {
    let (client, _db) = anon_client();
    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "create_node",
            "label": "Person",
            "properties": { "nested": { "not": "allowed" } }
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "nested property must be a 400 client error, got {status}: {body}"
    );
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "error.message must be a non-empty string: {body}"
    );
    assert_eq!(body["error"]["retriable"], false);
    assert!(
        body["error"].get("details").is_none(),
        "400 carries no `details`: {body}"
    );
}

/// PARITY: without the `observability` feature the error envelope carries no
/// `trace_id` field (the population path is compiled out). The populated shape
/// is pinned by `tests/http_otel_tracing.rs` under `observability`/`otel`.
#[tokio::test]
async fn query_error_envelope_has_no_trace_id_without_observability() {
    let (client, _db) = anon_client();
    let resp = client
        .post("/query")
        .json(&json!({ "operation": "get_node", "node_id": 999_999 }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 404);
    assert!(
        resp.header("x-trace-id").is_none(),
        "x-trace-id must be absent without the observability feature"
    );
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert!(body.get("trace_id").is_none(), "no trace_id without otel");
}

// --- /query auth ---

/// PARITY: `/query` with no credential (required mode) → uniform 401 +
/// `WWW-Authenticate: Bearer`, body `code = UNAUTHENTICATED`.
#[tokio::test]
async fn query_missing_credential_is_uniform_401() {
    let (client, _store, _db) = required_client();
    let resp = client.post("/query").json(&find_people()).send().await;
    assert_eq!(resp.status.as_u16(), 401);
    assert_eq!(resp.header("www-authenticate"), Some("Bearer"));
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "UNAUTHENTICATED");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "uniform 401 keeps a non-empty error.message string: {body}"
    );
}

/// PARITY: a Reader performing a WRITE operation → 403 PERMISSION_DENIED.
#[tokio::test]
async fn query_reader_write_is_403_permission_denied() {
    let (client, store, _db) = required_client();
    let reader = mint(&store, "reader", Role::Reader);
    let (status, body) = post_query_key(
        &client,
        &reader,
        &json!({ "operation": "create_node", "label": "Person", "properties": {"name": "X"} }),
    )
    .await;
    assert_eq!(status, 403, "reader may not write: {body}");
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
}

// --- /query resource limits (Issue #3368) ---

/// PARITY: result-row cap under the `Reject` policy → 413 with the structured
/// `{code:RESOURCE_EXHAUSTED, retriable:false, details{dimension, limit,
/// consumed}}` envelope.
#[tokio::test]
async fn query_row_cap_reject_is_413_resource_exhausted() {
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 2,
        max_result_rows: 0,
        default_max_response_bytes: 0,
        max_response_bytes: 0,
        row_overflow: RowOverflowPolicy::Reject,
    };
    let (client, db) = anon_client_with_limits(limits);
    seed(&db, 5);

    let (status, body) = post_query(&client, &find_people()).await;
    assert_eq!(status, 413, "reject policy over cap → 413: {body}");
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "RESOURCE_EXHAUSTED");
    assert_eq!(body["error"]["retriable"], false);
    assert_eq!(body["error"]["details"]["dimension"], "result_rows");
    assert_eq!(body["error"]["details"]["limit"], 2);
    assert_eq!(body["error"]["details"]["consumed"], 5);
}

/// PARITY: result-byte cap → 413 with `details.dimension = result_bytes` and a
/// `consumed` that reports the real serialized size.
#[tokio::test]
async fn query_byte_cap_is_413_resource_exhausted() {
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 0,
        max_result_rows: 0,
        default_max_response_bytes: 16,
        max_response_bytes: 0,
        row_overflow: RowOverflowPolicy::Truncate,
    };
    let (client, db) = anon_client_with_limits(limits);
    seed(&db, 3);

    let (status, body) = post_query(&client, &find_people()).await;
    assert_eq!(status, 413, "oversized response → 413: {body}");
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "RESOURCE_EXHAUSTED");
    assert_eq!(
        body["error"]["retriable"], false,
        "a byte cap is never retriable (symmetry with the row cap): {body}"
    );
    assert_eq!(body["error"]["details"]["dimension"], "result_bytes");
    assert_eq!(body["error"]["details"]["limit"], 16);
    assert!(body["error"]["details"]["consumed"].as_u64().unwrap() > 16);
}

/// PARITY: a per-call limit override above the operator ceiling is rejected
/// 422 INVALID_ARGUMENT before any DB work, with `details{dimension, requested,
/// ceiling}`.
#[tokio::test]
async fn query_over_ceiling_override_is_422_invalid_argument() {
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 10,
        max_result_rows: 100,
        default_max_response_bytes: 0,
        max_response_bytes: 0,
        row_overflow: RowOverflowPolicy::Truncate,
    };
    let (client, _db) = anon_client_with_limits(limits);

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Person",
            "limits": { "max_result_rows": 1000 }
        }),
    )
    .await;
    assert_eq!(status, 422, "over-ceiling override → 422: {body}");
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(body["error"]["retriable"], false);
    assert_eq!(body["error"]["details"]["dimension"], "result_rows");
    assert_eq!(body["error"]["details"]["requested"], 1000);
    assert_eq!(body["error"]["details"]["ceiling"], 100);
}

/// PARITY: auth is evaluated BEFORE limit validation — an unauthenticated
/// request that also breaches a ceiling is a 401, never a 422/413.
#[tokio::test]
async fn query_auth_precedes_limit_validation() {
    let db = Arc::new(AletheiaDB::new().expect("db"));
    let state = AppState::new(db.clone());
    let store = Arc::new(AuthStore::new());
    let auth = AuthState::new(store, AuthMode::Required);
    let limits = QueryLimitsConfig {
        enabled: true,
        max_in_flight_queries: 0, // Issue #3368: unbounded — these tests don't exercise the worker cap
        default_timeout_ms: 0,
        max_timeout_ms: 0,
        default_max_result_rows: 10,
        max_result_rows: 100,
        default_max_response_bytes: 0,
        max_response_bytes: 0,
        row_overflow: RowOverflowPolicy::Truncate,
    };
    let config = ServerConfig::builder().query_limits(limits).build();
    let router = build_test_router_with_auth(state, auth, &config).expect("router");
    let client = TestApp::from_router(router);

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Person",
            "limits": { "max_result_rows": 1000 }
        }),
    )
    .await;
    assert_eq!(status, 401, "auth rejects before limit logic: {body}");
    assert_eq!(body["error"]["code"], "UNAUTHENTICATED");
}

// ===========================================================================
// POST /admin/keys — create (Admin class).
// ===========================================================================

/// PARITY: an admin creating a key gets 200 with the plaintext key returned
/// exactly once (prefixed `aletheia_sk_`), the principal id, and the role.
#[tokio::test]
async fn admin_create_key_success_shape() {
    let (client, store, _db) = required_client();
    let admin = mint(&store, "root", Role::Admin);

    let resp = client
        .post("/admin/keys")
        .header("Authorization", &format!("Bearer {admin}"))
        .json(&json!({ "name": "temp-reader", "role": "reader" }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert_eq!(body["success"], true);
    let key = body["data"]["key"].as_str().expect("plaintext key once");
    assert!(key.starts_with("aletheia_sk_"), "key prefix: {key}");
    assert!(
        body["data"]["id"].as_str().is_some(),
        "principal id present"
    );
    assert_eq!(body["data"]["role"], "reader");
}

/// PARITY: a non-admin role → 403 PERMISSION_DENIED on key creation.
#[tokio::test]
async fn admin_create_key_non_admin_is_403() {
    let (client, store, _db) = required_client();
    let writer = mint(&store, "writer", Role::Writer);
    let resp = client
        .post("/admin/keys")
        .header("Authorization", &format!("Bearer {writer}"))
        .json(&json!({ "name": "sneaky", "role": "admin" }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 403);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
}

/// PARITY: invalid create-key input (empty name) is a 400 (repairable client
/// error), never a 500.
#[tokio::test]
async fn admin_create_key_invalid_name_is_400() {
    let (client, store, _db) = required_client();
    let admin = mint(&store, "root", Role::Admin);
    let resp = client
        .post("/admin/keys")
        .header("Authorization", &format!("Bearer {admin}"))
        .json(&json!({ "name": "", "role": "reader" }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 400);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
}

// ===========================================================================
// GET /admin/keys — masked list (Admin class).
// ===========================================================================

/// PARITY: the key list masks material — it exposes `key_prefix` per entry and
/// NEVER a full key; the created key's plaintext is absent from the body.
#[tokio::test]
async fn admin_list_keys_masks_material() {
    let (client, store, _db) = required_client();
    let admin = mint(&store, "root", Role::Admin);
    let bearer = format!("Bearer {admin}");

    // Create a key so there is something to mask.
    let created = client
        .post("/admin/keys")
        .header("Authorization", &bearer)
        .json(&json!({ "name": "listed", "role": "reader" }))
        .send()
        .await;
    let created: Value = serde_json::from_slice(&created.body).expect("json");
    let full_key = created["data"]["key"].as_str().expect("key").to_string();

    let resp = client
        .get("/admin/keys")
        .header("Authorization", &bearer)
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let raw = String::from_utf8_lossy(&resp.body).to_string();
    assert!(
        !raw.contains(&full_key),
        "the key list must never contain a full key"
    );
    let body: Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(body["success"], true);
    let keys = body["data"]["keys"].as_array().expect("keys array");
    assert!(!keys.is_empty());
    let entry = keys
        .iter()
        .find(|k| {
            k["key_prefix"]
                .as_str()
                .is_some_and(|p| full_key.starts_with(p))
        })
        .expect("listed entry with a masking prefix");
    let prefix = entry["key_prefix"].as_str().unwrap();
    assert!(
        prefix.len() < full_key.len(),
        "prefix must be a strict mask"
    );
}

/// PARITY: a non-admin role → 403 on the list route.
#[tokio::test]
async fn admin_list_keys_non_admin_is_403() {
    let (client, store, _db) = required_client();
    let reader = mint(&store, "reader", Role::Reader);
    let resp = client
        .get("/admin/keys")
        .header("Authorization", &format!("Bearer {reader}"))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 403);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
}

// ===========================================================================
// POST /admin/keys/revoke — revoke (Admin class).
// ===========================================================================

/// PARITY: revoking a real key → 200 `{revoked:true}` and the key stops
/// authenticating (uniform 401 thereafter).
#[tokio::test]
async fn admin_revoke_key_success_and_immediate_effect() {
    let (client, store, _db) = required_client();
    let admin = mint(&store, "root", Role::Admin);
    let bearer = format!("Bearer {admin}");

    let created = client
        .post("/admin/keys")
        .header("Authorization", &bearer)
        .json(&json!({ "name": "revoke-me", "role": "reader" }))
        .send()
        .await;
    let created: Value = serde_json::from_slice(&created.body).expect("json");
    let victim_key = created["data"]["key"].as_str().unwrap().to_string();
    let victim_id = created["data"]["id"].as_str().unwrap().to_string();

    let resp = client
        .post("/admin/keys/revoke")
        .header("Authorization", &bearer)
        .json(&json!({ "id": victim_id }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert_eq!(body["data"]["revoked"], true);

    // Immediate revocation: the key is now a uniform 401.
    let (status, body) = post_query_key(&client, &victim_key, &find_people()).await;
    assert_eq!(status, 401);
    assert_eq!(body["error"]["code"], "UNAUTHENTICATED");
}

/// PARITY: revoking an unknown id → 404 with the unified nested envelope
/// `{error:{code:NOT_FOUND, message, retriable:false}}` (Issue #3234).
#[tokio::test]
async fn admin_revoke_unknown_id_is_404() {
    let (client, store, _db) = required_client();
    let admin = mint(&store, "root", Role::Admin);
    let resp = client
        .post("/admin/keys/revoke")
        .header("Authorization", &format!("Bearer {admin}"))
        .json(&json!({ "id": "no-such-id" }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 404);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "NOT_FOUND");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "error.message must be a non-empty string: {body}"
    );
    assert_eq!(body["error"]["retriable"], false);
}

/// PARITY: a non-admin role → 403 on the revoke route.
#[tokio::test]
async fn admin_revoke_non_admin_is_403() {
    let (client, store, _db) = required_client();
    let reader = mint(&store, "reader", Role::Reader);
    let resp = client
        .post("/admin/keys/revoke")
        .header("Authorization", &format!("Bearer {reader}"))
        .json(&json!({ "id": "whatever" }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 403);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
}

// ===========================================================================
// Whole-inventory 401 sweep — a future handler that forgets the AuthContext
// extractor is caught, and the 401 body is byte-identical everywhere.
// ===========================================================================

/// PARITY: every registered route returns the SAME uniform 401 body when no
/// credential is presented (no route leaks a distinguishable shape).
#[tokio::test]
async fn every_route_uniform_401_without_credential() {
    let (client, _store, _db) = required_client();
    let routes = aletheiadb::http::handlers::all_routes();
    assert!(!routes.is_empty());

    let mut reference: Option<Vec<u8>> = None;
    for route in &routes {
        let builder = match route.method.as_str() {
            "GET" => client.get(route.path),
            "POST" => client.post(route.path).json(&json!({})),
            other => panic!("unhandled method {other} for {}", route.path),
        };
        let resp = builder.send().await;
        assert_eq!(
            resp.status.as_u16(),
            401,
            "route {} {} must 401 without a credential",
            route.method,
            route.path
        );
        assert_eq!(resp.header("www-authenticate"), Some("Bearer"));
        match &reference {
            None => reference = Some(resp.body.clone()),
            Some(r) => assert_eq!(
                &resp.body, r,
                "401 body diverged on {} {}",
                route.method, route.path
            ),
        }
    }
}

// ===========================================================================
// Pattern B — production run_server over a real TCP socket: request-body
// limit 413 (only present in the production layer stack, Issue #3426).
// ===========================================================================

fn reserve_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let p = l.local_addr().expect("addr").port();
    drop(l);
    p
}

fn raw_post_status(addr: &str, path: &str, body: &[u8]) -> Option<u16> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {len}\r\nConnection: close\r\n\r\n",
        len = body.len()
    );
    stream.write_all(head.as_bytes()).ok()?;
    // The server may RST after rejecting the oversized body before we finish
    // writing; tolerate broken-pipe/reset and still read the status line.
    let write_failed_hard = match stream.write_all(body) {
        Ok(()) => false,
        Err(e) => !matches!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        ),
    };
    if write_failed_hard {
        return None;
    }
    let _ = stream.flush();
    let mut resp = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                resp.extend_from_slice(&buf[..n]);
                if resp.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&resp);
    let line = text.lines().next()?;
    line.split_whitespace().nth(1)?.parse::<u16>().ok()
}

fn post_when_ready(addr: &str, path: &str, body: &[u8]) -> u16 {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(s) = raw_post_status(addr, path, body) {
            return s;
        }
        assert!(Instant::now() < deadline, "server at {addr} never came up");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// PARITY: the production `run_server` stack rejects an oversized request body
/// with 413 over the wire, while a small legitimate body still serves 200.
#[test]
fn run_server_enforces_request_body_413() {
    const LIMIT: usize = 4 * 1024;
    let port = reserve_port();
    let addr = format!("127.0.0.1:{port}");

    let config = ServerConfig::builder()
        .host("127.0.0.1")
        .port(port)
        .auth_mode(AuthMode::Anonymous)
        .max_request_body_bytes(LIMIT)
        .build();

    // The autumn/axum run_server future cannot be tokio::spawn'ed (higher-ranked
    // lifetime), so run it on a dedicated OS thread + current-thread runtime.
    let _server = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let _ = rt.block_on(run_server(config));
    });

    let big: Vec<Value> = (0..4_000).map(|_| json!(1)).collect();
    let oversized = serde_json::to_vec(&json!({
        "operation": "execute_query",
        "query": "MATCH (n) RETURN n",
        "parameters": { "v": big }
    }))
    .unwrap();
    assert!(oversized.len() > LIMIT && oversized.len() < 2 * 1024 * 1024);
    assert_eq!(
        post_when_ready(&addr, "/query", &oversized),
        413,
        "production stack must reject oversized body with 413"
    );

    let small = serde_json::to_vec(&json!({
        "operation": "create_node",
        "label": "Person",
        "properties": { "name": "Alice" }
    }))
    .unwrap();
    assert!(small.len() < LIMIT);
    assert_eq!(
        post_when_ready(&addr, "/query", &small),
        200,
        "a small legitimate request must still serve 200"
    );
}

// ===========================================================================
// Provenance filtering (Issue #3348)
// ===========================================================================

/// Seed a `Doc` node with the given provenance source/confidence.
fn seed_doc(db: &AletheiaDB, name: &str, source: Option<&str>, confidence: Option<f64>) -> u64 {
    let mut builder = aletheiadb::core::provenance::Provenance::builder();
    if let Some(s) = source {
        builder = builder.source(s);
    }
    if let Some(c) = confidence {
        builder = builder.confidence(c);
    }
    let mut options = aletheiadb::api::transaction::WriteRequestOptions::new();
    if source.is_some() || confidence.is_some() {
        options = options.with_provenance(builder.build().expect("valid provenance"));
    }
    db.create_node_with_options(
        "Doc",
        PropertyMapBuilder::new().insert("name", name).build(),
        options,
    )
    .expect("seed doc")
    .as_u64()
}

fn doc_names(data: &Value) -> Vec<String> {
    data.as_array()
        .unwrap()
        .iter()
        .map(|n| n["properties"]["name"].as_str().unwrap().to_string())
        .collect()
}

/// AC1/AC2: find_node filters by exact source; unattributed excluded by default.
#[tokio::test]
async fn find_node_filters_by_provenance_source() {
    let (client, db) = anon_client();
    seed_doc(&db, "crm-a", Some("crm"), Some(0.9));
    seed_doc(&db, "hr-a", Some("hr"), Some(0.9));
    seed_doc(&db, "none", None, None);

    let (status, body) = post_query(
        &client,
        &json!({"operation": "find_node", "label": "Doc", "provenance_source": "crm"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(doc_names(&body["data"]), vec!["crm-a"]);
}

/// AC2: include_unattributed re-includes no-provenance nodes.
#[tokio::test]
async fn find_node_include_unattributed() {
    let (client, db) = anon_client();
    seed_doc(&db, "crm-a", Some("crm"), Some(0.9));
    seed_doc(&db, "none", None, None);

    let (_s, excluded) = post_query(
        &client,
        &json!({"operation": "find_node", "label": "Doc", "min_confidence": 0.0}),
    )
    .await;
    assert_eq!(doc_names(&excluded["data"]), vec!["crm-a"]);

    let (_s, included) = post_query(
        &client,
        &json!({"operation": "find_node", "label": "Doc", "min_confidence": 0.0, "include_unattributed": true}),
    )
    .await;
    let mut names = doc_names(&included["data"]);
    names.sort();
    assert_eq!(names, vec!["crm-a", "none"]);
}

/// AC1: min_confidence is an inclusive lower bound.
#[tokio::test]
async fn find_node_min_confidence_inclusive() {
    let (client, db) = anon_client();
    seed_doc(&db, "hi", Some("crm"), Some(0.8));
    seed_doc(&db, "lo", Some("crm"), Some(0.79));

    let (_s, body) = post_query(
        &client,
        &json!({"operation": "find_node", "label": "Doc", "min_confidence": 0.8}),
    )
    .await;
    assert_eq!(doc_names(&body["data"]), vec!["hi"]);
}

/// AC5: invalid confidence is a 400 INVALID_ARGUMENT naming the field.
#[tokio::test]
async fn find_node_invalid_confidence_is_400() {
    let (client, db) = anon_client();
    seed_doc(&db, "a", Some("crm"), Some(0.9));

    let (status, body) = post_query(
        &client,
        &json!({"operation": "find_node", "label": "Doc", "min_confidence": 1.5}),
    )
    .await;
    assert_eq!(status, 400);
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("min_confidence"),
        "error must name the field: {body}"
    );
}

/// AC5: empty source list is a 400.
#[tokio::test]
async fn find_node_empty_source_list_is_400() {
    let (client, db) = anon_client();
    seed_doc(&db, "a", Some("crm"), Some(0.9));

    let (status, body) = post_query(
        &client,
        &json!({"operation": "find_node", "label": "Doc", "provenance_sources": []}),
    )
    .await;
    assert_eq!(status, 400);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("provenance_sources")
    );
}

/// AC1: get_node returns the node when the filter passes, 404 when excluded.
#[tokio::test]
async fn get_node_provenance_filter_excludes_as_404() {
    let (client, db) = anon_client();
    let id = seed_doc(&db, "a", Some("crm"), Some(0.9));

    let (status_ok, _b) = post_query(
        &client,
        &json!({"operation": "get_node", "node_id": id, "provenance_source": "crm"}),
    )
    .await;
    assert_eq!(status_ok, 200);

    let (status_excluded, _b) = post_query(
        &client,
        &json!({"operation": "get_node", "node_id": id, "provenance_source": "hr"}),
    )
    .await;
    assert_eq!(status_excluded, 404);
}

/// AC2: omitting the filter is byte-identical to today.
#[tokio::test]
async fn find_node_without_filter_is_unchanged() {
    let (client, db) = anon_client();
    seed_doc(&db, "a", Some("crm"), Some(0.9));
    seed_doc(&db, "b", None, None);

    let (_s, body) = post_query(&client, &json!({"operation": "find_node", "label": "Doc"})).await;
    let mut names = doc_names(&body["data"]);
    names.sort();
    assert_eq!(names, vec!["a", "b"], "no filter returns all nodes");
}

/// FIX A (review): a provenance-filtered HTTP find_node page that would filter
/// SHORT mid-scan is refilled by over-fetching, so a short/empty page genuinely
/// means end-of-data. Here the first `limit` nodes in ascending-id scan order
/// (the `hr` docs, seeded first) ALL fail the `crm` filter — the pre-fix code
/// applied the filter AFTER `.limit(limit)` and would have returned an EMPTY
/// page, tricking a "short page => done" client into concluding there are no
/// `crm` docs at all. The refill path must instead return a FULL page of 5.
#[tokio::test]
async fn find_node_filtered_page_is_refilled_not_short() {
    let (client, db) = anon_client();
    // 8 non-matching (hr) docs get the lowest ids, so they are scanned first.
    for i in 0..8 {
        seed_doc(&db, &format!("hr-{i}"), Some("hr"), Some(0.9));
    }
    // 8 matching (crm) docs follow, only reachable by scanning past the hr run.
    for i in 0..8 {
        seed_doc(&db, &format!("crm-{i}"), Some("crm"), Some(0.9));
    }

    // limit=5: the first 5 scanned (all hr) fail the filter. Pre-fix => 0 rows.
    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Doc",
            "provenance_source": "crm",
            "limit": 5,
        }),
    )
    .await;
    assert_eq!(status, 200);
    let names = doc_names(&body["data"]);
    assert_eq!(
        names.len(),
        5,
        "filtered page must be refilled to `limit` (not filtered short): {body}"
    );
    assert!(
        names.iter().all(|n| n.starts_with("crm-")),
        "every returned node must pass the crm filter: {names:?}"
    );

    // A large-limit request returns the full matching set (completeness).
    let (_s, all) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Doc",
            "provenance_source": "crm",
            "limit": 100,
        }),
    )
    .await;
    assert_eq!(
        doc_names(&all["data"]).len(),
        8,
        "all 8 crm docs are matchable: {all}"
    );

    // An offset past every node yields a genuinely EMPTY page (== end-of-data),
    // not a false "short page" mid-scan.
    let (_s, tail) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Doc",
            "provenance_source": "crm",
            "limit": 5,
            "offset": 16,
        }),
    )
    .await;
    assert_eq!(
        doc_names(&tail["data"]).len(),
        0,
        "an empty page means end-of-data: {tail}"
    );
}

/// FIX B (review): find_neighbors is wired provenance-filterable — pin that a
/// source filter narrows the neighbor set (filtered vs unfiltered differ), and
/// that invalid filter input fails closed with a 400 naming the field.
#[tokio::test]
async fn find_neighbors_filters_by_provenance() {
    use aletheiadb::core::NodeId;
    let (client, db) = anon_client();

    let center = seed_doc(&db, "center", None, None);
    let center_id = NodeId::new(center).expect("center id");

    // Two crm neighbors + two hr neighbors, all edge-connected to `center`.
    let crm1 = seed_doc(&db, "crm-1", Some("crm"), Some(0.9));
    let crm2 = seed_doc(&db, "crm-2", Some("crm"), Some(0.95));
    let hr1 = seed_doc(&db, "hr-1", Some("hr"), Some(0.9));
    let hr2 = seed_doc(&db, "hr-2", Some("hr"), Some(0.9));
    for nbr in [crm1, crm2, hr1, hr2] {
        db.create_edge(
            center_id,
            NodeId::new(nbr).expect("nbr id"),
            "LINK",
            PropertyMapBuilder::new().build(),
        )
        .expect("create edge");
    }

    // Unfiltered: all four neighbors.
    let (status, unfiltered) = post_query(
        &client,
        &json!({"operation": "find_neighbors", "node_id": center}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        doc_names(&unfiltered["data"]).len(),
        4,
        "unfiltered find_neighbors sees all four: {unfiltered}"
    );

    // Filtered by source: only the two crm neighbors.
    let (status, filtered) = post_query(
        &client,
        &json!({
            "operation": "find_neighbors",
            "node_id": center,
            "provenance_source": "crm",
        }),
    )
    .await;
    assert_eq!(status, 200);
    let mut names = doc_names(&filtered["data"]);
    names.sort();
    assert_eq!(
        names,
        vec!["crm-1", "crm-2"],
        "source filter must narrow neighbors to crm: {filtered}"
    );

    // Filtered by confidence: min_confidence 0.95 keeps only crm-2.
    let (_s, by_conf) = post_query(
        &client,
        &json!({
            "operation": "find_neighbors",
            "node_id": center,
            "min_confidence": 0.95,
        }),
    )
    .await;
    assert_eq!(
        doc_names(&by_conf["data"]),
        vec!["crm-2"],
        "min_confidence filter must apply to neighbors: {by_conf}"
    );
}

/// FIX B (review): find_neighbors validates the provenance filter and fails
/// closed with a 400 naming the offending field (AC5 parity with find_node).
#[tokio::test]
async fn find_neighbors_invalid_confidence_is_400() {
    let (client, db) = anon_client();
    let center = seed_doc(&db, "center", None, None);

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "find_neighbors",
            "node_id": center,
            "min_confidence": 1.5,
        }),
    )
    .await;
    assert_eq!(status, 400, "out-of-range confidence must be a 400: {body}");
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("min_confidence"),
        "error must name the field: {body}"
    );
}
