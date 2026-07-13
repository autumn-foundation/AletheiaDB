# The Cost of Trust — Provenance-Enabled Write Throughput (Issue #3383)

AletheiaDB lets a writer record *why* a fact is trustworthy — attaching
[**provenance**](../guides/provenance-hash-chain.md) (`source` / `confidence` /
`note`, Issue #3224), enforcing a [**uniqueness constraint**](../guides/mcp-query-tool.md)
(Issue #3218), sealing writes into a [**tamper-evident hash chain**](../guides/provenance-hash-chain.md)
(Issue #3351), and declaring [**fact-to-fact derivation lineage**](../guides/derivation-lineage.md)
(Issue #3371). None of it is free. This page publishes what these features cost
on the write path, how the numbers are produced reproducibly, and the CI policy
that keeps the overhead from regressing.

The benchmark lives at
[`benches/provenance_write_throughput.rs`](../../benches/provenance_write_throughput.rs).

## Two measurement regimes — and which one is the gate

A single-writer **GroupCommit** database is bounded by the ~10 ms fsync **batch
timer**: the writer blocks on the timer and is otherwise idle, so per-write CPU
cost *up to ~10 ms* is completely absorbed with **zero** throughput impact.
Gating write throughput on that regime would be a rubber stamp — a realistic
single-digit-microsecond trust-path regression is roughly **1000×** below the
masking window and would be invisible to the gate. (An unmodified single-writer
GroupCommit run is also prone to *spurious* ratio failures from batch-timer
alignment noise, so it is simultaneously blind to real regressions and prone to
false alarms.)

So the suite measures **two regimes** per config:

| Arm | Durability mode | What it shows | Gated? |
|-----|-----------------|---------------|:------:|
| **CPU-bound** | `Async { flush_interval_ms: 1 }`, **interleaved multi-pass, solo DB per config** | the 1 ms flush drains the WAL ring fast enough that the single writer never blocks on flush backpressure, so per-write **CPU cost** of the trust feature is the throughput bottleneck (~100K+ writes/sec) | **YES** — the ratio bounds gate on this arm |
| **Latency** | `GroupCommit { max_delay_ms: 10, max_batch_size: 200 }` | single-writer **p50 / p99** latency (fsync-batch-timer-dominated) | **NO** — observation only |

Two design details make the CPU-bound arm both *sensitive* and *stable*:

- **1 ms flush, not the default 100 ms.** At the default flush interval the
  single writer fills the WAL ring and then *blocks on the background flush
  thread's drain rate* (~3K writes/sec) — that is **flush-bound**, not
  CPU-bound, and an injected per-write CPU cost is completely invisible (it just
  fills idle wait). A 1 ms flush keeps the drain ahead of the writer, so
  throughput tracks writer CPU (~100K+ writes/sec, the issue's high-throughput
  regime).
- **Interleaved multi-pass, one DB alive at a time.** Each `(pass, config)` cell
  builds a fresh solo DB, times `CPU_ARM_BATCH` (5000) writes, and drops it,
  repeating for `writes / batch` passes. This (a) cancels the shared VM's speed
  drift — each config is measured in many temporally-adjacent slices, not one
  long slice, so the same-run ratio reflects per-write cost rather than which
  config ran during a fast/slow patch; and (b) keeps exactly one WAL flush
  thread / chain sealer alive at any instant (concurrent per-config flush
  threads otherwise create chaotic contention that lets a heavier config
  spuriously *out*-measure a lighter one).

The CPU-bound arm is where a real trust-path regression is *detectable*: the
[injection acceptance test](#the-injection-acceptance-test-proving-the-gate-catches-a-real-regression)
below shows a real **5 µs/write** busy-spin injected into the composed write
path **drops the CPU-bound composed ratio from ~0.92 to ~0.52 and trips the
gate**, while the very same injection barely moves the GroupCommit p50/p99 — the
masking effect, made visible.

## The matrix

Six configurations are measured **back-to-back in the same process run**, so
every ratio is taken against a baseline captured on the same hardware in the
same run — immune to cross-machine and cross-CI-runner timing variance:

| Config              | Trust feature | CPU-bound bound |
|---------------------|---------------|:---------------:|
| `baseline`          | none (reference) | — |
| `provenance_only`   | #3224 provenance bundle per write | ≥ 0.85 |
| `constraint_active` | #3218 per-write uniqueness reservation check | ≥ 0.85 |
| `chain_active`      | #3351 tamper-evident provenance hash chain | **observation (ungated)** |
| `lineage_active`    | #3371 fact-to-fact derivation lineage per write | ≥ 0.80 |
| `composed`          | provenance + constraint together | ≥ 0.80 |

For each config the suite reports the CPU-bound arm's sustained **throughput
(ops/sec)** and **ratio vs the same-run baseline** (the gated signal), plus the
GroupCommit arm's single-write **p50 / p99** latency (an ungated observation).

### What `composed` folds in — and how chain / lineage / auth are handled

`composed` = **provenance (#3224) + uniqueness constraint (#3218)**: the two
write-path trust features whose cost the CPU-bound arm measures as genuine
**per-write CPU**, and which compose cleanly in a single public write
(provenance is a per-write `WriteRequestOptions`, the constraint a per-label
toggle). Three other shipped trust features are handled specially, for honest,
documented reasons:

- **Hash chain (#3351)** is its own `chain_active` row, reported as an
  **observation but not hard-gated here** (and not in `composed`). The chain is
  an async SHA-256 sealer sidecar; at the CPU-bound arm's ~100K+ writes/sec the
  **sealer** — not the writer's per-write enqueue CPU — becomes the throughput
  ceiling on a slow shared core, so the CPU-arm ratio reflects *sealer
  throughput* (observed ~0.63–0.84 here), not per-write cost. Hard-gating it on
  this regime would spuriously fail on sandbox hardware where the sealer is
  slow. The chain's **≥ 0.90 overhead metric is owned and gated by the dedicated
  [`provenance_chain`](../../benches/provenance_chain.rs) bench (#3351)** in its
  native GroupCommit regime, where the fsync batch timer paces writes so the
  sealer keeps up.
- **Derivation lineage (#3371)** is its own `lineage_active` row, not in
  `composed`: the only *public* write API attaching a `derived_from` list is
  `AletheiaDB::create_node_with_lineage`, which does not also accept a provenance
  bundle — the combined `create_node_with_options_and_lineage` is `pub(crate)`
  (MCP-internal). Rather than silently drop provenance to make lineage fit,
  lineage is reported standalone and gated on its own per-write-CPU cost.
- **Auth principal stamping (#3350)** is **scoped out** of this matrix: stamping
  a principal into version provenance requires a *served* authenticated session
  (`AletheiaMcpServer::with_auth` / the HTTP surface), not the embedded
  in-process `AletheiaDB` API these benches drive, so its per-write cost cannot
  be measured here. **Follow-up:** a served-surface auth-stamping write
  micro-benchmark is tracked as a future addition (it needs an in-process
  authenticated harness); its cost is expected to be dominated by the same
  provenance-serialization path `provenance_only` already bounds.

## The fixture (AC2 — reproducibility)

The workload is deterministic: a fixed-seed `rand::rngs::SmallRng`
(`WORKLOAD_SEED = 0x3383c0570f740500`) drives the write sequence on every run.
Each write creates a `"Bench"` node with a fixed property **shape**:

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

The `lineage_active` config derives every write from a fixed set of
`LINEAGE_REFS_PER_WRITE = 2` source facts (drawn from `LINEAGE_SOURCES = 4`
seeded `"Src"` nodes).

Every config writes the **identical property shape** — only the trust
attachment differs. Note that the seeded RNG *value* stream diverges across
configs (provenance-carrying writes draw extra bytes for the `note`), so the
configs are not byte-for-byte identical workloads; the property **shape** — the
thing that governs serialized write cost — is identical, and that is what the
same-run ratio isolates.

### How to run

```bash
# Full statistical run (Criterion arms + matrix table + JSON artifact)
cargo bench --bench provenance_write_throughput

# Fast reduced-scale smoke (fewer passes)
BENCH_SAMPLE_SIZE=10 BENCH_MEASUREMENT_TIME=1 BENCH_WARMUP_TIME=1 \
  PROV_BENCH_WRITES=10000 cargo bench --bench provenance_write_throughput

# Self-gating mode (assert the CPU-bound ratio bounds; non-zero exit on violation)
PROV_BENCH_GATE=1 cargo bench --bench provenance_write_throughput

# Prove the gate catches a REAL regression: inject a real 5µs busy-spin into
# the composed per-write path. The CPU-bound composed ratio drops from ~0.92 to
# ~0.52 and the gate trips, naming the composed row:
PROV_BENCH_GATE=1 PROV_BENCH_INJECT_SPIN_US=5 \
  cargo bench --bench provenance_write_throughput
```

Environment knobs:

| Variable | Meaning | Default |
|----------|---------|---------|
| `PROV_BENCH_WRITES` | timed writes per config on the CPU-bound arm (across `writes / 5000` interleaved passes) | 50000 |
| `PROV_BENCH_GATE` | `1` applies the CPU-bound ratio gate + the AC5 recovery `<5s` assertion (panics → non-zero exit) | off |
| `PROV_BENCH_INJECT_SPIN_US` | microseconds of **real** `black_box`'d busy-spin injected into the composed write path, to prove the gate detects a genuine per-write CPU regression | `0.0` |
| `PROV_BENCH_JSON_OUT` | path for the JSON results artifact | `$CARGO_TARGET_DIR/provenance_write_throughput.json` |
| `PROV_BENCH_RECOVERY_NODES` / `_EDGES` | recovery spot-check scale (10000 / 50000 = the manual/perf-rig reference scale) | 2000 / 4000 |
| `BENCH_SAMPLE_SIZE` / `BENCH_MEASUREMENT_TIME` / `BENCH_WARMUP_TIME` | Criterion tuning (shared harness) | 50 / 5 / 3 |

## Measured numbers (reference run)

Captured in this sandbox (`PROV_BENCH_WRITES=50000`, 10 interleaved passes).
Absolute throughput is hardware-dependent; the **CPU-bound ratios** are the
durable, portable signal. The `gc p50/p99` columns are the ungated GroupCommit
**latency observation** (fsync-batch-timer-dominated — **not** the overhead
gate; its p99 occasionally spikes to tens of ms on batch-timer alignment, which
is exactly why latency is not gated).

<!-- REFERENCE_NUMBERS_START -->
| Config              | CPU-bound tput (ops/s) | CPU-bound ratio | Bound | gc p50 (µs) | gc p99 (µs) |
|---------------------|-----------------------:|----------------:|:-----:|------------:|------------:|
| `baseline`          | 163281 | 1.000 | — | 10823 | 11389 |
| `provenance_only`   | 159816 | 0.979 | ≥0.85 | 10861 | 11177 |
| `constraint_active` | 163660 | 1.002 | ≥0.85 | 10921 | 11345 |
| `chain_active`      | 128983 | 0.790 | obs | 10932 | 22452 |
| `lineage_active`    | 149569 | 0.916 | ≥0.80 | 10784 | 11481 |
| `composed`          | 150780 | 0.923 | ≥0.80 | 10836 | 11409 |
<!-- REFERENCE_NUMBERS_END -->

Stability across three back-to-back unmodified gated runs (all **GATE PASS**):
`provenance_only` 0.938–0.979, `constraint_active` 0.959–1.002,
`lineage_active` 0.858–0.916, `composed` 0.895–0.934 — every gated row clears
its bound with margin every time. `chain_active` is the ungated observation and
swings more (0.63–0.84) with sealer contention, confirming why it is not gated
here.

> Reference hardware note: these were captured on a shared CI-class sandbox
> VM, not a dedicated performance rig. Treat absolute throughput as indicative
> only; the same-run CPU-bound ratios are what the gate and this page assert.

### The injection acceptance test (proving the gate catches a real regression)

The gate's whole value is that it *detects a real per-write CPU regression in a
trust path*. That is demonstrated directly, not asserted (both runs at
`PROV_BENCH_WRITES=50000`):

<!-- INJECTION_EVIDENCE_START -->
- **Control** (`PROV_BENCH_INJECT_SPIN_US=0`): composed **150780 ops/s, ratio
  0.923 ≥ 0.80 → gate PASSES** (all gated rows pass).
- **With a real 5 µs/write busy-spin** (`PROV_BENCH_INJECT_SPIN_US=5`): composed
  drops to **82380 ops/s, ratio 0.521 < 0.80 → gate TRIPS**, naming the
  `composed` row — while every *other* gated row still passes (provenance 0.953,
  constraint 0.970, lineage 0.894), because the injection targets only the
  composed path. The same 5 µs injection leaves the GroupCommit latency arm's
  p50 unchanged (~10.8 ms, batch-timer-bound), which is exactly why the
  fsync-dominated arm cannot be the gate.
<!-- INJECTION_EVIDENCE_END -->

A 5 µs/write regression is ~44 % of the ~11 µs composed write and is caught
unambiguously. The injection is a real `black_box`'d busy-spin (`busy_spin_us`),
**not** an arithmetic deflation of the reported throughput — so the "seeded
synthetic regression fails CI" success metric injects genuine work through the
actual write loop.

### Read / recovery spot-checks

- **AC4 — reads unaffected by provenance.** The suite computes a **real** p99
  (nearest-rank, from per-op samples — not a Criterion mean) for a current-state
  single-hop `get_node` on a provenance-carrying node (target **< 1µs p99**) and
  a temporal reconstruction of a provenance-carrying superseded version via
  `get_node_at_time` (target **< 10ms**). These are **ungated observations**
  (like the write-latency arm): in-sandbox they clear their targets with >20×
  margin, so hard-gating them would only invite scheduler-hiccup false-positives.
  <!-- READ_SPOTCHECK_START -->
  Reference run: current read p99 ≈ **0.19 µs** (p50 ≈ 0.08 µs); temporal
  reconstruct p99 ≈ **0.32 µs** (p50 ≈ 0.19 µs) — both far inside target.
  <!-- READ_SPOTCHECK_END -->
- **AC5 — recovery of a trusted dataset.** `provenance_recovery` populates a
  durable DB with provenance-carrying nodes and edges under an active uniqueness
  constraint, drops it, and times the reopen. Under `PROV_BENCH_GATE=1` the arm
  **asserts the reopen completes in < 5s** (a hard-fail alert on the scheduled
  lane — never a PR blocker). The **scheduled CI lane exercises that assertion at
  the reduced default (2000 nodes / 4000 edges)**: the dataset *build* is a
  single-writer GroupCommit populate (~10 ms/commit), so a 60K-write 10K/50K
  dataset would take ~10 min to *build* — too slow for the shared CI job. The
  **10K-node / 50K-edge reference scale (the medium-dataset < 5s budget) is
  reproduced manually / on the performance rig** via
  `PROV_BENCH_RECOVERY_NODES=10000 PROV_BENCH_RECOVERY_EDGES=50000` — it is the
  *reopen* (index-snapshot load + WAL tail replay), not the build, that the < 5s
  budget governs. At the reduced 50/50 smoke scale the reopen completes in
  <!-- RECOVERY_SPOTCHECK_START -->< 0.01 s<!-- RECOVERY_SPOTCHECK_END -->.

## Overhead bounds and the CI gate (AC3 / AC6 / AC7)

The self-gate (`PROV_BENCH_GATE=1`) computes the same-run **CPU-bound** ratios
and **fails with a non-zero exit, naming the offending row**, when any config
falls below its declared bound:

| Config              | Bound (CPU-bound ratio vs same-run baseline) |
|---------------------|:--------------------------------------------:|
| `provenance_only`   | ≥ 0.85 |
| `constraint_active` | ≥ 0.85 |
| `chain_active`      | **observation — ungated here** (≥ 0.90 gated by the #3351 `provenance_chain` bench) |
| `lineage_active`    | ≥ 0.80 |
| `composed`          | ≥ 0.80 (Issue #3383 hard success metric; provenance + constraint) |

These bounds are also published in
[`benchmarks/performance-targets.json`](../../benchmarks/performance-targets.json)
as `provenance_write_composed_ratio`, `provenance_only_write_ratio`,
`constraint_active_write_ratio`, `chain_active_write_ratio` (recorded as an
observation, not gated here), and `lineage_active_write_ratio`.

### CI policy (two lanes)

- **Scheduled bench lane** (weekly/manual cron): runs the full
  `provenance_write_throughput` target **with the gate enabled**
  (`PROV_BENCH_GATE=1`), including the recovery `< 5s` assertion at the reduced
  default scale (2000/4000 — the 10K/50K reference is manual/perf-rig, see AC5
  above). A gate failure (CPU-bound ratio below bound, or recovery ≥ 5s)
  hard-fails that job as an *alert* — it does not block any pull request.
- **Per-PR smoke**: runs a reduced-scale invocation (no gate) as
  **informational / non-required** (`continue-on-error: true`). It never fails
  the PR. Shared-runner timing noise must not become a spurious merge blocker.

Flipping the per-PR smoke from informational to blocking is a one-line change
(drop `continue-on-error`) once a week or two of scheduled-lane signal proves
the gate is stable on the runner — and that flip is an operator decision, not
part of this change.

## JSON results artifact (AC6)

Every matrix run writes a machine-readable JSON artifact to
`$PROV_BENCH_JSON_OUT` (default `$CARGO_TARGET_DIR/provenance_write_throughput.json`).
Schema `aletheiadb.provenance_write_throughput.v2` (bumped from v1: per-config
entries now carry `cpu_throughput` / `cpu_ratio_vs_baseline` from the gated
CPU-bound arm and `gc_p50_us` / `gc_p99_us` from the ungated latency arm — the
v1 single-arm `throughput` / `ratio_vs_baseline` fields are replaced; the doc
also records `gated_arm` / `latency_arm` metadata, `injected_spin_us`,
`composed_contains`, and `auth_stamping_scoped_out`):

```json
{
  "schema": "aletheiadb.provenance_write_throughput.v2",
  "issue": 3383,
  "gated_arm": {
    "regime": "cpu_bound",
    "durability_mode": "Async{flush_interval_ms:1}",
    "measurement": "interleaved multi-pass, solo DB per config",
    "rationale": "1ms flush drains the WAL ring fast enough that the single writer never blocks on flush backpressure, so per-write trust-feature CPU cost bottlenecks throughput; this is the gated arm. chain_active is reported but ungated (async sealer ceiling, not per-write CPU)."
  },
  "latency_arm": {
    "regime": "latency_observation",
    "durability_mode": "GroupCommit{max_delay_ms:10,max_batch_size:200}",
    "gated": false,
    "rationale": "single-writer fsync-batch-timer-dominated p50/p99; masks CPU cost, NOT gated"
  },
  "injected_spin_us": 0.0,
  "workload_seed": "0x3383c0570f740500",
  "writes_per_config": 50000,
  "gated": true,
  "composed_contains": ["provenance", "constraint"],
  "chain_gated_here": false,
  "auth_stamping_scoped_out": true,
  "fixture": { "label": "Bench", "unique_property": "uid", "name_bytes": 16, "payload_bytes": 64, "lineage_sources": 4, "lineage_refs_per_write": 2, "provenance": { "source": "prov-write-bench-3383", "confidence": 0.95, "note_bytes": 64 } },
  "configs": [
    {
      "config": "composed",
      "cpu_throughput": 150779.8,
      "cpu_ratio_vs_baseline": 0.923,
      "bound": 0.80,
      "gated": true,
      "pass": true,
      "gc_p50_us": 10836.0,
      "gc_p99_us": 11409.0
    }
  ]
}
```

Each entry in `configs` carries the CPU-bound `cpu_throughput` (ops/sec) and
`cpu_ratio_vs_baseline`, the declared `bound` (JSON `null` for the ungated
`baseline` and `chain_active` rows), a `gated` boolean, the boolean `pass`
(`cpu_ratio_vs_baseline >= bound`; always `true` for ungated rows), and the
ungated GroupCommit `gc_p50_us` / `gc_p99_us`. A CI job can publish or diff this
artifact without parsing benchmark stdout.
