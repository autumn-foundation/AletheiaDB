//! Warden: robustness tests for malformed `/query` payloads. Ensures serde
//! rejects invalid JSON and invalid field types at the extractor layer
//! (before any handler code runs).
//!
//! Run with: `cargo test --test warden_http_panic --features http-server`

#![cfg(feature = "http-server")]

use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::http::{AppState, ServerConfig, build_test_router};
use autumn_web::test::{TestApp, TestClient};
use axum::body::Body;
use serde_json::json;

fn client() -> TestClient {
    let db = Arc::new(AletheiaDB::new().expect("create DB"));
    let state = AppState::new(db);
    let config = ServerConfig::default();
    let router = build_test_router(state, &config).expect("build router");
    TestApp::from_router(router)
}

// NOTE: axum's `Json<T>` extractor distinguishes two failure modes:
//
// - Malformed JSON bytes (unparseable)           → HTTP 400 Bad Request
// - Parseable JSON but schema mismatch           → HTTP 422 Unprocessable Entity
//
// actix-web collapsed both into 400. axum's split is RFC 9110-correct. The
// migration accepts this as a deliberate, minor API semantics change — see
// ADR 0051 for details.

#[tokio::test]
async fn unparseable_json_returns_400() {
    let client = client();

    let resp = client
        .post("/query")
        .header("content-type", "application/json")
        .body(Body::from(
            "{ \"operation\": \"create_node\", \"label\": \"Person\", }",
        ))
        .send()
        .await;
    assert_eq!(
        resp.status.as_u16(),
        400,
        "malformed JSON bytes should return 400"
    );
}

#[tokio::test]
async fn missing_required_field_returns_422() {
    let client = client();

    // `label` is required by the `create_node` variant.
    let resp = client
        .post("/query")
        .json(&json!({ "operation": "create_node", "properties": {} }))
        .send()
        .await;
    assert_eq!(
        resp.status.as_u16(),
        422,
        "missing required field should return 422 (axum convention)"
    );
}

#[tokio::test]
async fn wrong_type_field_returns_422() {
    let client = client();

    // `label` must be a string, not a number.
    let resp = client
        .post("/query")
        .json(&json!({
            "operation": "create_node",
            "label": 12345,
            "properties": {}
        }))
        .send()
        .await;
    assert_eq!(
        resp.status.as_u16(),
        422,
        "wrong-type field should return 422 (axum convention)"
    );
}

#[tokio::test]
async fn find_neighbors_overflow_pagination_rejected() {
    let client = client();

    // offset = usize::MAX, limit = 1 — `offset + limit` would overflow; our
    // `saturating_add` clamp must reject with 400.
    let resp = client
        .post("/query")
        .json(&json!({
            "operation": "find_neighbors",
            "node_id": 1,
            "offset": usize::MAX,
            "limit": 1
        }))
        .send()
        .await;
    assert!(
        resp.status.is_client_error(),
        "overflow attempt must be rejected"
    );
    let body: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    assert_eq!(body["success"], false);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("Pagination limit exceeded"),
        "error message should mention pagination limit: {body}"
    );
}

#[tokio::test]
async fn find_node_deep_pagination_rejected() {
    let client = client();

    let resp = client
        .post("/query")
        .json(&json!({
            "operation": "find_node",
            "label": "Person",
            "offset": 100_000,
            "limit": 10
        }))
        .send()
        .await;
    assert!(
        resp.status.is_client_error(),
        "deep pagination must be rejected"
    );
}

#[tokio::test]
async fn pagination_boundary_at_max_allowed_is_permitted() {
    let client = client();

    // offset + limit = 9_900 + 100 = 10_000 exactly — at the limit, allowed.
    let resp = client
        .post("/query")
        .json(&json!({
            "operation": "find_node",
            "label": "Person",
            "offset": 9_900,
            "limit": 100
        }))
        .send()
        .await;
    assert_eq!(
        resp.status.as_u16(),
        200,
        "pagination at exactly max should be permitted"
    );

    // 9_901 + 100 = 10_001 — just over, rejected.
    let resp = client
        .post("/query")
        .json(&json!({
            "operation": "find_node",
            "label": "Person",
            "offset": 9_901,
            "limit": 100
        }))
        .send()
        .await;
    assert!(
        resp.status.is_client_error(),
        "pagination one over max should be rejected"
    );
}
