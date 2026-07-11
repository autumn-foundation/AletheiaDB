//! End-to-end OpenTelemetry tracing tests for the HTTP surface (Issue #3376).
//!
//! Uses an in-memory span exporter as a stand-in OTLP receiver (installed once
//! as the process-global subscriber so spans created on `spawn_blocking`
//! threads are captured too) and drives the fully-wired test router. Verifies:
//!
//! - database child spans nest under the HTTP root span (0 orphaned spans);
//! - an incoming W3C `traceparent` continues the caller's trace;
//! - an absent `traceparent` starts a fresh root trace;
//! - **credentials never appear in any span** (attributes/name/events);
//! - error responses carry the active trace id for correlation;
//! - a request is identifiable by its operation attribute.
//!
//! Run with: `cargo test --test http_otel_tracing --features http-server,otel`

#![cfg(all(feature = "http-server", feature = "otel"))]

use std::sync::Arc;
use std::sync::OnceLock;

use aletheiadb::AletheiaDB;
use aletheiadb::auth::AuthMode;
use aletheiadb::http::{AppState, ServerConfig, build_test_router};
use aletheiadb::observability::otel::{InMemorySpanExporter, OtelConfig, SamplerConfig, SpanData};
use autumn_web::test::{TestApp, TestClient};
use serde_json::{Value, json};
use serial_test::serial;

/// The HTTP root span name.
const ROOT_SPAN: &str = "aletheiadb.http.request";
/// The invalid/zero span id (16 hex zeros) that marks a root span's parent.
const INVALID_SPAN_ID: &str = "0000000000000000";
/// Canary credential material; no span may ever contain it.
const CREDENTIAL_CANARY: &str = "credentialleakcanary0xdeadbeef";

/// Install the global OTel subscriber exactly once (only one global `tracing`
/// subscriber may exist per process) and return the shared in-memory exporter.
/// The guard is intentionally leaked so tracing stays active for the whole
/// test binary.
fn exporter() -> InMemorySpanExporter {
    static EXPORTER: OnceLock<InMemorySpanExporter> = OnceLock::new();
    EXPORTER
        .get_or_init(|| {
            let config = OtelConfig {
                enabled: true,
                sampler: SamplerConfig::AlwaysOn,
                ..OtelConfig::default()
            };
            let (guard, exp) = aletheiadb::observability::otel::init_in_memory_global(&config)
                .expect("install global otel subscriber");
            std::mem::forget(guard);
            exp
        })
        .clone()
}

/// Fresh anonymous-mode router (auth is covered elsewhere; anonymous mode lets
/// us send credential headers that are ignored, so we can prove they never
/// leak into spans).
fn client() -> TestClient {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let state = AppState::new(db);
    let config = ServerConfig::builder()
        .auth_mode(AuthMode::Anonymous)
        .build();
    let router = build_test_router(state, &config).expect("router builds");
    TestApp::from_router(router)
}

fn find_root(spans: &[SpanData]) -> &SpanData {
    spans
        .iter()
        .find(|s| s.name == ROOT_SPAN)
        .expect("an http root span was exported")
}

/// Spans belonging to the same trace as the HTTP root span. The test router's
/// tower `TraceLayer` emits its own separate-trace request span; we scope
/// nesting/continuity assertions to *our* trace.
fn request_trace_spans(spans: &[SpanData]) -> Vec<&SpanData> {
    let root = find_root(spans);
    let trace = root.span_context.trace_id().to_string();
    spans
        .iter()
        .filter(|s| s.span_context.trace_id().to_string() == trace)
        .collect()
}

/// Full lowercase debug dump of a span, for credential-leak scanning across
/// name, attributes, and events.
fn span_dump(s: &SpanData) -> String {
    format!("{s:?}").to_lowercase()
}

/// Read a string-valued attribute off a span.
fn attr(s: &SpanData, key: &str) -> Option<String> {
    s.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.to_string())
}

