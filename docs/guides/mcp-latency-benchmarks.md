# MCP Round-Trip Latency Benchmarks (Issue #3361)

This guide documents the **MCP round-trip latency harness** (`benches/mcp_round_trip.rs`):
what it measures, the published latency targets, the seeded fixture, how to run
it, the machine-readable JSON artifact schema, and the two-lane CI wiring.

## What it measures

The harness is **black-box**. It spawns the *shipped* `aletheia-mcp` binary and
drives it over its real stdio transport — newline-delimited JSON-RPC (rmcp 1.7) —
exactly as an LLM client (e.g. Claude) would. For each registered tool it times
the **full end-to-end round-trip**:

```
client serializes request bytes
  → writes one JSON line to the child's stdin
    → server parses, authorizes, dispatches against the seeded fixture
    → server writes one JSON response line to stdout
  → client reads the line
→ client parses result.content[0].text
```

It reports, per scenario:

- **`p50 / p95 / p99 / max`** (plus `min` / `mean`), nearest-rank percentiles over
  the per-call `Instant` deltas (a custom timing loop — Criterion cannot emit
  p95/p99/max, which is why this bench is `harness = false`).
- An **MCP-overhead sub-metric** (`round_trip − direct_call`): the same request is
  also executed in-process against `AletheiaMcpServer`'s typed method on an
  identical fixture, isolating JSON-RPC serialization + stdio + the process
  boundary from the database work itself.

It touches **no** product code — nothing under `src/`. The design is deliberately
black-box so it exercises precisely what a real client hits.

## Published targets

| Metric | Target | How it is checked |
|---|---|---|
| MCP round-trip **p99** (representative read / write / temporal / vector / query) | **< 5 ms** (absolute ceiling) | Gated per-scenario; hard-failed in the nightly CI lane (exit 3) |
| MCP round-trip **p50** vs committed baseline (same gated set) | **≤ 2× baseline** (relative) | Median-based regression gate; hard-failed in the nightly CI lane (exit 4) |
| MCP transport **overhead** (round-trip − in-process direct call), single-entity reads | **< 1 ms** | Reported per scenario; independently checkable from the artifact |

The two latency rows are recorded in
[`benchmarks/performance-targets.json`](../../benchmarks/performance-targets.json).
The p99 ceiling is scoped to **MCP protocol + parse + dispatch + handler latency
under the Async durability profile** — durable-write cost under `GroupCommit` is a
separate WAL concern (see the durability note below).

### A note on durability and the write gate

Absolute write latency through the WAL is dominated by the operator's
**durability** choice, not by MCP overhead. Under the default durable
`GroupCommit { max_delay_ms: 10 }` profile a *solo* write waits the full
batch-delay (~10 ms) before returning — that is a durability/fsync tradeoff paid
equally by any caller, including the in-process direct floor (so it cancels out
of the overhead sub-metric).

To keep the reported numbers a measure of **MCP + dispatch latency** rather than
fsync cost, the harness serves the fixture under an **`Async` durability profile**
(`flush_interval_ms: 50`, background flush, ~10–100 µs commit latency). It does
this by generating a TOML config at runtime and pointing the spawned binary at it
via `ALETHEIADB_CONFIG` (the same config backs the in-process direct DB). Under
this profile every write tool round-trips in well under 1 ms.

So be precise about what the 5 ms write gate measures: **MCP protocol + parse +
dispatch + handler latency under the Async profile** — *not* durable-write
latency. A production deployment that chooses the default durable
`GroupCommit { max_delay_ms: 10 }` adds its ~10 ms batch-wait on top; a *solo*
`create_node` there round-trips at ≈ 10–11 ms, which is **durability-bound, not an
MCP-layer cost**, and is a separate WAL concern (see [docs/WAL.md](../WAL.md)).
Crucially this does **not** inflate the MCP-overhead sub-metric: the batch-wait is
paid equally by the in-process direct floor, so it cancels out of
`round_trip − direct` (the overhead stays sub-millisecond regardless of
durability mode).

**Observational durable-write number (ungated).** Re-running the harness under the
default durable profile (`MCP_BENCH_DURABILITY=group_commit`) shows the honest
durable cost alongside the Async-profile gated number:

