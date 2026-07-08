//! Integration tests for HTTP authentication + RBAC (Issue #3350, Phase 1).
//!
//! Exercises the auth extractor, per-operation authorization, admin key
//! lifecycle endpoints, and the uniform-401 contract through a fully-wired
//! `axum::Router` (`build_test_router_with_auth`) via
//! `autumn_web::test::TestApp`.
//!
//! Run with: `cargo test --test http_auth --features http-server`

#![cfg(feature = "http-server")]

use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::http::{
    AppState, AuthState, ServerConfig, build_test_router_with_auth, validate_auth_startup,
};
use autumn_web::test::{TestApp, TestClient};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a client in the given auth mode, returning the shared store so the
/// test can mint/revoke keys directly.
fn client_with_auth(mode: AuthMode) -> (TestClient, Arc<AuthStore>, Arc<AletheiaDB>) {
    let db = Arc::new(AletheiaDB::new().expect("create DB"));
    let state = AppState::new(db.clone());
    let store = Arc::new(AuthStore::new());
    let auth = AuthState::new(store.clone(), mode);
    let config = ServerConfig::default();
    let router = build_test_router_with_auth(state, auth, &config).expect("build router");
    (TestApp::from_router(router), store, db)
}

fn required_client() -> (TestClient, Arc<AuthStore>, Arc<AletheiaDB>) {
    client_with_auth(AuthMode::Required)
}

/// Mint a key with the given role directly against the store.
fn mint(store: &AuthStore, name: &str, role: Role) -> String {
    let (_principal, key) = store.create_key(name, role).expect("create key");
    key.to_string()
}

async fn post_query_with_key(client: &TestClient, key: &str, body: &Value) -> (u16, Value) {
    let resp = client
        .post("/query")
        .header("Authorization", &format!("Bearer {key}"))
        .json(body)
        .send()
        .await;
    let status = resp.status.as_u16();
    let json = serde_json::from_slice::<Value>(&resp.body).unwrap_or(Value::Null);
    (status, json)
}

fn find_node_op() -> Value {
    json!({ "operation": "find_node", "label": "Person" })
}

fn create_node_op() -> Value {
    json!({ "operation": "create_node", "label": "Person", "properties": {"name": "X"} })
}

// ---------------------------------------------------------------------------
// Uniform 401 contract
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_credential_returns_401_with_uniform_body() {
    let (client, _store, _db) = required_client();

    let resp = client.post("/query").json(&find_node_op()).send().await;
    assert_eq!(resp.status.as_u16(), 401);

    // WWW-Authenticate: Bearer is present.
    let www = resp
        .header("www-authenticate")
        .expect("WWW-Authenticate header present");
    assert_eq!(www, "Bearer");

    let body: Value = serde_json::from_slice(&resp.body).expect("json body");
    assert_eq!(body["success"], false);
    assert_eq!(body["code"], "UNAUTHENTICATED");
    assert!(!body["error"].as_str().expect("error string").is_empty());
}

#[tokio::test]
async fn all_authentication_failures_are_byte_identical() {
    let (client, store, _db) = required_client();

    // Mint a real key and revoke it, so "revoked" is distinguishable from
    // "never existed" only if the server leaks it.
    let (principal, revoked_key) = store.create_key("victim", Role::Admin).expect("create");
    let revoked_key = revoked_key.to_string();
    assert!(store.revoke(&principal.id).expect("revoke"));

    // 1. Missing credential.
    let missing = client.post("/query").json(&find_node_op()).send().await;
    // 2. Malformed Authorization header (wrong scheme).
    let malformed = client
        .post("/query")
        .header("Authorization", "Basic dXNlcjpwYXNz")
        .json(&find_node_op())
        .send()
        .await;
    // 3. Unknown key.
    let unknown = client
        .post("/query")
        .header("Authorization", "Bearer aletheia_sk_definitely_not_a_key")
        .json(&find_node_op())
        .send()
        .await;
    // 4. Revoked key.
    let revoked = client
        .post("/query")
        .header("Authorization", &format!("Bearer {revoked_key}"))
        .json(&find_node_op())
        .send()
        .await;

    for resp in [&missing, &malformed, &unknown, &revoked] {
        assert_eq!(resp.status.as_u16(), 401);
        assert_eq!(resp.header("www-authenticate"), Some("Bearer"));
    }
    // Bodies must be byte-identical: no distinction between failure kinds.
    assert_eq!(missing.body, malformed.body);
    assert_eq!(missing.body, unknown.body);
    assert_eq!(missing.body, revoked.body);
}