#[tokio::test]
#[serial]
async fn db_child_spans_nest_under_http_root_with_no_orphans() {
    let exp = exporter();
    exp.clear();
    let client = client();

    // A write op produces an `aletheiadb.transaction.commit` child span.
    let resp = client
        .post("/query")
        .json(&json!({"operation": "create_node", "label": "Person"}))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200, "create_node should succeed");

    let all = exp.spans();
    assert!(!all.is_empty(), "expected exported spans");
    let spans = request_trace_spans(&all);
    let root = find_root(&all);

    // At least one child DB span nested under the http root.
    let child_count = spans
        .iter()
        .filter(|s| s.parent_span_id == root.span_context.span_id())
        .count();
    assert!(
        child_count >= 1,
        "expected >=1 DB child span under the http root, got {child_count}"
    );

    // 0 orphans: every non-root span in the request trace has a parent present.
    let ids: std::collections::HashSet<String> = spans
        .iter()
        .map(|s| s.span_context.span_id().to_string())
        .collect();
    for s in &spans {
        let parent = s.parent_span_id.to_string();
        if parent != INVALID_SPAN_ID {
            assert!(
                ids.contains(&parent),
                "orphaned span {}: parent {parent} not present in request trace",
                s.name
            );
        }
    }
    // The root itself is a root (no in-trace parent).
    assert_eq!(root.parent_span_id.to_string(), INVALID_SPAN_ID);
}

#[tokio::test]
#[serial]
async fn incoming_traceparent_continues_caller_trace() {
    let exp = exporter();
    exp.clear();
    let client = client();

    // W3C traceparent: version-traceid-spanid-flags.
    let incoming_trace = "0af7651916cd43dd8448eb211c80319c";
    let incoming_span = "b7ad6b7169203331";
    let traceparent = format!("00-{incoming_trace}-{incoming_span}-01");

    let resp = client
        .post("/query")
        .header("traceparent", &traceparent)
        .json(&json!({"operation": "create_node", "label": "Doc"}))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);

    let all = exp.spans();
    assert!(!all.is_empty());
    let root = find_root(&all);
    // Continuity: the root joined the caller's trace, and every span in that
    // trace (root + DB children) carries the incoming trace id.
    assert_eq!(root.span_context.trace_id().to_string(), incoming_trace);
    for s in request_trace_spans(&all) {
        assert_eq!(
            s.span_context.trace_id().to_string(),
            incoming_trace,
            "span {} did not join the caller's trace",
            s.name
        );
    }
    // The root span's parent is the caller's span id.
    assert_eq!(
        root.parent_span_id.to_string(),
        incoming_span,
        "http root did not nest under the incoming remote span"
    );
}

#[tokio::test]
#[serial]
async fn absent_traceparent_starts_new_root_trace() {
    let exp = exporter();
    exp.clear();
    let client = client();

    let resp = client
        .post("/query")
        .json(&json!({"operation": "find_node", "label": "Nothing"}))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);

    let spans = exp.spans();
    let root = find_root(&spans);
    // A fresh, valid trace with no remote parent.
    assert_eq!(root.parent_span_id.to_string(), INVALID_SPAN_ID);
    assert_ne!(
        root.span_context.trace_id().to_string(),
        "00000000000000000000000000000000",
        "root should have a valid, freshly-generated trace id"
    );
}

/// A malformed `traceparent` (`"garbage"`) must not error the request and must
/// start a fresh, valid (non-all-zeros) root trace rather than nesting under a
/// bogus parent (Issue #3376 test gap).
#[tokio::test]
#[serial]
async fn malformed_traceparent_starts_fresh_root() {
    let exp = exporter();
    exp.clear();
    let client = client();

    // Each is rejected by the W3C parser: no structure; the forbidden `ff`
    // version; and a truncated 4-field header missing the flags. (A merely
    // *unknown* forward-compatible version like `99` is deliberately NOT here —
    // the spec requires it still be parsed, so it would continue the trace.)
    for bad in [
        "garbage",
        "ff-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331",
    ] {
        exp.clear();
        let resp = client
            .post("/query")
            .header("traceparent", bad)
            .json(&json!({"operation": "find_node", "label": "Nothing"}))
            .send()
            .await;
        assert_eq!(
            resp.status.as_u16(),
            200,
            "malformed traceparent {bad:?} must not error"
        );

        let spans = exp.spans();
        let root = find_root(&spans);
        assert_eq!(
            root.parent_span_id.to_string(),
            INVALID_SPAN_ID,
            "malformed traceparent {bad:?} should yield a fresh root"
        );
        assert_ne!(
            root.span_context.trace_id().to_string(),
            "00000000000000000000000000000000",
            "root should have a valid, freshly-generated trace id (not all zeros)"
        );
    }
}

