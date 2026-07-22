# Engine-Lane Per-Query Resource Limits (Issue #3368)

This is the **query-engine** lane of #3368 — the counterpart to the
[HTTP lane](http-query-limits.md) and the MCP `query`-tool lane
([mcp-query-tool.md](mcp-query-tool.md#per-query-resource-limits-issue-3368)).
It enforces limits **inside** the query executor's pull-based iterator pipeline,
so an over-limit query is **cooperatively cancelled** — the pipeline stops
pulling rows and returns a structured error — rather than being answered by an
outer thread-race that lets the underlying computation run to completion in the
background.

Because every surface that runs a graph/AQL/Cypher query routes through the
executor, this lane is what makes the caller-facing timeouts on the other
surfaces *actually stop doing work*.

## The three dimensions

| Dimension | Token (`details.dimension`) | Bounds | Over-limit |
|-----------|------------------------------|--------|-----------|
| **Wall-clock timeout** | `wall_clock_timeout` | time to produce the result stream | `QueryError::ResourceExhausted`, **`retriable: true`** |
| **Result rows** | `result_rows` | rows pulled through the guard | `QueryError::ResourceExhausted`, `retriable: false` |
| **Memory budget** | `memory_bytes` | estimated working memory materialized (see below) | `QueryError::ResourceExhausted`, `retriable: false` |

Only a wall-clock timeout is retriable: a read is safe to re-run and a
backed-off retry may land inside budget. A row or memory breach is deterministic
— the same request reproduces the same oversized result — so it is a fault to
**repair** (tighten the query), not retry.

### What "memory budget" measures

The memory dimension is an **honest proxy**, not allocator-level accounting
(the same philosophy as the MCP surface's response-byte proxy). As each row is
pulled through the guard, `estimate_row_bytes` adds a cheap, allocation-free
estimate of that row's materialized size — `size_of::<QueryRow>()` plus the
O(1) cached serialized size of any property maps the row carries (nodes/edges,
path entries, multi-variable bindings, aggregate columns). The budget bounds the
**cumulative** estimate across the scan. This deliberately bounds the dominant,
caller-attributable cost (the rows the query materializes) without the overhead
or fragility of instrumenting every allocation in the executor. True
per-allocation accounting and a spill-to-disk path are a documented follow-up.

## Rust builder API

```rust
use std::time::Duration;
use aletheiadb::AletheiaDB;

let db = AletheiaDB::new()?;
let results = db.query()
    .start(alice_id)
    .traverse("KNOWS")
    .with_timeout(Duration::from_millis(250)) // wall-clock budget (0 = unlimited)
    .with_max_rows(10_000)                     // protective row cap
    .with_memory_budget(64 * 1024 * 1024)      // 64 MiB working-memory cap
    .execute(&db)?;

// The stream is LAZY: the guard fires while you consume it.
for row in results {
    let row = row?; // an over-limit row yields Err(QueryError::ResourceExhausted { .. })
}
```

Each override is folded against the database's operator ceiling (below). An
override **above** the ceiling — or requesting unlimited (`0`) under a finite
ceiling — is rejected with `QueryError::InvalidParameter` (→ MCP/HTTP
`INVALID_ARGUMENT`) **before any work happens**, and counted as an override
rejection.

## Configuration: default / override / ceiling

Every dimension has a server **default**, a per-call **override** (the builder
methods above), and an operator **hard ceiling** the override cannot exceed —
configured on the unified config and mirroring the HTTP/MCP merge semantics
byte-for-byte (`0` means "unlimited"; a ceiling of `0` means "no ceiling"):

```rust
use aletheiadb::{AletheiaDB, AletheiaDBConfig};
use aletheiadb::query::limits::EngineQueryLimitsConfig;

let config = AletheiaDBConfig::builder()
    .query_limits(EngineQueryLimitsConfig {
        enabled: true,
        default_timeout_ms: 30_000,   max_timeout_ms: 300_000,
        default_max_result_rows: 1_000_000, max_result_rows: 10_000_000,
        default_max_memory_bytes: 0,  max_memory_bytes: 0, // memory default-OFF, opt-in
    })
    .build();
let db = AletheiaDB::with_unified_config(config)?;
```

`EngineQueryLimitsConfig::default()` is the above (protective but generous, so no
existing query behavior changes). `EngineQueryLimitsConfig::disabled()` turns off
all enforcement — and, as on the other surfaces, **per-call overrides are then
ignored** (a disabled config is fully unlimited). Consequently the builder
overrides (`with_max_rows`, …) only take effect under an *enabled* config; the
default `AletheiaDB::new()` is enabled, so they work out of the box.

## Termination is prompt and clean (cooperative cancellation)

The guard checks the deadline **before pulling each row**, so an over-budget
query performs no further work; on a breach it returns the structured error and
"fuses" (every later `next()` yields `None`). Because the pipeline is lazy and
pull-based, stopping the pull releases the query's resources immediately — there
is **no orphaned background thread** continuing the computation. Read paths only:
the guard wraps read queries; write-path limits remain governed by existing
transactional abort semantics, so a timeout never leaves a partial write.

### Grace bound and the row-granularity floor

Cancellation is a **row-boundary** event. To keep the fast path cheap, the
deadline is polled on **every row for the first 4096 rows**, then amortized to a
**64-row stride** thereafter:

- A *slow, low-row-count* query (e.g. a few deep temporal reconstructions) is in
  the every-row regime, so it is cut at the first row boundary past its deadline.
- A *many-fast-rows* scan (the pathological class this issue targets — deep
  traversals, cross products) is amortized, so it can overshoot by at most the
  time to produce 64 more rows: tens of microseconds, far inside the ≤10% grace
  bound even for a 100 ms timeout.
- The inherent floor: a query whose *individual rows* each exceed the grace
  budget can overshoot by one such row regardless of stride — cooperative,
  row-granular cancellation cannot interrupt a single in-flight row.

## Structured errors (#3234)

`QueryError::ResourceExhausted { dimension, limit, consumed, retriable }` maps to
the `RESOURCE_EXHAUSTED` code on both the MCP and HTTP surfaces, carrying
`details: { dimension, limit, consumed }`. This is exactly the payload a caller
(an LLM) needs to self-correct: read the dimension and the consumed-vs-limit gap,
tighten the query (smaller depth / `LIMIT` / window), and re-issue. See the
end-to-end self-correction test in `tests/query_resource_limits.rs`.

## Observability

Per-dimension, process-lifetime counters are exposed via
`AletheiaDB::query_limit_counters()` → `{ wall_clock_timeout, result_rows,
memory_bytes, override_rejected }` (relaxed atomic reads, no locks — safe to poll
frequently). They are deliberately **not** folded into the storage-tier
`DatabaseStats` (which stays a pure storage snapshot the MCP thin-aggregator
contract asserts); instead the MCP `database_stats` tool surfaces them in the
`engine` sub-object of its additive `resource_limits` block — kept distinct from
that block's top-level MCP-surface counters (read-tool byte cap, etc.) so the two
enforcement families are never conflated or double-counted.

## Performance: what the guard costs, and what it does not

The guard is installed **only on the `execute_query` path** (the builder / AQL /
Cypher query pipeline). It is **not** on the direct current-state or temporal
APIs (`get_node`, `get_outgoing_edges`, `get_node_at_time`, …) that the standard
`benches/current_state.rs` and `benches/performance_targets.rs` suites — and the
issue's "single-hop p99 < 1µs" target — measure. Those paths are byte-for-byte
unchanged, so the standard suite sees **0% regression** and the < 1µs single-hop
target is unaffected.

On the query-pipeline path itself, `benches/query_resource_limits.rs` measures
the guard-enabled-vs-disabled delta directly. Enforcement adds a small per-query
setup cost and a per-row deadline poll (amortized on large scans as described
above); the benchmark is the honest, reproducible record of that cost and the
guard against regressing it. The zero-allocation fast path still applies when a
query is executed under a fully-unlimited (`disabled()`) config: no
`ResourceGuardIterator` is constructed at all.

## Coverage and documented exclusions

| Aspect | Rust engine | MCP `query` tool | HTTP `/query` |
|--------|:-----------:|:----------------:|:-------------:|
| Wall-clock timeout (cooperative) | ✅ | ✅ (worker self-cancels; race is the caller-facing reporter) | outer `tokio::timeout` bounds the response; the engine guard backstops the abandoned worker at the operator **default** limit (per-call not yet threaded — follow-up) |
| Result-row cap | ✅ (builder) | ✅ (truncation contract) | ✅ |
| Memory budget | ✅ (default/override/ceiling) | ✅ (default-off, from MCP config) | ⏳ follow-up (output byte cap is the current proxy) |
| Default / override / ceiling | ✅ | override on `query` tool | ✅ |
| Structured `RESOURCE_EXHAUSTED` | ✅ | ✅ | ✅ |
| Per-dimension counters | ✅ (`query_limit_counters`) | ✅ (`database_stats`) | span attributes |

**Documented exclusions / follow-ups:** HTTP-surface memory dimension and HTTP
cooperative-cancellation parity; true per-allocation memory accounting and
spill; per-call `limits` overrides on the MCP read tools (server-defaults only
today).

## Tests & benchmarks

- `tests/query_resource_limits.rs` — builder row/timeout/memory caps, the
  operator-ceiling override rejection + counter, default no-op, and the AC5
  self-correction + retriability contract.
- `tests/query_resource_limits_soak.rs` — AC8 neighbor protection: a
  pathological stream (all three dimensions) is bounded while a concurrent
  well-behaved stream keeps succeeding.
- `src/query/limits.rs`, `src/query/executor/iterators.rs` — unit tests for the
  merge semantics, the row-byte estimator, and the guard (past-deadline
  short-circuit, row/memory caps, counter recording, unlimited pass-through).
- `benches/query_resource_limits.rs` — guard-enabled-vs-disabled overhead on a
  single hop and a larger scan (AC6 evidence).