#[tokio::test]
async fn unauthorized_body_never_echoes_the_presented_credential() {
    let (client, _store, _db) = required_client();

    let presented = "aletheia_sk_SecretCredentialValue123";
    let resp = client
        .post("/query")
        .header("Authorization", &format!("Bearer {presented}"))
        .json(&find_node_op())
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 401);
    let body_text = String::from_utf8_lossy(&resp.body);
    assert!(
        !body_text.contains(presented),
        "401 body must never echo the credential"
    );
    assert!(!body_text.contains("SecretCredentialValue123"));
}

#[tokio::test]
async fn x_api_key_header_is_accepted() {
    let (client, store, _db) = required_client();
    let key = mint(&store, "svc", Role::Reader);

    let resp = client
        .post("/query")
        .header("x-api-key", &key)
        .json(&find_node_op())
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
}

// ---------------------------------------------------------------------------
// Role enforcement on /query and /status
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reader_can_read_but_not_write() {
    let (client, store, db) = required_client();
    let key = mint(&store, "reader", Role::Reader);

    // Seed a node directly.
    let node_id = db
        .create_node("Person", aletheiadb::properties! { "name" => "Alice" })
        .expect("seed node");

    // find_node: allowed.
    let (status, body) = post_query_with_key(&client, &key, &find_node_op()).await;
    assert_eq!(status, 200, "reader find_node failed: {body}");
    assert_eq!(body["success"], true);

    // get_node: allowed.
    let (status, _body) = post_query_with_key(
        &client,
        &key,
        &json!({ "operation": "get_node", "node_id": node_id.as_u64() }),
    )
    .await;
    assert_eq!(status, 200);

    // create_node: 403 PERMISSION_DENIED.
    let (status, body) = post_query_with_key(&client, &key, &create_node_op()).await;
    assert_eq!(status, 403);
    assert_eq!(body["success"], false);
    assert_eq!(body["code"], "PERMISSION_DENIED");

    // bulk_delete_nodes: 403 PERMISSION_DENIED.
    let (status, body) = post_query_with_key(
        &client,
        &key,
        &json!({ "operation": "bulk_delete_nodes", "node_ids": [node_id.as_u64()] }),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "PERMISSION_DENIED");

    // The node must still exist (authorization happened before any DB work).
    assert!(db.get_node(node_id).is_ok());
}

#[tokio::test]
async fn metrics_role_can_probe_status_but_not_read_data() {
    let (client, store, _db) = required_client();
    let key = mint(&store, "monitor", Role::Metrics);

    // /status: allowed.
    let resp = client
        .get("/status")
        .header("Authorization", &format!("Bearer {key}"))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert_eq!(body["status"], "healthy");

    // /query read: 403.
    let (status, body) = post_query_with_key(&client, &key, &find_node_op()).await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "PERMISSION_DENIED");
}

#[tokio::test]
async fn status_requires_credentials_in_required_mode() {
    let (client, _store, _db) = required_client();
    let resp = client.get("/status").send().await;
    assert_eq!(resp.status.as_u16(), 401);
}

#[tokio::test]
async fn writer_can_read_and_write_but_not_admin() {
    let (client, store, _db) = required_client();
    let key = mint(&store, "writer", Role::Writer);

    // Write: allowed.
    let (status, body) = post_query_with_key(&client, &key, &create_node_op()).await;
    assert_eq!(status, 200, "writer create_node failed: {body}");

    // Read: allowed.
    let (status, _body) = post_query_with_key(&client, &key, &find_node_op()).await;
    assert_eq!(status, 200);

    // Admin endpoints: all three are 403.
    let create = client
        .post("/admin/keys")
        .header("Authorization", &format!("Bearer {key}"))
        .json(&json!({ "name": "sneaky", "role": "admin" }))
        .send()
        .await;
    assert_eq!(create.status.as_u16(), 403);
    let list = client
        .get("/admin/keys")
        .header("Authorization", &format!("Bearer {key}"))
        .send()
        .await;
    assert_eq!(list.status.as_u16(), 403);
    let revoke = client
        .post("/admin/keys/revoke")
        .header("Authorization", &format!("Bearer {key}"))
        .json(&json!({ "id": "whatever" }))
        .send()
        .await;
    assert_eq!(revoke.status.as_u16(), 403);
    let body: Value = serde_json::from_slice(&revoke.body).expect("json");
    assert_eq!(body["code"], "PERMISSION_DENIED");
}