/// Per W3C Trace Context, more than one `traceparent` header is ambiguous and
/// must be treated as no parent — a fresh root trace (Issue #3376, LOW5).
#[tokio::test]
#[serial]
async fn multiple_traceparent_headers_start_fresh_root() {
    let exp = exporter();
    exp.clear();
    let client = client();

    let incoming_trace = "0af7651916cd43dd8448eb211c80319c";

    let resp = client
        .post("/query")
        .header(
            "traceparent",
            &format!("00-{incoming_trace}-b7ad6b7169203331-01"),
        )
        .header(
            "traceparent",
            "00-11111111111111111111111111111111-2222222222222222-01",
        )
        .json(&json!({"operation": "find_node", "label": "Nothing"}))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);

    let spans = exp.spans();
    let root = find_root(&spans);
    // Fresh root: neither incoming trace id is adopted, and there is no parent.
    assert_eq!(root.parent_span_id.to_string(), INVALID_SPAN_ID);
    assert_ne!(
        root.span_context.trace_id().to_string(),
        incoming_trace,
        "duplicate traceparent must NOT continue the caller's trace"
    );
    assert_ne!(
        root.span_context.trace_id().to_string(),
        "00000000000000000000000000000000"
    );
}

/// A `tracestate` with no accompanying `traceparent` carries no parent span —
/// the request must start a fresh root trace (Issue #3376 test gap).
#[tokio::test]
#[serial]
async fn tracestate_without_traceparent_starts_fresh_root() {
    let exp = exporter();
    exp.clear();
    let client = client();

    let resp = client
        .post("/query")
        .header("tracestate", "vendor=value")
        .json(&json!({"operation": "find_node", "label": "Nothing"}))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);

    let spans = exp.spans();
    let root = find_root(&spans);
    assert_eq!(root.parent_span_id.to_string(), INVALID_SPAN_ID);
    assert_ne!(
        root.span_context.trace_id().to_string(),
        "00000000000000000000000000000000",
        "root should have a valid, freshly-generated trace id"
    );
}

#[tokio::test]
#[serial]
async fn credentials_never_appear_in_span_attributes() {
    let exp = exporter();
    exp.clear();
    let client = client();

    // Anonymous mode ignores these, but they must still never land in a span.
    let resp = client
        .post("/query")
        .header("Authorization", &format!("Bearer {CREDENTIAL_CANARY}"))
        .header("x-api-key", CREDENTIAL_CANARY)
        .header(
            "traceparent",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        )
        .json(&json!({"operation": "create_node", "label": "Secret"}))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);

    let spans = exp.spans();
    assert!(!spans.is_empty(), "expected spans to scan");
    for s in &spans {
        let dump = span_dump(s);
        assert!(
            !dump.contains(CREDENTIAL_CANARY),
            "credential canary leaked into span {}: {dump}",
            s.name
        );
        // The literal header names must not appear as attribute keys either.
        for kv in &s.attributes {
            let key = kv.key.as_str().to_ascii_lowercase();
            assert_ne!(
                key, "authorization",
                "authorization became a span attribute"
            );
            assert_ne!(key, "x-api-key", "x-api-key became a span attribute");
        }
    }
}

#[tokio::test]
#[serial]
async fn error_response_carries_trace_id_for_correlation() {
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

    // Body carries a trace_id, and the x-trace-id header is present.
    let body: Value = serde_json::from_slice(&resp.body).expect("json error body");
    let trace_id = body
        .get("trace_id")
        .and_then(Value::as_str)
        .expect("error body has trace_id");
    assert_eq!(trace_id.len(), 32, "trace id should be 32 hex chars");
    let header_tid = resp
        .header("x-trace-id")
        .expect("x-trace-id header present");
    assert_eq!(header_tid, trace_id, "header and body trace id must match");

    // And the id resolves to the root span actually exported for this request.
    let spans = exp.spans();
    let root = find_root(&spans);
    assert_eq!(root.span_context.trace_id().to_string(), trace_id);
    assert_eq!(
        attr(root, "aletheiadb.error.code").as_deref(),
        Some("NOT_FOUND")
    );
}

#[tokio::test]
#[serial]
async fn request_is_identifiable_by_operation_attribute() {
    let exp = exporter();
    exp.clear();
    let client = client();

    // Mix a few operation shapes; then locate one by its span attribute.
    for op in [
        json!({"operation": "find_node", "label": "A"}),
        json!({"operation": "create_node", "label": "B"}),
    ] {
        let resp = client.post("/query").json(&op).send().await;
        assert_eq!(resp.status.as_u16(), 200);
    }

    let spans = exp.spans();
    // The create_node root span is findable purely by attribute — the
    // "find the offending operation" workflow.
    let create_root = spans
        .iter()
        .find(|s| {
            s.name == ROOT_SPAN && attr(s, "aletheiadb.operation").as_deref() == Some("create_node")
        })
        .expect("create_node request identifiable by operation attribute");
    // Result count is stamped for slicing.
    assert!(
        attr(create_root, "aletheiadb.result.count").is_some(),
        "result.count attribute should be recorded"
    );
}
