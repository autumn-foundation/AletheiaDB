//! Integration tests for read-only-replica enforcement on the HTTP surface
//! (Issue #3355, Slice A).
//!
//! Covers:
//! 6. `POST /query` with a `create_node` request against a replica returns
//!    the structured envelope with `code: FAILED_PRECONDITION` and
//!    `details.node_role == "replica"`; a read request still succeeds.
//!    Also covers the admin key-lifecycle write routes
//!    (`POST /admin/keys`, `POST /admin/keys/revoke`).
//!
//! Run with: `cargo test --test replication_http_readonly --features http-server`

#![cfg(feature = "http-server")]

use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::auth::AuthMode;
use aletheiadb::http::{AppState, ServerConfig, build_test_router};
use autumn_web::test::{TestApp, TestClient};
use serde_json::{Value, json};

fn client_with_db(db: Arc<AletheiaDB>) -> TestClient {
    let state = AppState::new(db);
    // Anonymous mode: these tests exercise replica-role enforcement, not
    // authentication (see tests/http_auth.rs for the RBAC surface).
    let config = ServerConfig::builder()
        .auth_mode(AuthMode::Anonymous)
        .build();
    let router = build_test_router(state, &config).expect("build router");
    TestApp::from_router(router)
}

async fn post_query(client: &TestClient, body: &Value) -> (u16, Value) {
    let resp = client.post("/query").json(body).send().await;
    let status = resp.status.as_u16();
    let json = serde_json::from_slice::<Value>(&resp.body).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn create_node_is_refused_on_a_replica() {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    db.enter_replica_mode();
    let client = client_with_db(db);

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "create_node",
            "label": "Person",
            "properties": {"name": "Alice"}
        }),
    )
    .await;

    assert_eq!(status, 412, "body: {body}");
    assert!(
        body.get("success").is_none(),
        "flat `success` field should be dropped, got: {body}"
    );
    assert_eq!(body["error"]["code"], "FAILED_PRECONDITION");
    assert_eq!(body["error"]["retriable"], false);
    assert_eq!(body["error"]["details"]["node_role"], "replica");
    assert_eq!(body["error"]["details"]["reason"], "read_only_replica");
}

#[tokio::test]
async fn read_request_still_succeeds_on_a_replica() {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    let node_id = db
        .create_node(
            "Person",
            aletheiadb::PropertyMapBuilder::new()
                .insert("name", "Alice")
                .build(),
        )
        .expect("create node while primary");
    db.enter_replica_mode();
    let client = client_with_db(db);

    let (status, body) = post_query(
        &client,
        &json!({ "operation": "get_node", "node_id": node_id.as_u64() }),
    )
    .await;

    assert_eq!(status, 200, "body: {body}");
    assert!(body.get("error").is_none(), "body: {body}");
}

#[tokio::test]
async fn admin_key_create_is_refused_on_a_replica() {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    db.enter_replica_mode();
    let client = client_with_db(db);

    let resp = client
        .post("/admin/keys")
        .json(&json!({ "name": "operator", "role": "writer" }))
        .send()
        .await;
    let status = resp.status.as_u16();
    let body: Value = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);

    assert_eq!(status, 412, "body: {body}");
    assert_eq!(body["error"]["code"], "FAILED_PRECONDITION");
    assert_eq!(body["error"]["details"]["node_role"], "replica");
}

#[tokio::test]
async fn admin_key_revoke_is_refused_on_a_replica() {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    db.enter_replica_mode();
    let client = client_with_db(db);

    let resp = client
        .post("/admin/keys/revoke")
        .json(&json!({ "id": "does-not-matter" }))
        .send()
        .await;
    let status = resp.status.as_u16();
    let body: Value = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);

    assert_eq!(status, 412, "body: {body}");
    assert_eq!(body["error"]["code"], "FAILED_PRECONDITION");
    assert_eq!(body["error"]["details"]["node_role"], "replica");
}
