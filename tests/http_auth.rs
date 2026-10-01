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
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "UNAUTHENTICATED");
    assert!(
        !body["error"]["message"]
            .as_str()
            .expect("error string")
            .is_empty()
    );
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
// Credential transport semantics (Authorization vs x-api-key)
// ---------------------------------------------------------------------------

/// The `Authorization` header, when present, SHADOWS `x-api-key`: a
/// malformed `Authorization` is a hard 401 even when a valid `x-api-key`
/// accompanies it. This pins the semantics so a future refactor cannot
/// silently start falling back to the secondary header.
#[tokio::test]
async fn malformed_authorization_shadows_valid_x_api_key() {
    let (client, store, _db) = required_client();
    let key = mint(&store, "svc", Role::Reader);

    let resp = client
        .post("/query")
        .header("Authorization", "Basic garbage")
        .header("x-api-key", &key)
        .json(&find_node_op())
        .send()
        .await;
    assert_eq!(
        resp.status.as_u16(),
        401,
        "a malformed Authorization header must not fall back to x-api-key"
    );
    let body: Value = serde_json::from_slice(&resp.body).expect("json body");
    assert_eq!(body["error"]["code"], "UNAUTHENTICATED");
}

/// When BOTH headers carry valid keys, `Authorization` wins. Observable via
/// roles: Authorization carries a reader key, x-api-key a writer key — a
/// write must be denied with the READER's 403.
#[tokio::test]
async fn authorization_header_wins_over_x_api_key_when_both_valid() {
    let (client, store, _db) = required_client();
    let reader_key = mint(&store, "reader", Role::Reader);
    let writer_key = mint(&store, "writer", Role::Writer);

    let resp = client
        .post("/query")
        .header("Authorization", &format!("Bearer {reader_key}"))
        .header("x-api-key", &writer_key)
        .json(&create_node_op())
        .send()
        .await;
    assert_eq!(
        resp.status.as_u16(),
        403,
        "Authorization (reader) must win over x-api-key (writer)"
    );
    let body: Value = serde_json::from_slice(&resp.body).expect("json body");
    assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("error string")
            .contains("reader"),
        "the denial must reflect the Authorization key's role: {body}"
    );
}

/// The bearer scheme is case-insensitive (`bearer`, `BEARER`, `Bearer`).
#[tokio::test]
async fn bearer_scheme_is_case_insensitive() {
    let (client, store, _db) = required_client();
    let key = mint(&store, "svc", Role::Reader);

    for scheme in ["bearer", "BEARER", "Bearer", "BeArEr"] {
        let resp = client
            .post("/query")
            .header("Authorization", &format!("{scheme} {key}"))
            .json(&find_node_op())
            .send()
            .await;
        assert_eq!(
            resp.status.as_u16(),
            200,
            "scheme casing '{scheme}' must be accepted"
        );
    }
}

/// An empty bearer token (`Authorization: Bearer ` / bare `Bearer`) is a 401.
#[tokio::test]
async fn empty_bearer_token_returns_401() {
    let (client, _store, _db) = required_client();

    for header_value in ["Bearer ", "Bearer", "Bearer   "] {
        let resp = client
            .post("/query")
            .header("Authorization", header_value)
            .json(&find_node_op())
            .send()
            .await;
        assert_eq!(
            resp.status.as_u16(),
            401,
            "empty bearer token ({header_value:?}) must be rejected"
        );
    }
}

// ---------------------------------------------------------------------------
// Full route-inventory probes (closes the "future handler forgets the
// AuthContext extractor" hole)
// ---------------------------------------------------------------------------

