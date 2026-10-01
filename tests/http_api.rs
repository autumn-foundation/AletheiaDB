//! Integration tests for the `/query` endpoint's dispatch, error paths, and
//! pagination behavior. Exercises the full HTTP middleware stack via
//! `autumn_web::test::TestApp`.
//!
//! Run with: `cargo test --test http_api --features http-server`

#![cfg(feature = "http-server")]

use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::auth::AuthMode;
use aletheiadb::http::{AppState, ServerConfig, build_test_router};
use autumn_web::test::{TestApp, TestClient};
use serde_json::{Value, json};

fn client_with_db() -> (TestClient, Arc<AletheiaDB>) {
    let db = Arc::new(AletheiaDB::new().expect("create DB"));
    let state = AppState::new(db.clone());
    // Anonymous mode is an explicit opt-in (Issue #3350): these tests
    // exercise the query API, not authentication (see tests/http_auth.rs).
    let config = ServerConfig::builder()
        .auth_mode(AuthMode::Anonymous)
        .build();
    let router = build_test_router(state, &config).expect("build router");
    (TestApp::from_router(router), db)
}

async fn post_query(client: &TestClient, body: &Value) -> (u16, Value) {
    let resp = client.post("/query").json(body).send().await;
    let status = resp.status.as_u16();
    let json = serde_json::from_slice::<Value>(&resp.body).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn create_get_find_and_traverse_neighbors() {
    let (client, db) = client_with_db();

    // Create Alice
    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "create_node",
            "label": "Person",
            "properties": { "name": "Alice", "age": 30 }
        }),
    )
    .await;
    assert_eq!(status, 200, "create_node failed: {body}");
    assert_eq!(body["success"], true);
    let alice_id = body["data"]["id"].as_u64().expect("node id");

    // Get Alice
    let (status, body) = post_query(
        &client,
        &json!({ "operation": "get_node", "node_id": alice_id }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["id"].as_u64(), Some(alice_id));
    assert_eq!(body["data"]["properties"]["name"], "Alice");

    // Find Alice by label + property
    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "Person",
            "properties": { "name": "Alice" }
        }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
    let nodes = body["data"].as_array().unwrap();
    assert!(nodes.iter().any(|n| n["id"].as_u64() == Some(alice_id)));

    // Create Bob and a KNOWS edge Alice → Bob
    let (_, bob_body) = post_query(
        &client,
        &json!({
            "operation": "create_node",
            "label": "Person",
            "properties": { "name": "Bob" }
        }),
    )
    .await;
    let bob_id = bob_body["data"]["id"].as_u64().unwrap();

    db.create_edge(
        aletheiadb::core::NodeId::new(alice_id).unwrap(),
        aletheiadb::core::NodeId::new(bob_id).unwrap(),
        "KNOWS",
        aletheiadb::core::PropertyMap::new(),
    )
    .unwrap();

    // Find Alice's neighbors; Bob should be there
    let (status, body) = post_query(
        &client,
        &json!({ "operation": "find_neighbors", "node_id": alice_id }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
    let neighbors = body["data"].as_array().unwrap();
    assert!(neighbors.iter().any(|n| n["id"].as_u64() == Some(bob_id)));
}

#[tokio::test]
async fn error_cases_return_correct_status_codes() {
    let (client, _db) = client_with_db();

    // 404 for non-existent node
    let (status, _) = post_query(
        &client,
        &json!({ "operation": "get_node", "node_id": 999_999 }),
    )
    .await;
    assert_eq!(status, 404, "expected 404 for non-existent node");

    // 400 for nested-object property (unsupported)
    let (status, _) = post_query(
        &client,
        &json!({
            "operation": "create_node",
            "label": "Test",
            "properties": { "nested": { "foo": "bar" } }
        }),
    )
    .await;
    assert_eq!(status, 400, "expected 400 for nested object property");
}

/// Issue #3629/#3234: a uniqueness violation raised on the legacy JSON-RPC
/// write path (`create_node`) is classified into the unified nested envelope —
/// `409 CONSTRAINT_VIOLATION`, `retriable: false`, and machine-readable
/// `details.existing_node_id` — end-to-end through the full HTTP stack, instead
/// of the pre-#3629 blanket `400 INVALID_ARGUMENT`.
#[tokio::test]
async fn create_node_unique_violation_maps_to_409_constraint_violation() {
    let (client, db) = client_with_db();
    db.unique_constraint("Person", "email")
        .enable()
        .expect("enable unique constraint");

    // First Alice with a unique email — succeeds.
    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "create_node",
            "label": "Person",
            "properties": { "name": "Alice", "email": "a@b.com" }
        }),
    )
    .await;
    assert_eq!(status, 200, "first create should succeed: {body}");
    let alice_id = body["data"]["id"].as_u64().expect("alice id");

    // Second node reusing the email — unique violation.
    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "create_node",
            "label": "Person",
            "properties": { "name": "Bob", "email": "a@b.com" }
        }),
    )
    .await;
    assert_eq!(
        status, 409,
        "duplicate email must be a 409 conflict: {body}"
    );
    // Nested #3629 envelope (no flat `success` field on errors).
    assert!(body.get("success").is_none(), "flat success field dropped");
    assert_eq!(body["error"]["code"], "CONSTRAINT_VIOLATION");
    assert_eq!(body["error"]["retriable"], false);
    assert_eq!(body["error"]["details"]["label"], "Person");
    assert_eq!(body["error"]["details"]["property"], "email");
    assert_eq!(
        body["error"]["details"]["existing_node_id"]
            .as_u64()
            .expect("existing_node_id"),
        alice_id,
        "details must name the node that already owns the value"
    );
}

