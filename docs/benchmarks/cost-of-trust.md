# The Cost of Trust — Provenance-Enabled Write Throughput (Issue #3383)

AletheiaDB lets a writer record *why* a fact is trustworthy — attaching
[**provenance**](../guides/provenance-hash-chain.md) (`source` / `confidence` /
`note`, Issue #3224) and enforcing a [**uniqueness constraint**](../guides/mcp-query-tool.md)
(Issue #3218). Neither is free. This page publishes what they cost on the write
path, how the numbers are produced reproducibly, and the CI policy that keeps
the overhead from regressing.

The benchmark lives at
[`benches/provenance_write_throughput.rs`](../../benches/provenance_write_throughput.rs).

## The matrix

Four configurations are measured **back-to-back in the same process run**, so
every ratio is taken against a baseline captured on the same hardware in the
same run — immune to cross-machine and cross-CI-runner timing variance:

| Config              | Provenance (#3224) | Unique constraint (#3218) | What it isolates |
|---------------------|:------------------:|:-------------------------:|------------------|
| `baseline`          |         no         |            no             | reference, no trust features |
| `provenance_only`   |        yes         |            no             | cost of serializing a provenance bundle per write |
| `constraint_active` |         no         |            yes            | cost of the per-write uniqueness reservation check |
| `composed`          |        yes         |            yes            | both together — the full "trusted write" cost |

For each config the suite reports sustained **throughput (ops/sec)**,
single-write **latency p50 / p99**, and the throughput **ratio vs the same-run
baseline**.

All four use a **durable GroupCommit** database (WAL fsync + index persistence,
`DurabilityMode::GroupCommit { max_delay_ms: 10, max_batch_size: 200 }`), never
the ephemeral `AletheiaDB::new()`, so the numbers reflect real fsync-batched
commit cost. Each config runs against its own fresh `TempDir`.

## The fixture (AC2 — reproducibility)

The workload is fully deterministic: a fixed-seed `rand::rngs::SmallRng`
(`WORKLOAD_SEED = 0x3383C0570F7405`) drives the identical write sequence on
every run. Each write creates a `"Bench"` node with a fixed property shape:

| Property  | Type   | Size / value |
|-----------|--------|--------------|
| `uid`     | i64    | process-unique monotonic id (satisfies the constraint, so constraint configs never error) |
| `seq`     | i64    | seeded pseudo-random value |
| `name`    | String | 16 ASCII bytes |
| `payload` | String | 64 ASCII bytes |

The provenance bundle attached by `provenance_only` / `composed`:

| Field        | Value |
|--------------|-------|
| `source`     | `"prov-write-bench-3383"` |
| `confidence` | `0.95` |
| `note`       | 64 ASCII bytes |

Every config writes the **identical property shape** — only the provenance
attachment and the constraint declaration differ — so the measured delta is
exactly the cost of the trust feature and nothing else.

### How to run

```bash
# Full statistical run (Criterion arms + matrix table + JSON artifact)
cargo bench --bench provenance_write_throughput

# Fast reduced-scale smoke
BENCH_SAMPLE_SIZE=10 BENCH_MEASUREMENT_TIME=1 BENCH_WARMUP_TIME=1 \
  PROV_BENCH_WRITES=120 cargo bench --bench provenance_write_throughput

# Self-gating mode (assert the ratio bounds; non-zero exit on violation)
PROV_BENCH_GATE=1 cargo bench --bench provenance_write_throughput
```

Environment knobs:

| Variable | Meaning | Default |
|----------|---------|---------|
| `PROV_BENCH_WRITES` | measured writes per config in the matrix | 240 (120 in gate mode) |
| `PROV_BENCH_GATE` | `1` applies the same-run ratio gate (panics → non-zero exit) | off |
| `PROV_BENCH_INJECT_REGRESSION` | fraction (e.g. `0.25`) to synthetically deflate `composed` throughput, to prove the gate fails | `0.0` |
| `PROV_BENCH_JSON_OUT` | path for the JSON results artifact | `$CARGO_TARGET_DIR/provenance_write_throughput.json` |
| `PROV_BENCH_RECOVERY_NODES` / `_EDGES` | recovery spot-check scale | 2000 / 4000 |
| `BENCH_SAMPLE_SIZE` / `BENCH_MEASUREMENT_TIME` / `BENCH_WARMUP_TIME` | Criterion tuning (shared harness) | 50 / 5 / 3 |

## Measured numbers (reference run)

Captured in this sandbox at reduced scale
(`BENCH_SAMPLE_SIZE=10 BENCH_MEASUREMENT_TIME=1 PROV_BENCH_WRITES=120`). The
GroupCommit single-writer path is bounded by the ~10 ms batch timer, so
per-write latency clusters near the batch delay and throughput ratios sit close
to 1.0 — an honest "fsync dominates the commit, trust features are cheap"
result. Absolute throughput is hardware- and scale-dependent; the **ratios** are
the durable, portable signal.

<!-- REFERENCE_NUMBERS_START -->
| Config              | Throughput (ops/s) | p50 (µs) | p99 (µs) | Ratio vs baseline | Bound |
|---------------------|-------------------:|---------:|---------:|------------------:|------:|
| `baseline`          |             88.5   | 10820.0  | 11421.0  |            1.000  |  —    |
| `provenance_only`   |             92.3   | 10806.8  | 11135.6  |            1.042  | ≥0.85 |
| `constraint_active` |             92.3   | 10811.4  | 11146.3  |            1.043  | ≥0.85 |
| `composed`          |             92.3   | 10803.2  | 11148.7  |            1.043  | ≥0.80 |
<!-- REFERENCE_NUMBERS_END -->

Read / recovery spot-checks from the same run: current-state read of a
provenance-carrying node ≈ **43 ns** (target < 1µs p99); temporal
reconstruction of a superseded provenance-carrying version ≈ **154 ns**
(target < 10ms); reopen/recovery of a provenance + constraint dataset at the
reduced 2000-node / 4000-edge sandbox scale ≈ **1.05 s** (reference scale 10K /
50K, target < 5s).

All four configs clear their bounds with wide margin: under GroupCommit the
single-writer commit is bounded by the ~10 ms fsync batch timer, so the extra
provenance serialization and constraint check are amortized into noise — the
"cost of trust" is real but small. In this run the trust-enabled configs even
measured *above* baseline (ratio ≈ 1.04); at this scale the same-run ratios sit
within a few percent of 1.0 in either direction, which is exactly why the gate
bounds (≥ 0.80 / ≥ 0.85) leave generous headroom for timing noise on shared
runners rather than asserting a tight equality. An earlier reduced-scale run
recorded `composed` at ratio 0.952 with a p99 of ~26 ms (occasional batch-timer
alignment on the feature-heaviest write) — still comfortably above the 0.80
bound.

> Reference hardware note: these were captured on a shared CI-class sandbox
> VM, not a dedicated performance rig. Treat absolute throughput as indicative
> only; the same-run ratios are what the gate and this page assert.

### Read / recovery spot-checks

- **AC4 — reads unaffected by provenance.** `provenance_read_spotchecks`
  benchmarks a current-state single-hop `get_node` on a provenance-carrying
  node (target **< 1µs p99**) and a temporal reconstruction of a
  provenance-carrying superseded version via `get_node_at_time` (target
  **< 10ms**).
- **AC5 — recovery of a trusted dataset.** `provenance_recovery` populates a
  durable DB with provenance-carrying nodes and edges under an active
  uniqueness constraint, drops it, and times the reopen. The default sandbox
  scale is reduced (2000 nodes / 4000 edges); the **reference scale is 10K
  nodes / 50K edges with a < 5s target** (the medium-dataset recovery budget).
  The scheduled CI lane runs the arm at the reduced default; the reference
  scale is reproduced by setting `PROV_BENCH_RECOVERY_NODES=10000
  PROV_BENCH_RECOVERY_EDGES=50000` (also the configuration the dedicated
  performance rig uses).

## Overhead bounds and the CI gate (AC3 / AC6 / AC7)

The self-gate (`PROV_BENCH_GATE=1`) computes the same-run ratios and **fails
with a non-zero exit, naming the offending row**, when any config falls below
its declared bound:

| Config              | Bound (ratio vs same-run baseline) |
|---------------------|:----------------------------------:|
| `provenance_only`   | ≥ 0.85 |
| `constraint_active` | ≥ 0.85 |
| `composed`          | ≥ 0.80 (Issue #3383 hard success metric) |

These bounds are also published in
[`benchmarks/performance-targets.json`](../../benchmarks/performance-targets.json)
as `provenance_write_composed_ratio`, `provenance_only_write_ratio`, and
`constraint_active_write_ratio`.

### CI policy (two lanes)

- **Scheduled bench lane** (weekly/manual cron): runs the full
  `provenance_write_throughput` target **with the gate enabled**
  (`PROV_BENCH_GATE=1`). A gate failure hard-fails that job as an *alert* — it
  does not block any pull request.
- **Per-PR smoke**: runs a reduced-scale invocation
  (`PROV_BENCH_WRITES=120`, no gate) as **informational / non-required**. It
  never fails the PR. Shared-runner timing noise must not become a spurious
  merge blocker.

Flipping the per-PR smoke from informational to blocking is a one-line change
once a week or two of scheduled-lane signal proves the gate is stable on the
runner — and that flip is an operator decision, not part of this change.

## JSON results artifact (AC6)

Every matrix run writes a machine-readable JSON artifact to
`$PROV_BENCH_JSON_OUT` (default `$CARGO_TARGET_DIR/provenance_write_throughput.json`).
Schema `aletheiadb.provenance_write_throughput.v1`:

```json
{
  "schema": "aletheiadb.provenance_write_throughput.v1",
  "issue": 3383,
  "durability_mode": "GroupCommit{max_delay_ms:10,max_batch_size:200}",
  "workload_seed": "0x3383c0570f7405",
  "writes_per_config": 120,
  "gated": true,
  "fixture": {
    "label": "Bench",
    "unique_property": "uid",
    "name_bytes": 16,
    "payload_bytes": 64,
    "provenance": { "source": "prov-write-bench-3383", "confidence": 0.95, "note_bytes": 64 }
  },
  "configs": [
    {
      "config": "baseline",
      "throughput": 0.0,
      "p50_us": 0.0,
      "p99_us": 0.0,
      "ratio_vs_baseline": 1.0,
      "bound": 1.0,
      "pass": true
    }
  ]
}
```

Each entry in `configs` carries `throughput` (ops/sec), `p50_us`, `p99_us`,
`ratio_vs_baseline`, the declared `bound`, and the boolean `pass`
(`ratio_vs_baseline >= bound`; always `true` for `baseline`). A CI job can
publish or diff this artifact without parsing benchmark stdout.