/// Every route in the real registry returns the uniform 401 body when no
/// credential is presented — iterating `all_routes()` means a newly added
/// handler that forgets the `AuthContext` extractor fails this test.
#[tokio::test]
async fn every_registered_route_returns_uniform_401_without_credential() {
    let (client, _store, _db) = required_client();

    let routes = aletheiadb::http::handlers::all_routes();
    assert!(!routes.is_empty(), "route registry must not be empty");

    let mut reference_body: Option<Vec<u8>> = None;
    for route in &routes {
        let builder = match route.method.as_str() {
            "GET" => client.get(route.path),
            // An empty JSON body is fine: the AuthContext extractor runs
            // (and rejects) before body deserialization.
            "POST" => client.post(route.path).json(&json!({})),
            other => panic!(
                "route {} {} uses method {other}: extend this test to cover it",
                route.method, route.path
            ),
        };
        let resp = builder.send().await;
        assert_eq!(
            resp.status.as_u16(),
            401,
            "route {} {} must return 401 without a credential",
            route.method,
            route.path
        );
        assert_eq!(
            resp.header("www-authenticate"),
            Some("Bearer"),
            "route {} {} must carry WWW-Authenticate",
            route.method,
            route.path
        );
        let body: Value = serde_json::from_slice(&resp.body).expect("json 401 body");
        assert_eq!(body["error"]["code"], "UNAUTHENTICATED");
        match &reference_body {
            None => reference_body = Some(resp.body.clone()),
            Some(reference) => assert_eq!(
                &resp.body, reference,
                "401 body must be byte-identical across the whole inventory \
                 (diverged on {} {})",
                route.method, route.path
            ),
        }
    }
}

/// With reader and metrics keys, every `/admin/keys*` route in the registry
/// returns 403 PERMISSION_DENIED.
#[tokio::test]
async fn non_admin_roles_get_403_on_every_admin_route() {
    let (client, store, _db) = required_client();
    let reader_key = mint(&store, "reader", Role::Reader);
    let metrics_key = mint(&store, "metrics", Role::Metrics);

    let admin_routes: Vec<_> = aletheiadb::http::handlers::all_routes()
        .into_iter()
        .filter(|r| r.path.starts_with("/admin/keys"))
        .collect();
    assert_eq!(
        admin_routes.len(),
        3,
        "admin route inventory changed — update this test"
    );

    // A body deserializable as BOTH CreateKeyRequest and RevokeKeyRequest,
    // so extraction succeeds and the role check is what rejects.
    let probe_body = json!({ "name": "probe", "role": "reader", "id": "nonexistent" });

    for (role_name, key) in [("reader", &reader_key), ("metrics", &metrics_key)] {
        for route in &admin_routes {
            let builder = match route.method.as_str() {
                "GET" => client.get(route.path),
                "POST" => client.post(route.path).json(&probe_body),
                other => panic!(
                    "admin route {} {} uses method {other}: extend this test",
                    route.method, route.path
                ),
            };
            let resp = builder
                .header("Authorization", &format!("Bearer {key}"))
                .send()
                .await;
            assert_eq!(
                resp.status.as_u16(),
                403,
                "{role_name} on {} {} must be 403",
                route.method,
                route.path
            );
            let body: Value = serde_json::from_slice(&resp.body).expect("json 403 body");
            assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
            assert!(
                body["error"]["message"]
                    .as_str()
                    .expect("error string")
                    .contains(role_name),
                "403 must name the denied role: {body}"
            );
        }
    }

    // Sanity: nothing was created by the probe attempts.
    assert_eq!(store.len(), 2, "denied requests must not mint keys");
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
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "PERMISSION_DENIED");

    // bulk_delete_nodes: 403 PERMISSION_DENIED.
    let (status, body) = post_query_with_key(
        &client,
        &key,
        &json!({ "operation": "bulk_delete_nodes", "node_ids": [node_id.as_u64()] }),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["error"]["code"], "PERMISSION_DENIED");

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
    assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
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
    assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
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
    assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
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
    assert_eq!(body["error"]["code"], "UNAUTHENTICATED");
}

