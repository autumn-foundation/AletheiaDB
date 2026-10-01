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
aletheiadb = { version = "0.2", features = ["http-server", "otel"] }
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
locally-rooted traces are sampled at your budget.

For a **sampled-out** request the root span is still *constructed* (the
`tracing` layer runs the sampler at span creation, and the incoming W3C context
is still attached first so parent-based sampling can honor the caller's
decision), but it is **not recorded or exported**, and the per-request attribute
recording (operation, result count, temporal scope, error code) is **skipped** —
the hot path detects the not-recording span and does no attribute work. The
residual cost of a sampled-out request is therefore the span construction plus
the sampler decision, not the full attribute set; raising the ratio trades that
remaining headroom for coverage. See [Overhead](#overhead).

The ratio arg (`OTEL_TRACES_SAMPLER_ARG`) is clamped to `[0.0, 1.0]`; an
out-of-range or non-numeric value falls back to full sampling (`1.0`).

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

  > **Warning — enabling statement capture can leak literal values.** The
  > captured `db.query.text` is the statement string *exactly as submitted*. If
  > a caller inlines literal values into the statement (e.g.
  > `MATCH (n {ssn: '123-45-6789'})` instead of `{ssn: $ssn}`), those literals
  > land in the exported span and travel to your trace backend. Statement
  > capture is off by default for this reason. When you enable it, keep values
  > out of traces by using `$param` bindings for every value — the binding
  > names are captured, the bound values are not.

## Context propagation and correlation

Incoming W3C `traceparent`/`tracestate` headers are honored, so AletheiaDB's
spans appear as children inside your caller's trace. With no incoming context,
AletheiaDB starts a fresh root trace — never a dangling child of framework
middleware.

Failures expose the active trace id for direct lookup in your backend: error
responses carry a `trace_id` body field **and** an `x-trace-id` response header.
Since the #3234 HTTP error-envelope unification the error is the nested
`{"error":{…}}` shape, and `trace_id` remains a **top-level** sibling of `error`
(the SDK reads it top-level):

```json
{
  "error": { "code": "NOT_FOUND", "message": "Node 999 not found", "retriable": false },
  "trace_id": "0af7651916cd43dd8448eb211c80319c"
}
```

## Overhead

Overhead is bounded and measured across three regimes:

- **Compiled in but disabled** (no subscriber installed): spans are `tracing`
  no-ops and the per-request attribute sites are skipped entirely — within noise
  of the `performance_targets` suite.
- **Enabled, sampled out**: the span is constructed and the sampler runs, but
  the span is neither recorded nor exported and the per-request attribute
  recording is skipped (the hot path checks the not-recording span first). This
  is *not* zero — it is span construction plus the sampler decision — but it is
  small and bounded.
- **Enabled, recorded**: the full cost — span construction, W3C context attach,
  and attribute recording — paid only for sampled requests.

**How the gate is enforced.** The authoritative, non-flaky gate is a structural
**inertness assertion** that runs in the normal `cargo test` suite (and thus in
CI): with the `otel` feature compiled but tracing **not** initialized, it asserts
`otel::is_active() == false` and that exercising the request-span hot path
exports **zero** spans and does not panic — proving the disabled path is inert.
The criterion micro-bench (`benches/otel_overhead.rs`) measures the three
regimes above for local/manual profiling; wiring the criterion bench into the
CI benchmark workflow is a follow-up (see the PR notes). Build the bench with
`cargo bench --no-run --bench otel_overhead --features otel`.

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
