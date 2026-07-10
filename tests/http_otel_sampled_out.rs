//! HTTP-surface behavior when the head sampler drops the trace (Issue #3376,
//! MED1 / MED2). Runs in its own test binary so it can install an `always_off`
//! global subscriber independent of the `always_on` subscriber used by
//! `http_otel_tracing.rs` (only one global `tracing` subscriber exists per
//! process).
//!
//! Asserts that for a sampled-out request:
//!
//! - **no** span is exported (the trace is dropped, not just un-sent); and
//! - an error response carries **no** `trace_id` body field and **no**
//!   `x-trace-id` header — a dropped trace id would be a dead-end lookup.
//!
//! Run with: `cargo test --test http_otel_sampled_out --features http-server,otel`

#![cfg(all(feature = "http-server", feature = "otel"))]

use std::sync::Arc;
use std::sync::OnceLock;

use aletheiadb::AletheiaDB;
use aletheiadb::auth::AuthMode;
use aletheiadb::http::{AppState, ServerConfig, build_test_router};
use aletheiadb::observability::otel::{InMemorySpanExporter, OtelConfig, SamplerConfig};
use autumn_web::test::{TestApp, TestClient};
use serde_json::{Value, json};
use serial_test::serial;

const ROOT_SPAN: &str = "aletheiadb.http.request";

/// Install an `always_off` global subscriber exactly once for this binary.
fn exporter() -> InMemorySpanExporter {
    static EXPORTER: OnceLock<InMemorySpanExporter> = OnceLock::new();
    EXPORTER
        .get_or_init(|| {
            let config = OtelConfig {
                enabled: true,
                sampler: SamplerConfig::AlwaysOff,
                ..OtelConfig::default()
            };
            let (guard, exp) = aletheiadb::observability::otel::init_in_memory_global(&config)
                .expect("install global otel subscriber");
            std::mem::forget(guard);
            exp
        })
        .clone()
}

fn client() -> TestClient {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let state = AppState::new(db);
    let config = ServerConfig::builder()
        .auth_mode(AuthMode::Anonymous)
        .build();
    let router = build_test_router(state, &config).expect("router builds");
    TestApp::from_router(router)
}

#[tokio::test]
#[serial]
async fn sampled_out_error_response_omits_trace_id() {
    let exp = exporter();
    exp.clear();
    let client = client();

    // A get_node for a non-existent id → 404 error path.
    let resp = client
        .post("/query")
        .json(&json!({"operation": "get_node", "node_id": 999_999_999}))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 404);

    // The trace was dropped by the sampler: no trace_id anywhere.
    let body: Value = serde_json::from_slice(&resp.body).expect("json error body");
    assert!(
        body.get("trace_id").is_none(),
        "sampled-out error response must omit trace_id, got {body}"
    );
    assert!(
        resp.header("x-trace-id").is_none(),
        "sampled-out error response must omit the x-trace-id header"
    );

    // And no aletheiadb.http.request span was ever exported.
    let spans = exp.spans();
    assert!(
        !spans.iter().any(|s| s.name == ROOT_SPAN),
        "a sampled-out request must export no http root span, got {} spans",
        spans.len()
    );
}

#[tokio::test]
#[serial]
async fn sampled_out_success_exports_no_span() {
    let exp = exporter();
    exp.clear();
    let client = client();

    let resp = client
        .post("/query")
        .json(&json!({"operation": "create_node", "label": "Person"}))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);

    let spans = exp.spans();
    assert!(
        !spans.iter().any(|s| s.name == ROOT_SPAN),
        "a sampled-out request must export no http root span, got {} spans",
        spans.len()
    );
}
