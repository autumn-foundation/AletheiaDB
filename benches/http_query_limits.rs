//! Benchmarks for per-query resource-limit enforcement on the HTTP `/query`
//! surface (Issue #3368, review P5 / AC "fast queries are not taxed").
//!
//! Quantifies the enforcement overhead on the **under-limit** happy path, so a
//! regression that makes ordinary reads pay a limit tax is visible:
//!
//! - `limit_resolution` — the per-request cost of folding server defaults +
//!   override + ceilings into `EffectiveQueryLimits`, with limits enabled
//!   (`QueryLimitsConfig::default`) vs disabled (`::disabled`).
//! - `query_response_path` — the true end-to-end `/query` response path for a
//!   representative under-limit read, enabled vs disabled, driven through the
//!   real router (so the extra serialize-once + byte-cap check is included).
//! - `envelope_vs_request_deser` — deserializing a representative bulk body
//!   into the wrapping `QueryEnvelope` vs the bare `QueryRequest`, isolating
//!   the cost the `#[serde(flatten)]` envelope adds to request parsing.
//!
//! Gated on `feature = "http-server"`. Build check:
//! `cargo bench --features http-server --no-run`.

#![cfg(feature = "http-server")]

use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::auth::AuthMode;
use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::http::handlers::{QueryEnvelope, QueryRequest};
use aletheiadb::http::{AppState, QueryLimitsConfig, ServerConfig, build_test_router};
use autumn_web::test::{TestApp, TestClient};
use criterion::{Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};
use std::hint::black_box;

/// Build an anonymous-mode `/query` client with the given limits, seeded with
/// `n` `Person` nodes so an under-limit read returns a representative payload.
fn client_with_limits(limits: QueryLimitsConfig, n: usize) -> TestClient {
    let db = Arc::new(AletheiaDB::new().expect("create DB"));
    for i in 0..n {
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("idx", i as i64).build(),
        )
        .expect("seed node");
    }
    let state = AppState::new(db);
    let config = ServerConfig::builder()
        .auth_mode(AuthMode::Anonymous)
        .query_limits(limits)
        .build();
    let router = build_test_router(state, &config).expect("build router");
    TestApp::from_router(router)
}

/// A representative bulk request body used to compare envelope vs bare-request
/// deserialization cost.
fn bulk_body(n: usize) -> Value {
    let nodes: Vec<Value> = (0..n)
        .map(|i| json!({ "label": "Person", "properties": { "idx": i } }))
        .collect();
    json!({ "operation": "bulk_create_nodes", "nodes": nodes })
}

fn bench_limit_resolution(c: &mut Criterion) {
    let enabled = QueryLimitsConfig::default();
    let disabled = QueryLimitsConfig::disabled();

    let mut group = c.benchmark_group("limit_resolution");
    group.bench_function("enabled_default", |b| {
        b.iter(|| black_box(enabled.effective(black_box(None)).unwrap()));
    });
    group.bench_function("disabled", |b| {
        b.iter(|| black_box(disabled.effective(black_box(None)).unwrap()));
    });
    group.finish();
}

fn bench_query_response_path(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    // An under-limit read: 20 rows, well below the generous defaults.
    let enabled_client = client_with_limits(QueryLimitsConfig::default(), 20);
    let disabled_client = client_with_limits(QueryLimitsConfig::disabled(), 20);
    let body = json!({ "operation": "find_node", "label": "Person" });

    async fn run(client: &TestClient, body: &Value) -> u16 {
        client
            .post("/query")
            .json(body)
            .send()
            .await
            .status
            .as_u16()
    }

    let mut group = c.benchmark_group("query_response_path");
    group.bench_function("enabled_default", |b| {
        b.iter(|| {
            let status = rt.block_on(run(&enabled_client, &body));
            assert_eq!(status, 200);
        });
    });
    group.bench_function("disabled", |b| {
        b.iter(|| {
            let status = rt.block_on(run(&disabled_client, &body));
            assert_eq!(status, 200);
        });
    });
    group.finish();
}

fn bench_envelope_vs_request_deser(c: &mut Criterion) {
    let body = bulk_body(50);
    let bytes = serde_json::to_vec(&body).expect("serialize body");

    let mut group = c.benchmark_group("envelope_vs_request_deser");
    group.bench_function("query_envelope", |b| {
        b.iter(|| {
            let env: QueryEnvelope =
                serde_json::from_slice(black_box(&bytes)).expect("deserialize envelope");
            black_box(env);
        });
    });
    group.bench_function("bare_request", |b| {
        b.iter(|| {
            let req: QueryRequest =
                serde_json::from_slice(black_box(&bytes)).expect("deserialize request");
            black_box(req);
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_limit_resolution,
    bench_query_response_path,
    bench_envelope_vs_request_deser
);
criterion_main!(benches);
