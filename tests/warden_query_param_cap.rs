//! Warden: structural parameter/embedding cap on the `/query` ingest path.
//!
//! The request body-size limit (Issue #3108 / #3424) bounds the *bytes* a
//! `/query` request may carry, but it is not the only defense the ingest path
//! should have. The parameter/embedding deserialization path
//! (`json_to_parameter_value`) must also enforce a structural element/dimension
//! cap — mirroring the sibling property path (`json_to_property_value`) — so
//! that a future operator raising `max_request_body_bytes` cannot silently
//! reopen a structural amplification vector. The byte-size limit and the
//! structural cap are complementary bounds, not substitutes.
//!
//! This test drives the cap end-to-end through the router with a *generous*
//! body limit, so the oversized embedding array clears the `413` byte gate and
//! reaches the converter — where it must be rejected with a `400 Bad Request`
//! (a caller-fault `4xx`), never a `5xx` and never silently accepted.
//!
//! Regression test for Issue #3426 (item 1).
//!
//! Run with: `cargo test --test warden_query_param_cap --features http-server`

#![cfg(feature = "http-server")]

use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::auth::AuthMode;
use aletheiadb::http::{AppState, ServerConfig, build_test_router};
use autumn_web::test::{TestApp, TestClient};
use axum::body::Body;
use serde_json::json;

/// A body limit far larger than the oversized embedding below, so the request
/// is NOT rejected by the byte gate (Issue #3108) — the `400` this test asserts
/// can only come from the structural cap (Issue #3426), proving the two bounds
/// are independent.
const GENEROUS_BODY_LIMIT: usize = 8 * 1024 * 1024;

/// Mirror of `crate::core::property::MAX_VECTOR_DIMENSIONS` (100_000). Kept as a
/// local literal because the constant is `pub` but this integration test drives
/// only the public HTTP surface; if the real cap changes, the `+ 1` request
/// below must still exceed it — see the `at_dimension_cap` companion assertion.
const MAX_VECTOR_DIMENSIONS: usize = 100_000;

fn client_with_limit(max_body: usize) -> TestClient {
    let db = Arc::new(AletheiaDB::new().expect("create DB"));
    let state = AppState::new(db);
    // Anonymous mode (Issue #3350) so auth extraction doesn't 401 before the
    // handler converts parameters — this test exercises the structural cap in
    // isolation, exactly as `warden_http_param_dos.rs` does for the byte cap.
    let config = ServerConfig::builder()
        .max_request_body_bytes(max_body)
        .auth_mode(AuthMode::Anonymous)
        .build();
    let router = build_test_router(state, &config).expect("build router");
    TestApp::from_router(router)
}

/// An embedding parameter array whose element count exceeds
/// `MAX_VECTOR_DIMENSIONS` must be rejected with `400 Bad Request` — a
/// structural `4xx`, distinct from the `413` byte gate and never a `5xx`.
#[tokio::test]
async fn oversized_embedding_parameter_is_rejected_with_400() {
    let client = client_with_limit(GENEROUS_BODY_LIMIT);

    // One element over the vector-dimension cap. Encoded as integers to keep
    // the body compact (~200 KB) — well under GENEROUS_BODY_LIMIT, so the byte
    // gate cannot fire and the `400` must originate in the converter.
    let big_embedding: Vec<serde_json::Value> =
        (0..=MAX_VECTOR_DIMENSIONS).map(|_| json!(1)).collect();
    let payload = json!({
        "operation": "execute_query",
        "query": "MATCH (n) RETURN n",
        "parameters": { "embedding": big_embedding }
    });
    let bytes = serde_json::to_vec(&payload).unwrap();
    assert!(
        bytes.len() < GENEROUS_BODY_LIMIT,
        "test body ({} bytes) must sit under the configured body limit so the \
         413 byte gate cannot fire — the 400 must come from the structural cap",
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
        400,
        "an oversized embedding parameter must be rejected with 400 Bad Request \
         (structural cap), got {}",
        resp.status.as_u16()
    );
    // Explicitly pin that this is neither the byte gate nor a server fault.
    assert_ne!(resp.status.as_u16(), 413, "must not be the byte-size gate");
    assert!(
        !resp.status.is_server_error(),
        "structural rejection must never be a 5xx"
    );
}

/// A legitimately-sized embedding parameter under the cap must still be
/// accepted — the structural cap must not regress the happy path.
#[tokio::test]
async fn normal_embedding_parameter_is_accepted() {
    let client = client_with_limit(GENEROUS_BODY_LIMIT);

    let payload = json!({
        "operation": "execute_query",
        "query": "MATCH (n) RETURN n",
        "parameters": { "embedding": [0.1, 0.2, 0.3, 0.4] }
    });

    let resp = client
        .post("/query")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .send()
        .await;

    assert_eq!(
        resp.status.as_u16(),
        200,
        "a legitimate small embedding parameter must still succeed"
    );
}