#[tokio::test]
async fn admin_has_full_access_including_admin_endpoints() {
    let (client, store, _db) = required_client();
    let key = mint(&store, "root", Role::Admin);
    let bearer = format!("Bearer {key}");

    // Reads and writes.
    let (status, _b) = post_query_with_key(&client, &key, &create_node_op()).await;
    assert_eq!(status, 200);
    let (status, _b) = post_query_with_key(&client, &key, &find_node_op()).await;
    assert_eq!(status, 200);

    // /status.
    let resp = client
        .get("/status")
        .header("Authorization", &bearer)
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);

    // Admin: list.
    let resp = client
        .get("/admin/keys")
        .header("Authorization", &bearer)
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert_eq!(body["success"], true);
    assert!(
        !body["data"]["keys"]
            .as_array()
            .expect("keys array")
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Mutating execute_query is classified Write
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reader_gets_403_for_mutating_execute_query() {
    let (client, store, _db) = required_client();
    let key = mint(&store, "reader", Role::Reader);

    let (status, body) = post_query_with_key(
        &client,
        &key,
        &json!({ "operation": "execute_query", "query": "CREATE (n:Person {name: 'X'})" }),
    )
    .await;
    assert_eq!(status, 403, "mutating statement must need Write: {body}");
    assert_eq!(body["code"], "PERMISSION_DENIED");
}

// ---------------------------------------------------------------------------
// Admin key lifecycle end-to-end (no restart involved)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_key_lifecycle_end_to_end() {
    let (client, store, _db) = required_client();
    let admin_key = mint(&store, "root", Role::Admin);
    let admin_bearer = format!("Bearer {admin_key}");

    // 1. Admin creates a reader key over HTTP.
    let resp = client
        .post("/admin/keys")
        .header("Authorization", &admin_bearer)
        .json(&json!({ "name": "temp-reader", "role": "reader" }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert_eq!(body["success"], true);
    let reader_key = body["data"]["key"]
        .as_str()
        .expect("plaintext key")
        .to_string();
    let reader_id = body["data"]["id"]
        .as_str()
        .expect("principal id")
        .to_string();
    assert_eq!(body["data"]["role"], "reader");
    assert!(reader_key.starts_with("aletheia_sk_"));

    // 2. The new key works for reads immediately.
    let (status, _b) = post_query_with_key(&client, &reader_key, &find_node_op()).await;
    assert_eq!(status, 200);

    // 3. Admin lists keys: response contains the prefix but NOT the full key.
    let resp = client
        .get("/admin/keys")
        .header("Authorization", &admin_bearer)
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let list_text = String::from_utf8_lossy(&resp.body).to_string();
    assert!(
        !list_text.contains(&reader_key),
        "key list must never contain a full key"
    );
    let body: Value = serde_json::from_str(&list_text).expect("json");
    let keys = body["data"]["keys"].as_array().expect("keys");
    let entry = keys
        .iter()
        .find(|k| k["id"] == reader_id.as_str())
        .expect("created key is listed");
    let prefix = entry["key_prefix"].as_str().expect("prefix");
    assert!(reader_key.starts_with(prefix));
    assert!(prefix.len() < reader_key.len());

    // 4. Admin revokes it.
    let resp = client
        .post("/admin/keys/revoke")
        .header("Authorization", &admin_bearer)
        .json(&json!({ "id": reader_id }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert_eq!(body["data"]["revoked"], true);

    // 5. The same key is now rejected with the uniform 401.
    let (status, body) = post_query_with_key(&client, &reader_key, &find_node_op()).await;
    assert_eq!(status, 401);
    assert_eq!(body["code"], "UNAUTHENTICATED");
}

#[tokio::test]
async fn revoking_unknown_id_returns_404() {
    let (client, store, _db) = required_client();
    let admin_key = mint(&store, "root", Role::Admin);

    let resp = client
        .post("/admin/keys/revoke")
        .header("Authorization", &format!("Bearer {admin_key}"))
        .json(&json!({ "id": "no-such-id" }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 404);
}

// ---------------------------------------------------------------------------
// Anonymous mode (explicit opt-in)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anonymous_mode_allows_uncredentialed_requests() {
    let (client, _store, _db) = client_with_auth(AuthMode::Anonymous);

    let resp = client.get("/status").send().await;
    assert_eq!(resp.status.as_u16(), 200);

    let resp = client.post("/query").json(&create_node_op()).send().await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert_eq!(body["success"], true);
}

// ---------------------------------------------------------------------------
// Startup rules
// ---------------------------------------------------------------------------

#[test]
fn server_config_defaults_to_required_auth() {
    assert_eq!(ServerConfig::default().auth_mode(), AuthMode::Required);
}

#[test]
fn startup_validation_refuses_required_mode_with_no_keys() {
    let store = AuthStore::new();
    let err = validate_auth_startup(AuthMode::Required, &store)
        .expect_err("must refuse to start with zero credentials");
    assert!(
        err.contains("anonymous"),
        "message should name the opt-out: {err}"
    );

    // With a credential (bootstrap key), startup validation passes.
    store
        .insert_bootstrap_key("bootstrap-admin", Role::Admin, "boot-key")
        .expect("bootstrap");
    assert!(validate_auth_startup(AuthMode::Required, &store).is_ok());

    // Anonymous mode never refuses.
    assert!(validate_auth_startup(AuthMode::Anonymous, &AuthStore::new()).is_ok());
}