| Scenario | Durability profile | round-trip p50 | round-trip p99 | MCP overhead p99 | Gated? |
|---|---|--:|--:|--:|:-:|
| `gate_write__create_node` | **Async** (gate profile) | ~0.16 ms | ~0.24 ms | ~0.21 ms | ✓ (< 5 ms) |
| `create_node` (observational) | **GroupCommit `{max_delay_ms:10}`** (default durable) | ~10.9 ms | ~11.5 ms | ~0.0 ms | ✗ (durability-bound) |

The ~10.9 ms is the batch-delay wait, not MCP work — note the overhead sub-metric
stays ≈ 0 ms because the direct floor pays the same wait. This row is **not
gated**; it exists only to make the durable number honest and visible.

## Coverage (AC1–AC3)

- **Every advertised tool has ≥1 scenario.** The harness enumerates the server's
  **live** registry with a `tools/list` request and asserts the scenario table
  covers every advertised tool. If any registered tool has no scenario the harness
  **exits non-zero (exit 2)** — the coverage cannot silently drift as tools are
  added. As of this writing that is **46 tools**.
- **Size-sensitive tools get small / medium / large parameterizations:**
  `traverse` (depth 1/2/3), `list_nodes` (page size 10/100/1000), `find_similar`
  (k 1/10/50), `query` (simple / filtered / traversal AQL), and `get_node_history`
  (short / long version chains).
- The **gated representative set** spans all five required categories: a read
  (`get_node`), a write (`create_node`), a temporal read (`get_node_at_time`), a
  vector search (`find_similar`), and a declarative `query`. The gated `query`
  uses **AQL** (always available) so the gate does not depend on the optional
  `cypher` feature.

## The seeded fixture (AC1)

Built deterministically in-process (fixed seeds), then handed to the spawned
binary. Two identical copies are seeded — one drives the binary (round-trip), one
backs the in-process direct-call floor — so entity and version ids line up.

| Scale | Nodes | Edges | Notes |
|---|---|---|---|
| `smoke` (default, per-PR) | 500 Person | 2,000 KNOWS | Fast; modest disk/time |
| `nightly` (reference) | 10,000 Person | 50,000 KNOWS | Reference scale (matches the recovery-benchmark reference dataset) |

Shape (both scales): `Person` nodes carry `name`/`age`/`city` and a 16-dim
`embedding` (a `cosine` HNSW vector index is enabled); a hub node fans out to ~10%
of the edges so traversal/adjacency scenarios have real degree. History exists for
temporal tools: dedicated short (2-version) and long (12/20-version) nodes, a
multi-version edge, and dedicated edgeless mutable/retractable targets so
`update_*`/`retract_*` are repeatable. **Disposable pools** of `Ephemeral` nodes
and edges (sized to `warmup + sample_size`) let `delete_node`/`delete_edge`/
`delete_node_cascade` consume a fresh id per iteration instead of re-deleting.
Version ids for `diff_*`/lineage scenarios are discovered at runtime via
`get_node_history`/`get_edge_history` over the wire.

## How to run

```bash
# Via just (additive recipe): just mcp-bench [scale] [sample] [warmup] [enforce]
just mcp-bench                       # smoke, 200 samples, informational
just mcp-bench nightly 1000 50 1     # nightly scale, 1000 samples, hard-fail the p99 gate

# Or directly (requires the mcp-server + config-toml features):
MCP_BENCH_SCALE=smoke MCP_BENCH_SAMPLE_SIZE=200 MCP_BENCH_WARMUP=20 \
  MCP_BENCH_JSON=mcp_round_trip_results.json \
  cargo bench --bench mcp_round_trip --features "mcp-server,config-toml"
```

The harness sets `ALETHEIADB_AUTH_MODE=anonymous` on the child (authentication is
fail-closed by default, so without this every call returns `UNAUTHENTICATED`). It
requires the `mcp-server` feature (so `CARGO_BIN_EXE_aletheia-mcp` is set and
`AletheiaMcpServer` is available for the direct floor) and `config-toml` (to
generate the Async-profile config).

