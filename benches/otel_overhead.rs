//! OpenTelemetry tracing overhead micro-bench (Issue #3376).
//!
//! Measures the magnitude of the per-request span hot path across the three
//! regimes that matter for the "overhead bounded and measured" AC:
//!
//! - `disabled`: the span builder + attribute recording with **no** subscriber
//!   installed (a compiled-in-but-off deployment). Measured FIRST, before any
//!   global subscriber exists, so the reading is genuinely the disabled cost.
//! - `enabled_sampled_out`: the exporter is active but the trace is head-sampled
//!   *out* (an incoming `...-00` parent under the parent-based sampler). The
//!   span is constructed and the sampler runs, but the span is not recorded, so
//!   the per-request attribute recording is inert.
//! - `enabled_recorded`: the exporter is active and the trace is sampled (an
//!   incoming `...-01` parent). The span is built, the incoming context is
//!   attached, and attributes are recorded — the full cost, paid only for
//!   sampled requests.
//!
//! **This criterion bench is a profiling aid, not the CI gate.** A single
//! process can only ever hold one global `tracing` subscriber, so once it is
//! installed the "disabled" cost can no longer be observed in-process; the
//! `disabled` group is therefore registered first and measured before install.
//! The authoritative, non-flaky, CI-run gate is the structural inertness test
//! `tests/otel_overhead_gate.rs`. The in-memory global here uses
//! `SimpleSpanProcessor` (export-on-end); production uses `BatchSpanProcessor`,
//! so treat the `enabled_recorded` number as an upper bound on per-span export
//! bookkeeping rather than a production-exact figure.
//!
//! Run with: `cargo bench --bench otel_overhead --features otel`
//! Build-only gate: `cargo bench --no-run --bench otel_overhead --features otel`.

use std::hint::black_box;
use std::sync::OnceLock;

use aletheiadb::observability::http_request_span;
use aletheiadb::observability::otel::{
    self, InMemorySpanExporter, OtelConfig, SamplerConfig, attach_parent,
};
use criterion::{Criterion, criterion_group, criterion_main};

/// A `...-01` (sampled) incoming parent.
const SAMPLED_TRACEPARENT: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
/// A `...-00` (not-sampled) incoming parent.
const UNSAMPLED_TRACEPARENT: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-00";

/// Exercise the per-request span hot path with the given incoming parent: build
/// the root span, attach the incoming W3C context when active, and record the
/// safe-by-default attributes. The exporter buffer (if any) is cleared each call
/// so it cannot grow unbounded across millions of iterations.
fn request_span_work(traceparent: &str, exporter: Option<&InMemorySpanExporter>) {
    let span = http_request_span("POST", "/query");
    if otel::is_active() {
        attach_parent(&span, Some(traceparent), None);
    }
    span.record("aletheiadb.operation", "create_node");
    span.record("aletheiadb.result.count", 1_i64);
    span.record("aletheiadb.temporal.scope", "current");
    {
        let _entered = span.enter();
        black_box(&span);
    }
    // Bound memory: keep the in-memory buffer from growing without limit.
    if let Some(exp) = exporter {
        exp.clear();
    }
}

/// Install the global in-memory subscriber exactly once (parent-based sampler so
/// the incoming flag decides sampled-out vs recorded per request).
fn install_global() -> InMemorySpanExporter {
    static EXPORTER: OnceLock<InMemorySpanExporter> = OnceLock::new();
    EXPORTER
        .get_or_init(|| {
            let config = OtelConfig {
                enabled: true,
                sampler: SamplerConfig::ParentBasedAlwaysOn,
                ..OtelConfig::default()
            };
            let (guard, exp) =
                otel::init_in_memory_global(&config).expect("install global otel subscriber");
            // Leak the guard so tracing stays active for the whole bench binary.
            std::mem::forget(guard);
            exp
        })
        .clone()
}

fn bench_disabled(c: &mut Criterion) {
    // Registered first and measured before any global subscriber exists, so this
    // is the true compiled-in-but-off cost.
    assert!(
        !otel::is_active(),
        "the disabled bench must run before any subscriber is installed"
    );
    c.bench_function("otel/disabled_request_span", |b| {
        b.iter(|| request_span_work(SAMPLED_TRACEPARENT, None));
    });
}

fn bench_enabled(c: &mut Criterion) {
    let exp = install_global();

    c.bench_function("otel/enabled_sampled_out_request_span", |b| {
        b.iter(|| request_span_work(UNSAMPLED_TRACEPARENT, Some(&exp)));
    });

    c.bench_function("otel/enabled_recorded_request_span", |b| {
        b.iter(|| request_span_work(SAMPLED_TRACEPARENT, Some(&exp)));
    });
}

criterion_group!(benches, bench_disabled, bench_enabled);
criterion_main!(benches);
