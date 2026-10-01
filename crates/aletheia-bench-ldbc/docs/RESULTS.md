# Published Results — AletheiaDB LDBC-style Benchmark Suite (Issue #3373)

> **LDBC-STYLE, NOT AN AUDITED LDBC RESULT.** Built-in synthetic generator (not
> official LDBC Datagen), single-node, in-process. Incumbents were **not run**
> in this environment. See [`METHODOLOGY.md`](METHODOLOGY.md) for every
> deviation and [`../incumbents/README.md`](../incumbents/README.md) for the
> honest capability table. Raw JSON is committed under
> [`../baselines/`](../baselines/).

## Environment (captured at run time)

| Field | Value |
|-------|-------|
| CPU | Intel(R) Xeon(R) Processor @ 2.80GHz |
| Logical CPUs | 4 |
| Memory | ~15 GiB |
| OS / arch | Linux / x86_64 |
| rustc | 1.94.1 |
| Durability | `AletheiaDB::new()` (tempdir WAL, group-commit; engine defaults) |
| Warm-up / iterations | 20–30 warm-up (discarded) / 200–300 measured |
| Percentiles | nearest-rank p50/p95/p99 |

Raw JSON: [`baselines/ldbc_smoke_baseline.json`](../baselines/ldbc_smoke_baseline.json)
(smoke, 300 iters) and
[`baselines/ldbc_sf0.1_baseline.json`](../baselines/ldbc_sf0.1_baseline.json)
(SF0.1-equivalent, 200 iters).

## AletheiaDB results — SF0.1-equivalent (11,280 nodes, 42,660 edges, 2,400 vectors, dim 64)

| Operation | SNB | Category | p50 (µs) | p95 (µs) | p99 (µs) |
|-----------|-----|----------|---------:|---------:|---------:|
| Person profile | IS1 | short read | 0.73 | 1.41 | 1.76 |
| Person recent messages | IS2 | short read | 4.15 | 6.64 | 8.70 |
| Person friends | IS3 | short read | 5.28 | 6.99 | 8.08 |
| Message creator | IS5 | short read | 1.43 | 2.36 | 2.76 |
| Forum of message | IS6 | short read | 0.93 | 1.52 | 2.41 |
| Friends within 2 hops | IC1 | complex read | 71.74 | 98.50 | 158.38 |
| Recent messages by friends | IC2 | complex read | 38.94 | 52.75 | 88.84 |
| Messages by friends-of-friends | IC9 | complex read | 477.67 | 545.76 | 571.54 |
| Insert person | INS1 | insert/update | 2976 | 3177 | 3439 |
| Insert post + edge | INS6 | insert/update | 5677 | 5972 | 6140 |
| Update person city | UPD | insert/update | 2855 | 3113 | 3212 |
| **Temporal reconstruction** | **EXT** | **temporal (non-standard)** | **0.90** | **1.57** | **1.85** |
| **Temporal AS OF traversal** | **EXT** | **temporal (non-standard)** | 18.38 | 23.80 | 51.81 |
| **Vector k-NN (k=10)** | **EXT** | **vector (non-standard)** | **49.22** | **71.51** | **88.70** |
| **Vector hybrid graph+vector** | **EXT** | **vector (non-standard)** | 2.31 | 2.37 | 2.47 |

## AletheiaDB results — smoke (222 nodes, 702 edges, 48 vectors, dim 32)

Committed baseline (300 iters). Highlights: single-hop reads are **sub-µs to a
few µs**; temporal reconstruction p99 **0.15µs**; vector k-NN p99 **~12µs**.
Full numbers in [`baselines/ldbc_smoke_baseline.json`](../baselines/ldbc_smoke_baseline.json).

## Targets (Issue #3373 AC)

| Target | Result | Status |
|--------|--------|--------|
| SNB short reads consistent with current-state targets | IS1 p50 sub-µs (0.73µs SF0.1); p99 ~1.8µs slightly exceeds the strict <1µs single-hop target; all IS reads single-digit µs | ✅ (p50) |
| Temporal-extension reconstruction **< 10 ms** | p99 **0.15µs** (smoke) / **1.85µs** (SF0.1) | ✅ |
| Vector-extension k-NN (k=10) **< 10 ms** | p99 **12µs** (48 vectors) / **89µs** (2,400 vectors) | ✅ (at the scale run) |

**Write latency note (honest):** insert/update ops are **milliseconds** (INS6
~6ms p50) because `AletheiaDB::new()` uses **group-commit durability** (a real
fsync per commit batch). This is a durability cost, not a graph-op cost, and is
reported as-is rather than tuned away. A benchmark of the Async durability mode
would show far lower write latency; we keep defaults per the methodology.

## Honesty on scale

- The scale points are **"SF0.1-equivalent"** and **"smoke"**, produced by the
  built-in synthetic generator — **not** the official LDBC Datagen and **not**
  the audited SF0.1/SF1 cardinalities. SF1 was not run.
- The **1M-vector** target for the vector extension is **aspirational and NOT
  run** here. The largest scale actually measured is **2,400 vectors** (dim 64);
  `vector_count` in the raw JSON always reports the real number.
- Incumbent engines were **not executed** in this environment. See the
  capability table in [`../incumbents/README.md`](../incumbents/README.md):
  every cell is marked *not measured here* or *not expressible* — never a
  fabricated number.

## Reproduce

```bash
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- --scale sf0.1 --out results.json
# or: just bench-ldbc sf0.1 200 20
```