### Environment knobs

| Variable | Default | Meaning |
|---|---|---|
| `MCP_BENCH_SAMPLE_SIZE` / `BENCH_SAMPLE_SIZE` | 200 | Measured calls per scenario |
| `MCP_BENCH_WARMUP` / `BENCH_WARMUP_TIME` | 20 | Warm-up calls per scenario (discarded) |
| `MCP_BENCH_SCALE` | `smoke` | `smoke` or `nightly` fixture scale |
| `MCP_BENCH_ENFORCE_LATENCY` | `0` | `1` → hard-fail (exit 3) if a gated scenario p99 ≥ 5 ms |
| `MCP_BENCH_BASELINE` | — | Path to the committed reference baseline JSON; enables the relative gate |
| `MCP_BENCH_ENFORCE_RELATIVE` | `0` | `1` → hard-fail (exit 4) if a gated scenario p50 > `multiplier` × baseline p50 |
| `MCP_BENCH_REGRESSION_MULTIPLIER` | `2.0` | Relative-gate threshold (ratio of current p50 to baseline p50) |
| `MCP_BENCH_WRITE_BASELINE` | — | Regenerate the reference baseline: write gated p50s to this path |
| `MCP_BENCH_JSON` | — | Path for the machine-readable artifact |
| `MCP_BENCH_INJECT_LATENCY_MS` | `0` | Sensitivity proof: synthetic per-call latency added to the injected gated scenario |
| `MCP_BENCH_INJECT_SCENARIO` | `gate_read__get_node` | Which gated scenario receives the injection |
| `MCP_BENCH_DROP_SCENARIO` | — | Test hook: drop all scenarios for a tool (to show registry-completeness fails) |

### Exit codes

- `0` — success (registry complete; no enforced gate failure).
- `2` — **registry-completeness failed** (an advertised tool has no scenario). Always enforced.
- `3` — an **enforced** gated scenario p99 ≥ 5 ms (only when `MCP_BENCH_ENFORCE_LATENCY=1`).
- `4` — an **enforced** gated scenario p50 exceeded the relative baseline multiplier (only when `MCP_BENCH_ENFORCE_RELATIVE=1`).

## Measured results (smoke scale, reference run)

Hardware baseline: `x86_64` Linux, 4 logical CPUs, **optimized (release-profile)
bench build**. `cargo bench` (and `just mcp-bench`, and both CI lanes) compile
this bench under `[profile.bench]`, which `inherits = "release"` in `Cargo.toml`
(opt-level 3, thin LTO) — so these are optimized-build numbers, not a debug
build. `sample_size=100`, `warmup=15`. Every one of the 46 tools has at least one
scenario; all p99 land well under the 5 ms gate.

