# OpenTelemetry Distributed Tracing (Issue #3376)

AletheiaDB emits [OpenTelemetry](https://opentelemetry.io/) spans for every
HTTP request, query execution, and write transaction, and joins them to your
application's traces via standard W3C context propagation. When a `hybrid_query`
gets slow, a trace decomposes it into its traversal, k-NN, and
temporal-reconstruction phases — an explainability artifact no incumbent graph
database produces.

This complements, and never duplicates, the metrics contract (ADR 0056) and
Tracy micro-profiling. Metrics tell you *that* p99 spiked; traces tell you
*which request, doing what, spent time where*.

## Enablement

Tracing lives behind the **`otel`** Cargo feature (which composes with
`observability`). It is compiled out entirely when the feature is absent, so a
build without `otel` pays exactly zero — the `observability::otel` module and
all of its call sites do not exist.

```toml
# Cargo.toml
aletheiadb = { version = "0.3", features = ["http-server", "otel"] }
```

Even compiled in, tracing stays **off** until initialized. The HTTP server
(`aletheia-server`) turns it on when either `ALETHEIADB_OTEL` is truthy or an
OTLP endpoint is configured:

```bash
# Point at your collector and start the server.
export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4318"
export OTEL_SERVICE_NAME="aletheiadb"
cargo run --bin aletheia-server --features http-server,otel
```

Programmatic (embedded) use installs the subscriber yourself and holds the
returned guard for the process lifetime:

```rust
use aletheiadb::observability::otel::{self, OtelConfig};

let _guard = otel::init(&OtelConfig::from_env())?; // Option<OtelGuard>
// ... run workload ...
// dropping `_guard` flushes and shuts the exporter down.
```

## Exporter and environment configuration

The exporter speaks **OTLP over HTTP/protobuf** and reads the standard
OpenTelemetry environment variables:

| Variable | Purpose | Default |
|----------|---------|---------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Collector endpoint | `http://localhost:4318` |
| `OTEL_SERVICE_NAME` | `service.name` resource attribute | `aletheiadb` |
| `OTEL_TRACES_SAMPLER` | Head sampler (see below) | `parentbased_always_on` |
| `OTEL_TRACES_SAMPLER_ARG` | Sampler ratio for the ratio samplers | `1.0` |
| `ALETHEIADB_OTEL` | Master enable switch (`1`/`true`/`yes`/`on`) | off |
| `ALETHEIADB_OTEL_CAPTURE_STATEMENTS` | Opt in to statement capture | off |

## Sampling

Head sampling is configured with `OTEL_TRACES_SAMPLER`:

| Value | Behavior |
|-------|----------|
| `always_on` | Sample every trace |
| `always_off` | Sample none |
| `traceidratio` | Sample a fraction (`OTEL_TRACES_SAMPLER_ARG`, e.g. `0.01`) |
| `parentbased_always_on` (default) | Honor the caller's decision; sample roots |
| `parentbased_always_off` | Honor the caller's decision; drop roots |
| `parentbased_traceidratio` | Honor the caller's decision; sample a fraction of roots |

**Guidance.** In production use `parentbased_traceidratio` with a low ratio
(1% is a good start): traces that arrive already-sampled from an upstream
service are always continued (so you never get half a waterfall), while
locally-rooted traces are sampled at your budget. A **sampled-out** request
skips span construction on the hot path, so raising the ratio is the only knob
that trades cost for coverage.

## Attributes and the privacy model

Spans are **safe by default**. They carry:

- `db.system.name` (`aletheiadb`), `http.request.method`, `url.path` (route, not
  query string);
- `aletheiadb.operation` — the operation name (bounded set);
- `aletheiadb.result.count` — number of entities/rows returned;
- `aletheiadb.temporal.scope` — `as_of` vs `current`;
- `aletheiadb.error.code` — the structured error code (Issue #3234) on failure;
- the existing DB child-span attributes (query kind, durability mode,
  transaction id, vector property, …).

What is **never** emitted:

- **Credentials.** API keys, the `Authorization` header, and bearer tokens are
  never read into a span. The W3C carriers only touch `traceparent` and
  `tracestate`. (A CI test captures spans and asserts the credential material
  never appears.)
- **Property values.** No node/edge property value is ever an attribute.
- **Statement text**, unless you opt in with
  `ALETHEIADB_OTEL_CAPTURE_STATEMENTS=1`. Even then only the query *template* is
  attached as `db.query.text` (following OTel database semantic conventions);
  parameters use `$`-bindings, so values are not interpolated.

## Context propagation and correlation

Incoming W3C `traceparent`/`tracestate` headers are honored, so AletheiaDB's
spans appear as children inside your caller's trace. With no incoming context,
AletheiaDB starts a fresh root trace — never a dangling child of framework
middleware.

Failures expose the active trace id for direct lookup in your backend: error
responses carry a `trace_id` body field **and** an `x-trace-id` response header.

```json
{ "success": false, "error": "Node 999 not found", "trace_id": "0af7651916cd43dd8448eb211c80319c" }
```

## Overhead

Overhead is bounded and enforced as a bench gate (`benches/otel_overhead.rs`):

- **Compiled in but disabled**: within noise of the `performance_targets`
  suite — spans are `tracing` no-ops.
- **Enabled at low sampling**: small, bounded — sampled-out requests skip span
  construction entirely.

## Find-the-slow-query walkthrough (collector + Jaeger)

A minimal local stack: an OpenTelemetry Collector receiving OTLP and forwarding
to Jaeger.

```yaml
# docker-compose.yml
services:
  jaeger:
    image: jaegertracing/all-in-one:1.60
    ports:
      - "16686:16686"   # Jaeger UI
      - "4317"          # OTLP gRPC (internal)
  otel-collector:
    image: otel/opentelemetry-collector:0.106.1
    command: ["--config=/etc/otel-collector.yaml"]
    volumes:
      - ./otel-collector.yaml:/etc/otel-collector.yaml
    ports:
      - "4318:4318"     # OTLP HTTP (AletheiaDB exports here)
    depends_on: [jaeger]
```

```yaml
# otel-collector.yaml
receivers:
  otlp:
    protocols:
      http:
        endpoint: 0.0.0.0:4318
exporters:
  otlp/jaeger:
    endpoint: jaeger:4317
    tls:
      insecure: true
service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [otlp/jaeger]
```

Run it, then start AletheiaDB pointed at the collector:

```bash
docker compose up -d
OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4318" \
OTEL_TRACES_SAMPLER="always_on" \
ALETHEIADB_AUTH_MODE=anonymous \
  cargo run --bin aletheia-server --features http-server,otel
```

Now diagnose a slow operation:

1. Open the Jaeger UI at <http://localhost:16686> and pick service `aletheiadb`.
2. Filter by the operation attribute — e.g. `aletheiadb.operation=execute_query`
   — to slice to the query shape you suspect, or sort by duration to surface the
   outliers.
3. Open a slow trace. The `aletheiadb.http.request` root span decomposes into
   child spans: `aletheiadb.query.execute`, `aletheiadb.vector.search`,
   `aletheiadb.temporal.query`, `aletheiadb.transaction.commit`. The widest
   child is the slow phase.
4. If the request failed, read `aletheiadb.error.code` on the span (or the
   `trace_id` returned in the error response) and jump straight to that trace.

For an agent request that carried a `traceparent` in, the AletheiaDB spans are
already nested inside the agent's trace — the database is no longer a black hole
in the waterfall.

## See also

- [ADR 0056 — Observability Contract](../adr/0056-observability-contract.md)
- [docs/OBSERVABILITY.md](../OBSERVABILITY.md)
- [Access control matrix](access-control-matrix.md) (no new routes are added by
  tracing; the `x-trace-id` header and `trace_id` field are additive to existing
  responses)
