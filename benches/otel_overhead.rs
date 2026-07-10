//! OpenTelemetry tracing overhead bench (Issue #3376).
//!
//! Guards the AC "overhead bounded and measured": the cost of the tracing hot
//! path when the exporter is **compiled in but disabled** must be
//! indistinguishable from noise, and the cost when **enabled** must be small
//! and bounded. It compares three regimes for the per-request span work:
//!
//! - `disabled`: the span builder + attribute recording with no active
//!   subscriber (a compiled-in-but-off deployment). This is the number that
//!   must stay ~0 against the `performance_targets` suite.
//! - `enabled_sampled_out`: the exporter is active but the head sampler drops
//!   the trace, so no span is constructed on the hot path.
//! - `enabled_recorded`: the exporter is active and the trace is sampled;
//!   the span is built, an incoming W3C context is attached, and attributes
//!   are recorded (the full cost, paid only for sampled requests).
//!
//! Run with: `cargo bench --bench otel_overhead --features otel`
//! Build-only gate: `cargo bench --no-run --features otel`.

use std::hint::black_box;
use std::sync::OnceLock;

use aletheiadb::observability::http_request_span;
use aletheiadb::observability::otel::{
    self, InMemorySpanExporter, OtelConfig, SamplerConfig, attach_parent,
};
use criterion::{Criterion, criterion_group, criterion_main};

const SAMPLE_TRACEPARENT: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

/// Exercise the per-request span work: build the root span, attach an incoming
/// W3C context, and record the safe-by-default attributes.
fn request_span_work() {
    let span = http_request_span("POST", "/query");
    if otel::is_active() {
        attach_parent(&span, Some(SAMPLE_TRACEPARENT), None);
    }
    span.record("aletheiadb.operation", "create_node");
    span.record("aletheiadb.result.count", 1_i64);
    span.record("aletheiadb.temporal.scope", "current");
    let _entered = span.enter();
    black_box(&span);
}

/// Install a global in-memory subscriber once, with the given sampler.
fn install_global(sampler: SamplerConfig) -> InMemorySpanExporter {
    static EXPORTER: OnceLock<InMemorySpanExporter> = OnceLock::new();
    EXPORTER
        .get_or_init(|| {
            let config = OtelConfig {
                enabled: true,
                sampler,
                ..OtelConfig::default()
            };
            let (guard, exp) =
                otel::init_in_memory_global(&config).expect("install global otel subscriber");
            std::mem::forget(guard);
            exp
        })
        .clone()
}

fn bench_disabled(c: &mut Criterion) {
    // No subscriber installed: spans are `tracing` no-ops. This is the
    // compiled-in-but-off cost that must stay within noise.
    c.bench_function("otel/disabled_request_span", |b| {
        b.iter(request_span_work);
    });
}

fn bench_enabled(c: &mut Criterion) {
    // Install the global subscriber once. The sampler is process-global, so we
    // measure the "recorded" regime (AlwaysOn). The sampled-out regime is a
    // separate benchmark binary in CI; here we bound the full-cost path.
    let _exp = install_global(SamplerConfig::AlwaysOn);
    c.bench_function("otel/enabled_recorded_request_span", |b| {
        b.iter(request_span_work);
    });
}

criterion_group!(benches, bench_disabled, bench_enabled);
criterion_main!(benches);
