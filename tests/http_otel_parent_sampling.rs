//! Parent-based sampling honors the incoming W3C `sampled` flag (Issue #3376
//! test gap). Runs in its own test binary with a `parentbased_always_on` global
//! subscriber (the default policy) so a request that arrives with an explicit
//! parent decision follows it:
//!
//! - incoming `...-01` (sampled) → the trace is continued and exported;
//! - incoming `...-00` (not sampled) → the trace is dropped, nothing exported.
//!
//! Run with: `cargo test --test http_otel_parent_sampling --features http-server,otel`

#![cfg(all(feature = "http-server", feature = "otel"))]

use std::sync::Arc;
use std::sync::OnceLock;

use aletheiadb::AletheiaDB;
use aletheiadb::auth::AuthMode;
use aletheiadb::http::{AppState, ServerConfig, build_test_router};
use aletheiadb::observability::otel::{InMemorySpanExporter, OtelConfig, SamplerConfig, SpanData};
use autumn_web::test::{TestApp, TestClient};
use serde_json::json;
use serial_test::serial;

const ROOT_SPAN: &str = "aletheiadb.http.request";
const INCOMING_TRACE: &str = "0af7651916cd43dd8448eb211c80319c";

fn exporter() -> InMemorySpanExporter {
    static EXPORTER: OnceLock<InMemorySpanExporter> = OnceLock::new();
    EXPORTER
        .get_or_init(|| {
            let config = OtelConfig {
                enabled: true,
                sampler: SamplerConfig::ParentBasedAlwaysOn,
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

fn root_in_trace(spans: &[SpanData]) -> Option<&SpanData> {
    spans
        .iter()
        .find(|s| s.name == ROOT_SPAN && s.span_context.trace_id().to_string() == INCOMING_TRACE)
}

#[tokio::test]
#[serial]
async fn incoming_sampled_flag_continues_trace() {
    let exp = exporter();
    exp.clear();
    let client = client();

    // Parent decision: sampled (`-01`).
    let resp = client
        .post("/query")
        .header(
            "traceparent",
            &format!("00-{INCOMING_TRACE}-b7ad6b7169203331-01"),
        )
        .json(&json!({"operation": "find_node", "label": "Nothing"}))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);

    let spans = exp.spans();
    assert!(
        root_in_trace(&spans).is_some(),
        "a `-01` sampled parent must continue and export the trace"
    );
}

#[tokio::test]
#[serial]
async fn incoming_not_sampled_flag_drops_trace() {
    let exp = exporter();
    exp.clear();
    let client = client();

    // Parent decision: not sampled (`-00`).
    let resp = client
        .post("/query")
        .header(
            "traceparent",
            &format!("00-{INCOMING_TRACE}-b7ad6b7169203331-00"),
        )
        .json(&json!({"operation": "find_node", "label": "Nothing"}))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);

    let spans = exp.spans();
    assert!(
        root_in_trace(&spans).is_none(),
        "a `-00` not-sampled parent must drop the trace (nothing exported), got {} spans",
        spans.len()
    );
}
