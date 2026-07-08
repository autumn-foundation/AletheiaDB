//! Warden: HTTP JSON payload amplification (DoS) prevention on `/query`.
//!
//! The `/query` endpoint buffers and deserializes the entire request body into
//! a `serde_json` tree before any handler code runs, so an unbounded body is a
//! memory-amplification vector: a terse payload (a long numeric array, a large
//! map) can expand into a very large in-memory structure. A request body size
//! cap bounds that allocation up front and rejects oversized requests with
//! `413 Payload Too Large`.
//!
//! These tests lock in that the cap is (a) explicit and operator-configurable
//! via [`ServerConfig::max_request_body_bytes`], and (b) actually enforced on
//! the wire — so a future middleware refactor can't silently remove it and
//! leave the endpoint protected only by an implicit framework default.
//!
//! Regression test for Issue #3108.
//!
//! Run with: `cargo test --test warden_http_param_dos --features http-server`

#![cfg(feature = "http-server")]

use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::http::{AppState, ServerConfig, build_test_router};
use autumn_web::test::{TestApp, TestClient};
use axum::body::Body;
use serde_json::json;

/// A test limit far below any real payload but comfortably above the small,
/// legitimate request used by the positive test. Chosen well under axum's
/// historical implicit 2 MiB default so the oversized-body test is meaningful:
/// a body in `(TEST_BODY_LIMIT, 2 MiB)` is rejected *only* because our explicit
/// configured limit is wired up — not by any framework default.
const TEST_BODY_LIMIT: usize = 4 * 1024;

fn client_with_limit(max_body: usize) -> TestClient {
    let db = Arc::new(AletheiaDB::new().expect("create DB"));
    let state = AppState::new(db);
    let config = ServerConfig::builder()
        .max_request_body_bytes(max_body)
        .build();
    let router = build_test_router(state, &config).expect("build router");
    TestApp::from_router(router)
}

/// An oversized (but otherwise well-formed) body must be rejected with
/// `413 Payload Too Large` before it is deserialized.
#[tokio::test]
async fn oversized_body_is_rejected_with_413() {
    let client = client_with_limit(TEST_BODY_LIMIT);

    // Build a valid `execute_query` request whose `parameters` array pushes the
    // total body well past TEST_BODY_LIMIT (but far under 2 MiB).
    let big_array: Vec<serde_json::Value> = (0..8_000).map(|_| json!(1)).collect();
    let payload = json!({
        "operation": "execute_query",
        "query": "MATCH (n) RETURN n",
        "parameters": { "v": big_array }
    });
    let bytes = serde_json::to_vec(&payload).unwrap();
    assert!(
        bytes.len() > TEST_BODY_LIMIT && bytes.len() < 2 * 1024 * 1024,
        "test body ({} bytes) must sit between the configured limit and 2 MiB",
        bytes.len()
    );

    let resp = client
        .post("/query")
        .header("content-type", "application/json")
        .body(Body::from(bytes))
        .send()
        .await;

    assert_eq!(
        resp.status.as_u16(),
        413,
        "oversized body must be rejected with 413 Payload Too Large"
    );
}

/// A normal, small request under the configured limit must still succeed —
/// the size cap must not break the existing `/query` contract.
#[tokio::test]
async fn normal_sized_request_still_succeeds() {
    let client = client_with_limit(TEST_BODY_LIMIT);

    let payload = json!({
        "operation": "create_node",
        "label": "Person",
        "properties": { "name": "Alice", "age": 30 }
    });
    let bytes = serde_json::to_vec(&payload).unwrap();
    assert!(
        bytes.len() < TEST_BODY_LIMIT,
        "positive-test body ({} bytes) must be under the configured limit",
        bytes.len()
    );

    let resp = client
        .post("/query")
        .header("content-type", "application/json")
        .body(Body::from(bytes))
        .send()
        .await;

    assert_eq!(
        resp.status.as_u16(),
        200,
        "a legitimate small request under the limit must still succeed"
    );
}