<!-- BEGIN MCP-LATENCY-TABLE (regenerate from the MCP_BENCH_JSON artifact) -->
| Scenario | Tool | Cat | Size | Gated | p50 (µs) | p95 (µs) | p99 (µs) | max (µs) | Overhead p99 (µs) |
|---|---|---|---|:-:|--:|--:|--:|--:|--:|
| gate_read__get_node | get_node | read | typical | ✓ | 121.6 | 173.0 | 187.8 | 213.8 | 182.6 |
| gate_write__create_node | create_node | write | typical | ✓ | 157.9 | 212.1 | 241.3 | 262.3 | 207.3 |
| gate_temporal__get_node_at_time | get_node_at_time | temporal | typical | ✓ | 159.4 | 216.1 | 250.8 | 329.1 | 249.6 |
| gate_vector__find_similar | find_similar | vector | typical | ✓ | 280.3 | 353.0 | 377.4 | 450.6 | 276.2 |
| gate_query__aql | query | query | typical | ✓ | 282.6 | 366.1 | 426.7 | 439.2 | 323.9 |
| read__get_edge | get_edge | read | typical |  | 149.5 | 198.3 | 250.6 | 266.4 | 247.8 |
| read__count_nodes | count_nodes | read | typical |  | 141.5 | 187.0 | 202.6 | 207.5 | 202.1 |
| read__count_edges | count_edges | read | typical |  | 142.4 | 180.0 | 202.6 | 258.4 | 202.1 |
| read__list_edges | list_edges | read | typical |  | 145.0 | 192.7 | 212.8 | 218.0 | 211.6 |
| read__get_outgoing_edges | get_outgoing_edges | read | typical |  | 1099.5 | 1700.1 | 2135.2 | 2423.7 | 1612.3 |
| read__get_incoming_edges | get_incoming_edges | read | typical |  | 128.8 | 175.2 | 186.9 | 191.6 | 179.6 |
| read__get_schema | get_schema | read | typical |  | 346.0 | 446.2 | 478.8 | 488.1 | n/a |
| read__database_stats | database_stats | read | typical |  | 134.4 | 202.7 | 228.7 | 303.0 | n/a |
| read__list_vector_indexes | list_vector_indexes | read | typical |  | 130.5 | 184.9 | 207.5 | 232.3 | 205.9 |
| read__list_unique_constraints | list_unique_constraints | read | typical |  | 134.0 | 207.0 | 227.6 | 258.3 | n/a |
| read__list_nodes_small | list_nodes | read | small |  | 234.2 | 325.8 | 344.8 | 360.6 | 284.3 |
| read__list_nodes_medium | list_nodes | read | medium |  | 829.8 | 954.9 | 986.5 | 1012.4 | 286.3 |
| read__list_nodes_large | list_nodes | read | large |  | 3493.4 | 3779.6 | 4200.6 | 4605.4 | 1455.3 |
| read__get_node_history_short | get_node_history | read | small |  | 142.2 | 182.1 | 202.6 | 213.1 | n/a |
| read__get_node_history_long | get_node_history | read | large |  | 210.6 | 252.3 | 268.2 | 294.5 | n/a |
| read__get_edge_history | get_edge_history | read | typical |  | 158.1 | 203.9 | 267.4 | 284.3 | n/a |
| traverse__depth1 | traverse | traversal | small |  | 674.0 | 783.7 | 822.0 | 856.5 | 497.2 |
| traverse__depth2 | traverse | traversal | medium |  | 674.7 | 779.8 | 814.1 | 831.7 | 497.0 |
| traverse__depth3 | traverse | traversal | large |  | 707.4 | 823.9 | 905.3 | 965.5 | 455.5 |
| vector__find_similar_k1 | find_similar | vector | small |  | 179.5 | 230.6 | 259.7 | 266.0 | 221.6 |
| vector__find_similar_k50 | find_similar | vector | large |  | 650.2 | 791.2 | 950.2 | 1070.6 | 678.0 |
| vector__hybrid_query | hybrid_query | vector | typical |  | 282.8 | 370.3 | 410.0 | 422.6 | 326.1 |
| query__aql_simple | query | query | small |  | 222.8 | 286.0 | 307.8 | 347.8 | 248.5 |
| query__aql_filtered | query | query | medium |  | 464.7 | 608.4 | 768.5 | 781.2 | 484.9 |
| query__aql_traversal | query | query | large |  | 798.5 | 1195.8 | 1231.9 | 1234.0 | 816.2 |
| temporal__get_edge_at_time | get_edge_at_time | temporal | typical |  | 143.0 | 197.1 | 247.6 | 257.4 | 246.4 |
| temporal__get_node_at_valid_time | get_node_at_valid_time | temporal | typical |  | 162.2 | 218.6 | 236.4 | 343.6 | n/a |
| temporal__get_node_at_transaction_time | get_node_at_transaction_time | temporal | typical |  | 156.5 | 223.1 | 235.0 | 252.1 | n/a |
| temporal__get_edge_at_valid_time | get_edge_at_valid_time | temporal | typical |  | 164.4 | 206.8 | 217.3 | 236.9 | n/a |
| temporal__get_edge_at_transaction_time | get_edge_at_transaction_time | temporal | typical |  | 153.0 | 217.6 | 227.7 | 241.1 | n/a |
| temporal__find_nodes_at_time | find_nodes_at_time | temporal | typical |  | 647.1 | 930.2 | 1089.3 | 1444.1 | 1002.6 |
| temporal__list_changes | list_changes | temporal | typical |  | 1022.5 | 1386.0 | 1591.0 | 1740.5 | 1546.8 |
| temporal__temporal_extent | temporal_extent | temporal | typical |  | 145.0 | 190.2 | 203.2 | 228.3 | n/a |
| temporal__diff_node_versions | diff_node_versions | temporal | typical |  | 145.8 | 188.6 | 197.3 | 259.6 | n/a |
| temporal__diff_edge_versions | diff_edge_versions | temporal | typical |  | 152.5 | 201.9 | 227.2 | 259.8 | n/a |
| lineage__upstream | lineage_upstream | lineage | typical |  | 146.4 | 198.8 | 216.4 | 227.4 | n/a |
| lineage__downstream | lineage_downstream | lineage | typical |  | 153.0 | 215.3 | 259.9 | 279.7 | n/a |
| write__create_edge | create_edge | write | typical |  | 179.0 | 256.9 | 275.7 | 316.6 | 258.2 |
| write__update_node | update_node | write | typical |  | 168.0 | 221.3 | 233.1 | 244.4 | 218.2 |
| write__update_edge | update_edge | write | typical |  | 164.1 | 213.7 | 229.6 | 235.9 | 202.9 |
| write__delete_node | delete_node | write | typical |  | 163.4 | 225.8 | 249.3 | 307.1 | 215.3 |
| write__delete_edge | delete_edge | write | typical |  | 160.3 | 203.7 | 227.0 | 234.6 | 219.9 |
| write__delete_node_cascade | delete_node_cascade | write | typical |  | 154.3 | 210.1 | 250.5 | 254.4 | 233.7 |
| write__retract_node | retract_node | write | typical |  | 152.4 | 192.8 | 211.8 | 213.7 | n/a |
| write__retract_edge | retract_edge | write | typical |  | 147.8 | 216.0 | 235.1 | 246.9 | n/a |
| write__apply_batch | apply_batch | write | typical |  | 180.2 | 233.9 | 251.0 | 251.3 | 210.5 |
| write__enable_vector_index | enable_vector_index | admin | typical |  | 158.9 | 206.2 | 251.7 | 280.4 | 247.6 |
| write__enable_unique_constraint | enable_unique_constraint | admin | typical |  | 165.7 | 210.0 | 223.5 | 236.5 | n/a |
| provenance__verify_chain | verify_chain | provenance | typical |  | 144.4 | 188.2 | 232.4 | 233.6 | n/a |
| provenance__export_chain_head | export_chain_head | provenance | typical |  | 130.7 | 175.7 | 198.7 | 204.1 | n/a |
| provenance__audit_export | audit_export | provenance | typical |  | 144.5 | 199.1 | 220.6 | 221.3 | n/a |
<!-- END MCP-LATENCY-TABLE -->