/// Invalid create-key input (empty or overlong name) is a 400 BadRequest,
/// not a 500 — the caller can repair the request.
#[tokio::test]
async fn create_key_with_invalid_name_returns_400() {
    let (client, store, _db) = required_client();
    let admin_key = mint(&store, "root", Role::Admin);

    for name in ["".to_string(), "x".repeat(300)] {
        let resp = client
            .post("/admin/keys")
            .header("Authorization", &format!("Bearer {admin_key}"))
            .json(&json!({ "name": name, "role": "reader" }))
            .send()
            .await;
        assert_eq!(
            resp.status.as_u16(),
            400,
            "invalid key name (len {}) must be a 400, not a 500",
            name.len()
        );
        let body: Value = serde_json::from_slice(&resp.body).expect("json body");
        assert!(
            body.get("success").is_none(),
            "flat `success` field dropped: {body}"
        );
    }

    // No key was minted by the rejected requests.
    assert_eq!(store.len(), 1, "only the admin key should exist");
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

// ---------------------------------------------------------------------------
// Provenance principal stamping (AC4, Issue #3350 Phase 3)
// ---------------------------------------------------------------------------

/// A write through the authenticated HTTP surface stamps the verified
/// principal's name into that version's provenance; anonymous-mode writes
/// record no principal.
#[tokio::test]
async fn authenticated_http_write_stamps_principal_into_provenance() {
    use aletheiadb::core::NodeId;

    let (client, store, db) = required_client();
    let key = mint(&store, "http-writer", Role::Writer);

    let (status, body) = post_query_with_key(&client, &key, &create_node_op()).await;
    assert_eq!(status, 200, "{body}");
    let node_id = body["data"]["id"].as_u64().expect("created node id");

    let provenance = db
        .get_node_provenance(NodeId::new(node_id).expect("node id"))
        .expect("provenance lookup")
        .expect("authenticated write must record provenance");
    assert_eq!(provenance.principal(), Some("http-writer"));
    // Nothing was fabricated in the caller-facing fields.
    assert_eq!(provenance.source(), None);

    // Bulk create stamps every node in the batch.
    let bulk = json!({
        "operation": "bulk_create_nodes",
        "nodes": [{"label": "Person"}, {"label": "Person"}]
    });
    let (status, body) = post_query_with_key(&client, &key, &bulk).await;
    assert_eq!(status, 200, "{body}");
    for created in body["data"].as_array().expect("created array") {
        let id = created["id"].as_u64().expect("id");
        let provenance = db
            .get_node_provenance(NodeId::new(id).expect("node id"))
            .expect("lookup")
            .expect("bulk write must record provenance");
        assert_eq!(provenance.principal(), Some("http-writer"));
    }

    // Bulk update stamps the NEW version's provenance too.
    let update = json!({
        "operation": "bulk_update_nodes",
        "updates": [{"node_id": node_id, "properties": {"name": "Y"}}]
    });
    let (status, body) = post_query_with_key(&client, &key, &update).await;
    assert_eq!(status, 200, "{body}");
    let provenance = db
        .get_node_provenance(NodeId::new(node_id).expect("node id"))
        .expect("lookup")
        .expect("updated version must record provenance");
    assert_eq!(provenance.principal(), Some("http-writer"));
}

/// Anonymous-mode HTTP writes record no provenance principal (absent, not
/// an empty string).
#[tokio::test]
async fn anonymous_http_write_records_no_principal() {
    use aletheiadb::core::NodeId;

    let (client, _store, db) = client_with_auth(AuthMode::Anonymous);
    let resp = client.post("/query").json(&create_node_op()).send().await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    let node_id = body["data"]["id"].as_u64().expect("id");

    let provenance = db
        .get_node_provenance(NodeId::new(node_id).expect("node id"))
        .expect("lookup");
    assert!(
        provenance.is_none(),
        "anonymous write must not fabricate provenance: {provenance:?}"
    );
}

/// A destructive `bulk_delete_nodes` through the authenticated HTTP surface
/// stamps the verified principal's name onto the tombstone version each
/// deleted node produces (Issue #3427). The node is gone from current state,
/// so attribution is read off the tombstone in history, not the live node.
#[tokio::test]
async fn authenticated_http_bulk_delete_stamps_principal_on_tombstone() {
    use aletheiadb::core::NodeId;

    let (client, store, db) = required_client();
    let key = mint(&store, "http-deleter", Role::Writer);

    // Create two nodes to delete.
    let bulk = json!({
        "operation": "bulk_create_nodes",
        "nodes": [{"label": "Person"}, {"label": "Person"}]
    });
    let (status, body) = post_query_with_key(&client, &key, &bulk).await;
    assert_eq!(status, 200, "{body}");
    let ids: Vec<u64> = body["data"]
        .as_array()
        .expect("created array")
        .iter()
        .map(|n| n["id"].as_u64().expect("id"))
        .collect();
    assert_eq!(ids.len(), 2);

    // Delete them (non-cascade) through the authenticated surface.
    let del = json!({ "operation": "bulk_delete_nodes", "node_ids": ids });
    let (status, body) = post_query_with_key(&client, &key, &del).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["deleted_count"], 2, "{body}");

    // Each node's tombstone version carries the deleter's principal.
    for id in ids {
        let history = db
            .get_node_history(NodeId::new(id).expect("node id"))
            .expect("node history");
        let tombstone = history
            .current_version()
            .expect("tombstone version")
            .provenance
            .as_ref()
            .expect("tombstone must carry provenance for an authenticated delete (#3427)");
        assert_eq!(tombstone.principal(), Some("http-deleter"));
    }
}

