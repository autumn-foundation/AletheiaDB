# Observability

AletheiaDB's observability surface is a contract, not an exporter stack.

Enable spans and the metrics contract:

```toml
aletheiadb = { version = "0.1", features = ["observability"] }
```

Enable the `metrics` facade adapter:

```toml
aletheiadb = { version = "0.1", features = ["metrics-rs"] }
```

## Tracing

The crate emits named `tracing` spans such as:

- `aletheiadb.query.execute`
- `aletheiadb.query.hybrid`
- `aletheiadb.vector.search`
- `aletheiadb.temporal.query`
- `aletheiadb.transaction.commit`

Applications install their own `tracing` subscriber, including any
`tracing-opentelemetry` layer.

### OTLP export (feature `otel`, Issue #3376)

For a batteries-included OpenTelemetry story — an OTLP exporter, head sampling,
W3C `traceparent`/`tracestate` propagation across the HTTP surface, and trace-id
correlation in error responses — enable the `otel` feature (it composes with
`observability`):

```toml
aletheiadb = { version = "0.2", features = ["http-server", "otel"] }
```

```bash
OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4318" \
  cargo run --bin aletheia-server --features http-server,otel
```

See **[guides/otel-tracing-guide.md](guides/otel-tracing-guide.md)** for
enablement, exporter/env configuration, sampling guidance, the attribute/privacy
model, and a worked "find the slow query" walkthrough with a collector + Jaeger
compose example.

## Metrics

Metrics flow through `observability::MetricsRecorder`. The default recorder is
`NoOpMetrics`; `metrics-rs` exposes `MetricsRsRecorder` for applications that
already use the `metrics` crate.

```rust
use aletheiadb::observability::{
    self,
    TelemetryConfig,
    metrics_rs_adapter::MetricsRsRecorder,
};
use std::sync::Arc;

observability::install(
    TelemetryConfig::builder()
        .metrics(Arc::new(MetricsRsRecorder))
        .build()
);
```

The host process owns exporter setup. Install the exporter through the `metrics`
ecosystem before starting database workloads.

## Cardinality

IDs are span attributes only. Metric labels are bounded values such as
`category`, `status`, and `durability_mode`.