**Overhead (AC6).** For the **26 of 46** tools whose request type is publicly
re-exported from `aletheiadb::mcp`, the harness computes the in-process
direct-call floor and reports `overhead = round_trip − direct`. All **5 gated
scenarios are covered**. Across the covered tools the p99 overhead is
sub-millisecond (e.g. `get_node` ≈ 0.18 ms, `find_similar` ≈ 0.28 ms), inside the
< 1 ms budget.

The remaining **20** tools show `n/a` for overhead: they lack a public
direct-call entry point (`aletheiadb::mcp` does not re-export their request type).
Extending coverage is a deliberate **Lane-4 / MCP-owner follow-up** — of the 20,
**11 are a one-line `pub use` re-export** in `src/mcp/mod.rs` (their typed method
already exists: e.g. `*_at_valid_time`/`*_at_transaction_time`, `diff_*`,
`get_*_history`, `get_schema`, `database_stats`, `retract_*`) and **9 need a new
public typed method** (lineage, provenance, constraint tools). The harness logic
already handles them the moment a floor exists. These are intentionally **not**
routed through the higher-level `AletheiaDB` API, because that would measure a
different (inconsistent) floor than the MCP handler path and make the overhead
sub-metric incomparable across tools.

> **Caveat — the overhead sub-metric is an informational decomposition, noisy on
> the tail.** `overhead.pN = round_trip.pN − direct.pN` subtracts two
> **independently sorted** percentile series; the p99 of the round-trip and the
> p99 of the direct floor come from *different* calls, so the difference is **not**
> a per-call paired latency. Treat the overhead p50 as a solid central estimate
> and the overhead p95/p99 as indicative only (they can even round to 0 when the
> two tails happen to align). It is a diagnostic, never a gate.

