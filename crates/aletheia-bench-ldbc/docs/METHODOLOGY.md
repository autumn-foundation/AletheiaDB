# Methodology — AletheiaDB LDBC-style Benchmark Suite (Issue #3373)

> **This is an "LDBC-style" suite, NOT an audited LDBC result.**
> The [LDBC](https://ldbcouncil.org/) Social Network Benchmark (SNB) is a
> formal, auditable specification. This suite borrows SNB's *workload shape*
> (short reads, complex reads, an insert/update stream) and adds two clearly
> labeled non-standard extensions. It has **not** been through an LDBC audit,
> does **not** use the official data generator by default, and does **not**
> claim conformance. Every deviation from the LDBC audit rules is listed below.

## What is measured

The runner (`ldbc-bench`) executes, in one command:

1. **Generate** a deterministic (seeded) SNB-shaped graph.
2. **Load** it into an in-process AletheiaDB instance (tempdir-backed WAL,
   group-commit durability via `AletheiaDB::new`).
3. **Benchmark** each operation: a warm-up phase (discarded) followed by N
   measured iterations, each timed with a monotonic `Instant`. Inputs vary
   deterministically across iterations to avoid single-key cache bias.
4. **Emit** a machine-readable JSON report with p50/p95/p99 (nearest-rank
   percentiles) per operation, plus a human summary.

### Operation set

| Key | SNB analogue | Category | Description |
|-----|--------------|----------|-------------|
| `is1_person_profile` | IS1 | short read | Single person profile lookup |
| `is2_person_recent_messages` | IS2 | short read | Messages authored by a person |
| `is3_person_friends` | IS3 | short read | Direct `KNOWS` friends |
| `is5_message_creator` | IS5 | short read | Creator of a post |
| `is6_forum_of_message` | IS6 | short read | Forum containing a post |
| `ic1_friends_within_2_hops` | IC1 | complex read | 2-hop friend reachability |
| `ic2_recent_messages_by_friends` | IC2 | complex read | Friends' messages |
| `ic9_messages_by_friends_of_friends` | IC9 | complex read | 2-hop friends' messages |
| `ins1_insert_person` | INS1 | insert/update | Insert a Person |
| `ins6_insert_post` | INS6 | insert/update | Insert a Post + creator edge |
| `upd2_update_person_city` | UPD | insert/update | Update a person property |
| `ext_temporal_reconstruction` | **EXT-TEMPORAL** | extension (non-standard) | Point-in-time reconstruction of a revised person |
| `ext_temporal_as_of_traversal` | **EXT-TEMPORAL** | extension (non-standard) | AS OF traversal of `KNOWS` at a historical valid time |
| `ext_vector_knn` | **EXT-VECTOR** | extension (non-standard) | k-NN (k=10) over post embeddings |
| `ext_vector_hybrid_graph_vector` | **EXT-VECTOR** | extension (non-standard) | Traverse forum→posts then rank by similarity |

The insert/update stream also builds the bi-temporal history the temporal
extension reconstructs (person revisions applied at increasing valid times).

## Scale points

| Label | Persons | ~KNOWS degree | Forums | Posts | Comments | Embeddings |
|-------|---------|---------------|--------|-------|----------|-----------|
| `smoke` | 60 | 6 | 6 | 48 | 96 | 48 |
| `sf0.1` ("SF0.1-equivalent") | 1,500 | 14 | 60 | 2,400 | 7,200 | 2,400 |

**Honesty on scale:** these are **not** the official LDBC SF0.1 cardinalities.
The official SF0.1 has ~tens of thousands of persons and a power-law degree
distribution produced by LDBC Datagen (Spark). We label our points
"SF0.1-equivalent" and "smoke" to be explicit that they are *shaped like* SNB
at a size that runs with zero external dependencies in a CI-nightly budget, not
the audited scale factors.

## Using the official LDBC generator instead

The built-in generator exists so the suite runs from a fresh clone with no
external tooling. To run against real LDBC data:

1. Produce SNB CSVs with [LDBC Datagen](https://github.com/ldbc/ldbc_snb_datagen_spark).
2. Map the CSV entities to the labels/edge-types in
   [`src/loader.rs`](../src/loader.rs) (`Person`, `KNOWS`, `Forum`,
   `CONTAINER_OF`, `Post`, `HAS_CREATOR`, `HAS_TAG`, `Comment`, `REPLY_OF`).
3. A CSV-ingest front-end for `loader` is a documented follow-up (the loader is
   already factored so a CSV reader can produce the same `GeneratedGraph`).

## Environment captured per run

The report's `hardware` block records `arch`, `os`, `logical_cpus`, best-effort
`cpu_model` (from `/proc/cpuinfo`), and `total_mem_mib` (from `/proc/meminfo`).
The sandbox this was first run on: **Intel Xeon @ 2.80GHz, 4 logical CPUs,
~15 GiB RAM, Linux x86_64, rustc 1.94.1** (see the committed baseline JSON for
the exact captured values).

## Tuning, warm-up, run counts

* **Tuning:** engine defaults. AletheiaDB uses `AletheiaDB::new()` (tempdir WAL,
  group-commit). No custom tuning is applied; any change here must be recorded.
* **Warm-up:** the first `--warmup` iterations of each operation are executed
  and discarded before measurement (default 20; the smoke/nightly recipe may
  use fewer).
* **Run counts:** `--iterations` measured iterations per operation (default
  200). Percentiles are nearest-rank over the measured samples.
* **Percentiles:** p50/p95/p99 via nearest-rank (`rank = ceil(p/100 * n)`), no
  interpolation — every reported number is an observed sample.

## Deviations from LDBC audit rules (explicit)

1. **Not audited.** No LDBC auditor has reviewed or certified these numbers.
2. **Not the official generator/scale.** Built-in synthetic generator; scale
   points are "-equivalent", not audited SF0.1/SF1.
3. **No official SNB parameter curation.** SNB ships curated substitution
   parameters chosen for stable selectivity; we cycle through sampled ids.
4. **Query subset.** A representative subset of IS/IC operations, not the full
   SNB interactive workload; the update stream is simplified.
5. **Single process, in-memory tempdir WAL.** No client/server round-trip, no
   separate driver process, no network. (MCP-layer latency is owned by #3361.)
6. **Extensions are non-standard.** The temporal and vector workloads are not
   part of LDBC SNB and are labeled `EXT-*` everywhere.
7. **Incumbents not executed here.** See `incumbents/README.md`; the capability
   table marks every incumbent cell *not measured here* or *not expressible* —
   never a fabricated number.
8. **Vector 1M scale is aspirational.** The suite runs the largest feasible
   scale in the environment and reports `vector_count` honestly; it never
   claims 1M vectors were run when they were not.

## Targets checked

* SNB short reads: consistent with AletheiaDB's current-state targets
  (single-hop reads are microsecond-scale in-process).
* Temporal reconstruction: **< 10 ms** (`ext_temporal_reconstruction` p99).
* Vector k-NN (k=10): **< 10 ms** at the scale actually run
  (`ext_vector_knn` p99); the 1M-vector target is aspirational.

## Regression gate

`ldbc-bench --check-gate --baseline <path>` compares a fresh run against a
stored baseline and exits non-zero (code 2) if any operation's p99 regresses, or
if a baseline operation is missing. The gate is **AletheiaDB-only** and never
compares against incumbents.

**Regression rule (two-part).** An operation is flagged only when its p99 grows
by **both** more than `--threshold` percent (default 10%) **and** more than
`--min-abs-delta-us` microseconds (default 5µs). The absolute noise floor is
required because the fastest operations here run at **sub-microsecond** p99
(single-node in-process reads, ~0.5–1µs): at that scale, ~0.2µs of run-to-run
scheduler jitter is a +30–40% *relative* swing, so a pure 10% relative gate
would flap red every single run and be useless. The floor keeps the ">10% p99"
contract meaningful for latencies that matter (complex reads, writes, the
extensions) while suppressing sub-noise-floor jitter. This is a **deliberate
deviation** from a naïve pure-relative gate, documented here for honesty; set
`--min-abs-delta-us 0` to restore the strict pure-relative behavior.

Sensitivity is proven by a unit test feeding a synthetic 2x-slower run
(`tests/gate_test.rs::synthetic_2x_slowdown_fails_the_gate`) — a 2x regression
clears both the percent and absolute thresholds comfortably — and the floor's
own tests prove it suppresses sub-µs jitter without masking a real +30µs/+30%
regression.