#[tokio::test]
async fn pagination_and_neighbor_deduplication() {
    let (client, db) = client_with_db();

    for i in 0..5i64 {
        db.create_node(
            "PaginationTest",
            aletheiadb::core::PropertyMapBuilder::new()
                .insert("idx", i)
                .build(),
        )
        .unwrap();
    }

    // limit=2, offset=1 → expect exactly 2 results
    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "find_node",
            "label": "PaginationTest",
            "limit": 2,
            "offset": 1
        }),
    )
    .await;
    assert_eq!(status, 200);
    let nodes = body["data"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);

    // Bidirectional edges between n1 and n2 must not double-count in neighbors.
    let n1 = db
        .create_node("N1", aletheiadb::core::PropertyMap::new())
        .unwrap();
    let n2 = db
        .create_node("N2", aletheiadb::core::PropertyMap::new())
        .unwrap();
    db.create_edge(n1, n2, "KNOWS", aletheiadb::core::PropertyMap::new())
        .unwrap();
    db.create_edge(n2, n1, "KNOWS", aletheiadb::core::PropertyMap::new())
        .unwrap();

    let (status, body) = post_query(
        &client,
        &json!({ "operation": "find_neighbors", "node_id": n1.as_u64() }),
    )
    .await;
    assert_eq!(status, 200);
    let neighbors = body["data"].as_array().unwrap();
    let n2_count = neighbors
        .iter()
        .filter(|n| n["id"].as_u64() == Some(n2.as_u64()))
        .count();
    assert_eq!(n2_count, 1, "neighbor should appear exactly once");
}

#[tokio::test]
async fn bulk_crud_and_bulk_query_endpoints_work() {
    let (client, db) = client_with_db();

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "bulk_create_nodes",
            "nodes": [
                { "label": "Person", "properties": { "name": "Alice" } },
                { "label": "Person", "properties": { "name": "Bob" } }
            ]
        }),
    )
    .await;
    assert_eq!(status, 200, "bulk_create_nodes failed: {body}");
    let created = body["data"].as_array().expect("created array");
    assert_eq!(created.len(), 2);
    let alice_id = created[0]["id"].as_u64().expect("alice id");
    let bob_id = created[1]["id"].as_u64().expect("bob id");

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "bulk_get_nodes",
            "node_ids": [alice_id, bob_id, 999_999_u64]
        }),
    )
    .await;
    assert_eq!(status, 200, "bulk_get_nodes failed: {body}");
    assert_eq!(body["data"]["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        body["data"]["missing_node_ids"].as_array().unwrap().len(),
        1
    );

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "bulk_update_nodes",
            "updates": [
                { "node_id": alice_id, "properties": { "name": "Alice Updated", "age": 30 } },
                { "node_id": bob_id, "properties": { "name": "Bob Updated" } }
            ]
        }),
    )
    .await;
    assert_eq!(status, 200, "bulk_update_nodes failed: {body}");
    let updated = body["data"].as_array().unwrap();
    assert_eq!(updated.len(), 2);
    assert_eq!(updated[0]["properties"]["name"], "Alice Updated");

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "bulk_execute_query",
            "queries": [
                { "query": "MATCH (n:Person) RETURN n" },
                { "query": "MATCH (n:Person) WHERE n.name = \"Alice Updated\" RETURN n" }
            ]
        }),
    )
    .await;
    assert_eq!(status, 200, "bulk_execute_query failed: {body}");
    let grouped = body["data"].as_array().unwrap();
    assert_eq!(grouped.len(), 2);
    assert!(grouped[0]["rows"].as_array().unwrap().len() >= 2);

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "bulk_delete_nodes",
            "node_ids": [alice_id, bob_id]
        }),
    )
    .await;
    assert_eq!(status, 200, "bulk_delete_nodes failed: {body}");
    assert_eq!(body["data"]["deleted_count"], 2);

    assert!(
        db.get_node(aletheiadb::core::NodeId::new(alice_id).unwrap())
            .is_err()
    );
}

#[tokio::test]
async fn bulk_execute_query_enforces_total_row_budget() {
    let (client, db) = client_with_db();

    for i in 0..40_i64 {
        db.create_node(
            "BudgetTest",
            aletheiadb::core::PropertyMapBuilder::new()
                .insert("idx", i)
                .build(),
        )
        .unwrap();
    }

    let mut queries = Vec::new();
    for _ in 0..600 {
        queries.push(json!({ "query": "MATCH (n:BudgetTest) RETURN n" }));
    }

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "bulk_execute_query",
            "queries": queries
        }),
    )
    .await;

    assert_eq!(status, 400, "expected request-level row budget rejection");
    assert!(
        body.get("success").is_none(),
        "flat `success` field dropped: {body}"
    );
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("result budget exceeded")
    );
}