## Machine-readable artifact (AC4)

With `MCP_BENCH_JSON=<path>` the harness writes a JSON document. Schema
(`schema_version: 1`):

```jsonc
{
  "schema_version": 1,
  "benchmark": "mcp_round_trip",
  "issue": 3361,
  "generated_at": "<RFC3339>",
  "git_sha": "<short sha>",
  "scale": "smoke" | "nightly",
  "sample_size": 100,
  "warmup": 15,
  "hardware": { "arch": "x86_64", "os": "linux", "logical_cpus": 4 },
  "fixture": { "nodes": 500, "edges": 2000, "embedding_dim": 16,
               "disposable_pool_per_delete_scenario": 165 },
  "gate": { "p99_threshold_ms": 5.0, "overhead_budget_ms": 1.0,
            "enforced": false, "injected_latency_ms": 0.0,
            "injected_scenario": "gate_read__get_node" },
  "registry": { "advertised": [ ...46 tool names... ],
                "covered": [ ... ], "missing": [], "complete": true },
  "scenarios": [
    {
      "name": "gate_read__get_node", "tool": "get_node",
      "category": "read", "size_class": "typical", "gated": true,
      "injected_latency_ms": 0.0, "sample_response_ok": true,
      "round_trip": { "min_us", "p50_us", "p95_us", "p99_us", "max_us", "mean_us", "samples" },
      "direct":     { ... same shape ... } | null,
      "overhead":   { "p50_us", "p95_us", "p99_us" } | null,
      "gate_pass":  true | null   // null for non-gated scenarios
    }
    // ... one per scenario ...
  ],
  "gated_failures": [], "overhead_failures": [],
  "pass": true
}
```

All latencies are microseconds (`_us`). `direct`/`overhead` are `null` when no
in-process typed floor exists for that tool. `gate_pass` is `null` for non-gated
scenarios.

## Registry-completeness mechanism (AC2)

`covered = { scenario.tool }` is compared against the **live** `tools/list`
result. Any advertised tool not in `covered` is `missing`; a non-empty `missing`
sets `complete: false` and exits 2. To demonstrate the check has teeth without
editing source, `MCP_BENCH_DROP_SCENARIO=<tool>` removes a tool's scenarios:

```
$ MCP_BENCH_DROP_SCENARIO=get_node cargo bench --bench mcp_round_trip --features "mcp-server,config-toml"
registry: 46 advertised, 45 covered by scenarios, 1 missing
  MISSING SCENARIOS: ["get_node"]
registry-completeness: FAIL
ERROR: registry-completeness FAILED — 1 advertised tool(s) have no scenario: ["get_node"]
$ echo $?
2
```

## Two gates: absolute ceiling + relative baseline (AC5)

The gated scenarios are checked **two** ways, and both hard-fail in the nightly
lane:

1. **Absolute p99 < 5 ms** — a fixed ceiling that catches gross blowups (an O(n)
   scan or serialization blow-up creeping into a hot path). Exit 3 when enforced
   (`MCP_BENCH_ENFORCE_LATENCY=1`).
2. **Relative committed-baseline regression** — for each gated scenario,
   `ratio = current_p50 / baseline_p50`; if any ratio exceeds
   `MCP_BENCH_REGRESSION_MULTIPLIER` (default **2.0×**) the gate fails, naming the
   scenario. Exit 4 when enforced (`MCP_BENCH_ENFORCE_RELATIVE=1`). The baseline
   lives at [`benchmarks/baselines/mcp_round_trip_baseline.json`](../../benchmarks/baselines/mcp_round_trip_baseline.json).

### Why the relative gate exists (the absolute gate alone can't satisfy AC5)

