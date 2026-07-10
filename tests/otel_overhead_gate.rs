//! Structural overhead gate for OTel tracing (Issue #3376, Perf HIGH).
//!
//! This is the **authoritative, non-flaky gate** for the "zero overhead when
//! disabled" acceptance criterion — and, unlike a criterion micro-bench, it runs
//! in the normal `cargo test` suite (and therefore in CI). It is a *structural*
//! assertion, not a timing one, so it can never flake on a noisy runner:
//!
//! With the `otel` feature compiled in but tracing **never initialized**
//! (runtime-disabled), the per-request span hot path must be inert — it must not
//! activate tracing, must surface no trace id, must export nothing (no in-memory
//! exporter is installed), and must not panic no matter how many times it runs.
//!
//! The companion criterion micro-bench (`benches/otel_overhead.rs`) measures the
//! *magnitude* of the three regimes for local profiling; this test proves the
//! disabled path is truly inert.
//!
//! Run with: `cargo test --test otel_overhead_gate --features otel`

#![cfg(feature = "otel")]

use aletheiadb::observability::http_request_span;
use aletheiadb::observability::otel;

const SAMPLE_TRACEPARENT: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

/// The per-request span hot path, mirroring what the HTTP surface does: build
/// the root span, attach an incoming W3C context *only when active*, and record
/// the safe-by-default attributes.
fn request_span_work() {
    let span = http_request_span("POST", "/query");
    if otel::is_active() {
        otel::attach_parent(&span, Some(SAMPLE_TRACEPARENT), None);
    }
    span.record("aletheiadb.operation", "create_node");
    span.record("aletheiadb.result.count", 1_i64);
    span.record("aletheiadb.temporal.scope", "current");
    let _entered = span.enter();
    std::hint::black_box(&span);
}

#[test]
fn disabled_request_span_hot_path_is_inert() {
    // Nothing installs a subscriber in this binary → tracing is compiled in but
    // runtime-disabled. This is deterministic (a fresh process), so the gate is
    // never order-dependent.
    assert!(
        !otel::is_active(),
        "tracing must be inactive when never initialized"
    );
    assert!(
        otel::current_trace_id().is_none(),
        "no trace id may be surfaced when tracing is inactive"
    );
    assert!(
        !otel::span_is_sampled(&http_request_span("POST", "/x")),
        "no span may report sampled when tracing is inactive"
    );

    // Exercising the hot path heavily must never panic, never activate tracing,
    // and never produce a trace id — the disabled path is inert.
    for _ in 0..100_000 {
        request_span_work();
    }

    assert!(
        !otel::is_active(),
        "the hot path must not activate tracing as a side effect"
    );
    assert!(
        otel::current_trace_id().is_none(),
        "the hot path must not produce a trace id while disabled"
    );
}