/// A cascade `bulk_delete_nodes` through the authenticated HTTP surface
/// stamps the principal onto the hub-node tombstone AND every co-deleted
/// edge's tombstone (Issue #3427). Edges are created via the embedded API
/// (the HTTP `/query` surface has no edge-creation operation).
#[tokio::test]
async fn authenticated_http_bulk_delete_cascade_stamps_principal_on_edges() {
    use aletheiadb::PropertyMapBuilder;
    use aletheiadb::core::{EdgeId, NodeId};

    let (client, store, db) = required_client();
    let key = mint(&store, "http-cascader", Role::Writer);

    // hub + two leaves, created through the authenticated HTTP surface.
    let bulk = json!({
        "operation": "bulk_create_nodes",
        "nodes": [{"label": "Person"}, {"label": "Person"}, {"label": "Person"}]
    });
    let (status, body) = post_query_with_key(&client, &key, &bulk).await;
    assert_eq!(status, 200, "{body}");
    let ids: Vec<u64> = body["data"]
        .as_array()
        .expect("created array")
        .iter()
        .map(|n| n["id"].as_u64().expect("id"))
        .collect();
    let hub = NodeId::new(ids[0]).expect("hub id");
    let leaf_a = NodeId::new(ids[1]).expect("leaf a id");
    let leaf_b = NodeId::new(ids[2]).expect("leaf b id");

    // One outgoing and one incoming edge on the hub (embedded API).
    let e1 = db
        .create_edge(hub, leaf_a, "KNOWS", PropertyMapBuilder::new().build())
        .expect("edge 1");
    let e2 = db
        .create_edge(leaf_b, hub, "KNOWS", PropertyMapBuilder::new().build())
        .expect("edge 2");

    // Cascade delete the hub through the authenticated surface.
    let del = json!({
        "operation": "bulk_delete_nodes",
        "node_ids": [hub.as_u64()],
        "cascade": true
    });
    let (status, body) = post_query_with_key(&client, &key, &del).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["cascade"], true, "{body}");

    // Hub node tombstone carries the principal.
    let hub_tombstone = db
        .get_node_history(hub)
        .expect("hub history")
        .current_version()
        .expect("hub tombstone")
        .provenance
        .clone()
        .expect("hub tombstone must carry provenance (#3427)");
    assert_eq!(hub_tombstone.principal(), Some("http-cascader"));

    // Both co-deleted edge tombstones carry the SAME principal.
    for eid in [e1, e2] {
        let edge_tombstone = db
            .get_edge_history(EdgeId::new(eid.as_u64()).expect("edge id"))
            .expect("edge history")
            .current_version()
            .expect("edge tombstone")
            .provenance
            .clone()
            .unwrap_or_else(|| {
                panic!(
                    "co-deleted edge {} tombstone must carry provenance (#3427)",
                    eid.as_u64()
                )
            });
        assert_eq!(edge_tombstone.principal(), Some("http-cascader"));
    }
}

/// Anonymous-mode `bulk_delete_nodes` records no provenance principal on the
/// tombstone (absent, not an empty string) — matching the anonymous
/// create/update contract.
#[tokio::test]
async fn anonymous_http_bulk_delete_records_no_principal() {
    use aletheiadb::core::NodeId;

    let (client, _store, db) = client_with_auth(AuthMode::Anonymous);

    // Create a node anonymously.
    let resp = client.post("/query").json(&create_node_op()).send().await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    let node_id = body["data"]["id"].as_u64().expect("id");

    // Delete it anonymously.
    let del = json!({ "operation": "bulk_delete_nodes", "node_ids": [node_id] });
    let resp = client.post("/query").json(&del).send().await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = serde_json::from_slice(&resp.body).expect("json");
    assert_eq!(body["data"]["deleted_count"], 1, "{body}");

    // The tombstone must not fabricate a principal.
    let tombstone = db
        .get_node_history(NodeId::new(node_id).expect("node id"))
        .expect("node history")
        .current_version()
        .expect("tombstone version")
        .provenance
        .clone();
    assert!(
        tombstone.is_none(),
        "anonymous delete must not fabricate provenance: {tombstone:?}"
    );
}