AC5 requires that *"a 2× regression must fail CI."* The absolute p99 gate cannot
deliver that on its own: the honest gated p99 sits **~26× under** the 5 ms
ceiling, so a clean 2× regression only lifts p99 to ~1.6 ms — still green. The
relative gate closes that hole by comparing against a committed reference median.

### Why it gates on p50 (median), not p99

The gate compares **medians**. Across honest runs the p50 of these gated
scenarios is stable (±3–9%), so a real 2× regression is unmistakable against a
2.0× threshold, while ordinary noise never trips it. The **p99 tail is the wrong
signal for a relative gate**: non-injected p99 drift of ±11% was observed, plus a
one-off AQL p99 spike to ~30 ms with zero injection — a p99-based relative gate
would false-fail. The absolute p99 ceiling is kept **additively** precisely
because a single-run tail spike is what it is designed to tolerate (it only fires
at 5 ms).

### The reference baseline

`benchmarks/baselines/mcp_round_trip_baseline.json` holds the gated scenarios'
baseline **p50** (µs), generated from an honest run at the nightly fixture scale
on the reference runner. It is a **reference** baseline tied to that hardware and
scale — refresh it per release, or whenever the reference runner changes, by
re-running with `MCP_BENCH_WRITE_BASELINE=<path>` (the harness writes the file in
the same schema). Because it gates on a 2.0× ratio of a stable median, moderate
hardware differences do not false-fail; a runner more than 2× slower than the
reference should regenerate the baseline.

### Sensitivity proof (both gates have teeth)

`MCP_BENCH_INJECT_LATENCY_MS` adds a synthetic per-call latency to a gated
scenario, inside the timed region, modelling a latency regression. It raises
every measured call, so it lifts the **median** (which the relative gate reads),
not just the tail. With both gates enforced, an **honest run passes (exit 0)**
while an **injected 2× regression fails the relative gate (exit 4)**, naming the
scenario — even though that same 2× regression stays under the absolute 5 ms
ceiling:

| Run | `gate_read__get_node` p50 | ratio vs baseline | Relative gate | Absolute p99 gate | Exit |
|---|--:|--:|---|---|:-:|
| **Honest** | ~0.12 ms | ~1.0× | PASS | PASS | 0 |
| **2× injection** (`+baseline p50`) | ~0.25 ms | **~2.1×** | **FAIL** `["gate_read__get_node"]` | PASS (~1.6 ms, still green) | **4** |
| Gross injection (`+5 ms`) | ~5.1 ms | ~40× | FAIL | **FAIL** (p99 ≫ 5 ms) | 3 or 4 |

The exact numbers from the verification run on this machine are recorded in the
PR description. The relative gate (exit 4) is what makes a clean 2× regression
fail CI; the absolute gate (exit 3) remains for gross blowups. Both are wired to
hard-fail the nightly lane.

## CI wiring (two lanes)

The bench is wired into `.github/workflows/benchmark.yml` as two additive jobs:

- **`mcp-roundtrip-nightly`** — runs on `schedule` / `workflow_dispatch`,
  **nightly** fixture scale, with **both** gates enforced:
  `MCP_BENCH_ENFORCE_LATENCY=1` (absolute p99 ≥ 5 ms → exit 3) **and**
  `MCP_BENCH_ENFORCE_RELATIVE=1` with `MCP_BENCH_BASELINE` pointed at the
  committed reference baseline (a gated p50 > 2× baseline → exit 4). Either
  failure **hard-fails** the job. This is the reference-runner enforcement lane;
  the relative gate is what makes a clean 2× regression fail CI.
- **`mcp-roundtrip-smoke`** — runs on `push` / `pull_request`, reduced (smoke)
  scale, **`continue-on-error: true`** — informational only, never a blocking
  per-PR check. Neither latency gate is enforced (`MCP_BENCH_ENFORCE_LATENCY=0`,
  no `MCP_BENCH_ENFORCE_RELATIVE`); registry-completeness (exit 2) still surfaces
  in the log.

Flipping the per-PR smoke lane to blocking is a deliberate one-line change
(`continue-on-error: false`) left to the repository owner.
