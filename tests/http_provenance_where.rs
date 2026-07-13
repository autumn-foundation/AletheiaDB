//! Integration test for AQL `WHERE`-clause provenance predicates over the HTTP
//! `/query` surface (Issue #3354a).
//!
//! Confirms the read-only provenance accessors flow through the HTTP
//! `execute_query` path unchanged (reader-class, no handler change) and that a
//! type-misused accessor fails closed with a structured error rather than a
//! silent empty result.
//!
//! Run with: `cargo test --test http_provenance_where --features http-server`

#![cfg(feature = "http-server")]

use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::auth::AuthMode;
use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::core::provenance::Provenance;
use aletheiadb::http::{AppState, ServerConfig, build_test_router};
use autumn_web::test::{TestApp, TestClient};
use serde_json::{Value, json};

fn client() -> (TestClient, Arc<AletheiaDB>) {
    let db = Arc::new(AletheiaDB::new().expect("create DB"));
    let state = AppState::new(db.clone());
    let config = ServerConfig::builder()
        .auth_mode(AuthMode::Anonymous)
        .build();
    let router = build_test_router(state, &config).expect("build router");
    (TestApp::from_router(router), db)
}

fn seed(db: &AletheiaDB, name: &str, source: &str, confidence: f64) {
    let props = PropertyMapBuilder::new().insert("name", name).build();
    let prov = Provenance::builder()
        .source(source)
        .confidence(confidence)
        .build()
        .expect("valid provenance");
    db.create_node_with_options(
        "Person",
        props,
        WriteRequestOptions::new().with_provenance(prov),
    )
    .expect("seed");
}

async fn post_query(client: &TestClient, body: &Value) -> (u16, Value) {
    let resp = client.post("/query").json(body).send().await;
    let status = resp.status.as_u16();
    let json = serde_json::from_slice::<Value>(&resp.body).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn provenance_where_filters_over_http() {
    let (client, db) = client();
    seed(&db, "alice", "hr-system", 0.95);
    seed(&db, "bob", "crm-sync", 0.60);
    seed(&db, "carol", "hr-system", 0.80);

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "execute_query",
            "query": "MATCH (n:Person) WHERE source(n) = 'hr-system' AND confidence(n) >= 0.9 RETURN n"
        }),
    )
    .await;

    assert_eq!(status, 200, "provenance query should succeed: {body}");
    assert_eq!(body["success"], true, "{body}");
    let rows = body["data"].as_array().expect("data array");
    assert_eq!(
        rows.len(),
        1,
        "only alice matches source+confidence: {body}"
    );
}

#[tokio::test]
async fn provenance_is_null_over_http() {
    let (client, db) = client();
    seed(&db, "alice", "hr-system", 0.95);
    // dave has no provenance bundle.
    db.create_node(
        "Person",
        PropertyMapBuilder::new().insert("name", "dave").build(),
    )
    .expect("seed dave");

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "execute_query",
            "query": "MATCH (n:Person) WHERE provenance(n) IS NULL RETURN n"
        }),
    )
    .await;

    assert_eq!(status, 200, "{body}");
    let rows = body["data"].as_array().expect("data array");
    assert_eq!(rows.len(), 1, "only the unattributed node matches: {body}");
}

#[tokio::test]
async fn provenance_type_mismatch_fails_closed_over_http() {
    let (client, db) = client();
    seed(&db, "alice", "hr-system", 0.95);

    let (status, body) = post_query(
        &client,
        &json!({
            "operation": "execute_query",
            "query": "MATCH (n:Person) WHERE confidence(n) = 'high' RETURN n"
        }),
    )
    .await;

    // Fails closed with a client error, never a silent empty 200 success.
    assert!(
        status >= 400,
        "type-misused accessor must be a structured error, not a silent empty result: \
         status={status} body={body}"
    );
    assert_eq!(body["success"], false, "{body}");
}
