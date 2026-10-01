# aletheia-bench-ldbc

An **LDBC-style** (not audited) benchmark suite for AletheiaDB — Issue #3373.

It implements a documented subset of the LDBC Social Network Benchmark (SNB)
interactive workload (short reads, complex reads, insert/update stream) mapped
onto AletheiaDB's graph model, plus **two clearly-labeled non-standard
extension workloads** exercising AletheiaDB's differentiators on the same
dataset:

- **Temporal extension** — point-in-time reconstruction + AS OF traversal over
  the update stream's accumulated bi-temporal history (target: < 10 ms).
- **Vector extension** — k-NN (k=10) + hybrid graph+vector over synthetic,
  seeded, deterministic embeddings attached to SNB entities.

> ## Honesty first
> This is **"LDBC-style, not an audited LDBC result."** It ships a built-in
> synthetic generator (not the official LDBC Datagen), does not execute
> incumbent engines in CI/sandbox, and treats the 1M-vector scale as
> **aspirational**. See [`docs/METHODOLOGY.md`](docs/METHODOLOGY.md) for every
> deviation and [`incumbents/README.md`](incumbents/README.md) for the honest
> capability table.

## One-command run

From a fresh clone (zero external dependencies):

```bash
# Tiny smoke size:
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- --scale smoke

# "SF0.1-equivalent":
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- --scale sf0.1 --out results.json
```

Or via `just`:

```bash
just bench-ldbc                 # smoke -> ldbc_results.json
just bench-ldbc sf0.1 300 30    # SF0.1-equivalent, 300 iters, 30 warmup
just bench-ldbc-gate            # run + check p99 regression gate vs committed baseline
```

The runner generates → loads → benchmarks → emits a machine-readable JSON
report (p50/p95/p99 per operation) and a human summary.

## Regression gate (AletheiaDB-only)

```bash
# Write a baseline:
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- \
  --scale smoke --write-baseline crates/aletheia-bench-ldbc/baselines/ldbc_smoke_baseline.json

# Compare a fresh run; exit code 2 on >10% p99 regression:
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- \
  --scale smoke --check-gate \
  --baseline crates/aletheia-bench-ldbc/baselines/ldbc_smoke_baseline.json
```

An informational, **non-gating** nightly workflow
(`.github/workflows/ldbc-bench-nightly.yml`) runs the smoke suite and checks the
gate. Making it a required check is a documented follow-up.

## Layout

| Path | Purpose |
|------|---------|
| `src/generator.rs` | Deterministic (seeded) SNB-shaped graph generator |
| `src/loader.rs` | Maps the generated graph onto AletheiaDB (labels/edges/history) |
| `src/runner.rs` | Timing harness + workload definitions (SNB subset + extensions) |
| `src/stats.rs` | p50/p95/p99 nearest-rank percentile math |
| `src/gate.rs` | Regression-gate decision logic |
| `src/report.rs` | JSON report schema + hardware capture |
| `src/bin/ldbc_bench.rs` | The `ldbc-bench` runner binary |
| `baselines/` | Committed baseline JSON reports |
| `incumbents/` | Neo4j / KuzuDB / XTDB runner configs + query mappings (framework only) |
| `docs/METHODOLOGY.md` | Methodology, scale points, deviations, targets |

## Coverage note

This crate is an **isolated workspace member** built only via `-p
aletheia-bench-ldbc`; bare `cargo build/test` stays root-scoped via
`default-members`. The repo's coverage job measures crates with an explicit
`-p` per crate, so this benchmark crate is **not** auto-measured and does not
affect combined coverage thresholds.
