# Comparative incumbents — framework only (NOT executed in this environment)

This directory ships **runnable drivers plus the query mappings** to run the
same logical queries against incumbent engines on the same data and hardware.
**None of these were executed in the sandbox that produced the committed
AletheiaDB results** (no Docker, no Neo4j/KuzuDB/XTDB binaries, no network) —
they are meant to run **on the self-hosted box** (see
[../../../docs/guides/benchmark-infra.md](../../../docs/guides/benchmark-infra.md)),
where Docker and the engine images are available. Every incumbent cell in the
capability table below is therefore marked either **not measured here**
(expressible, but not run in this environment) or **not expressible** (the
engine cannot represent the workload). No incumbent numbers are fabricated: the
drivers only ever record latencies from real timed query invocations, and fail
loudly rather than emit a value for a stopped engine or an empty database.

## Running on the box

Prerequisites on the self-hosted box: `docker` + `docker compose` v2, `bash` 5+
(for `$EPOCHREALTIME` microsecond timing), `python3` (percentile math / JSON
emit), and `curl` (the XTDB HTTP client). The engine CLIs (`cypher-shell`,
`kuzu`) run **inside** the containers, so no host install of those is needed.

```bash
cd crates/aletheia-bench-ldbc/incumbents

# 1. Bring up the incumbents:
docker compose up -d                    # or: up -d neo4j kuzu xtdb (pick engines)

# 2. Load the SHARED graph into each engine (see "Data interchange" below).
#    The drivers do NOT bulk-load for you — ETL is engine/dataset specific —
#    they verify the graph is non-empty and fail loudly over an empty DB.

# 3. Drive all engines (warmup + N timed iters, writes results/<engine>_results.json):
./run_all.sh --iterations 200 --warmup 20 [--datagen-dir /abs/path/to/ldbc/sfN]

# Or one engine at a time:
ITERATIONS=200 WARMUP=20 ./neo4j/run.sh
ITERATIONS=200 WARMUP=20 ./kuzu/run.sh
ITERATIONS=200 WARMUP=20 ./xtdb/run.sh

# AletheiaDB side (this crate) for the same-shape comparison:
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- \
    --scale sf0.1 --out results/aletheiadb.json
```

Each `run.sh` **actually executes** the mapped queries against the running
container — `neo4j/run.sh` via `cypher-shell`, `kuzu/run.sh` by piping Cypher
into the `kuzu` CLI, `xtdb/run.sh` by POSTing EDN Datalog to the XTDB HTTP API —
timing every iteration with the shell's microsecond clock (`lib.sh:time_once_us`)
and computing p50/p95/p99 with the **nearest-rank** method (`emit_results.py`,
matching `../src/stats.rs`) so the numbers line up with the AletheiaDB report.
Queries an engine cannot express natively are written as `{not_expressible:true}`
capability facts, never as a fabricated latency. `run_all.sh` returns non-zero
if any engine driver fails, so a stopped container or empty DB surfaces loudly.

> The `run_all.sh` / `lib.sh` / `emit_results.py` helpers are re-included past
> the repo-wide `*.sh` / `*.py` ignore rules (see the root `.gitignore`).

## Data interchange

Feed all engines the **same** generated graph. Export the built-in generator to
CSV (a documented follow-up front-end on `loader`), or use official LDBC Datagen
CSVs (see `../docs/METHODOLOGY.md`). The logical schema is:

```
(:Person {id, name, city, birth_year})
(:Person)-[:KNOWS]->(:Person)
(:Forum  {id, title})-[:CONTAINER_OF]->(:Post)
(:Post   {id, length, embedding})-[:HAS_CREATOR]->(:Person)
(:Post)-[:HAS_TAG]->(:Tag {id, name})
(:Comment)-[:REPLY_OF]->(:Post)
(:Comment)-[:HAS_CREATOR]->(:Person)
```

## Capability table (HONEST — no fabricated numbers)

Legend:
- **runnable-here** — executed by this crate in the sandbox (AletheiaDB only).
- **not measured here** — expressible on the engine, but not run in this
  environment (needs the engine binary + a driver).
- **not expressible** — the engine cannot represent this workload with its
  built-in model (would require full application-side modeling).

| Workload | AletheiaDB | Neo4j | KuzuDB | XTDB |
|----------|-----------|-------|--------|------|
| SNB short reads (IS1–IS6) | runnable-here | not measured here | not measured here | not measured here |
| SNB complex reads (IC1/IC2/IC9) | runnable-here | not measured here | not measured here | not measured here |
| Insert/update stream | runnable-here | not measured here | not measured here | not measured here |
| **EXT temporal: point-in-time reconstruction** | runnable-here | **not expressible**¹ | **not expressible**¹ | not measured here² |
| **EXT temporal: AS OF traversal** | runnable-here | **not expressible**¹ | **not expressible**¹ | not measured here² |
| **EXT vector: k-NN (k=10)** | runnable-here | not measured here³ | not measured here³ | **not expressible**⁴ |
| **EXT vector: hybrid graph+vector** | runnable-here | **not expressible**⁵ | **not expressible**⁵ | **not expressible**⁵ |

Footnotes:
1. **Neo4j / KuzuDB** have no native bi-temporal (valid-time + transaction-time)
   reconstruction. Point-in-time "as of" queries require modeling validity
   intervals as ordinary properties and filtering in the query — i.e. full
   application-side history modeling — so the *native* capability is **not
   expressible**. (One can emulate it; that is a different, hand-built workload.)
2. **XTDB** is bi-temporal and *can* express valid-time/tx-time "as of" queries,
   so these are **not expressible-blocked** — they are simply **not measured
   here** (no XTDB running in the sandbox). XTDB is not a property graph, so the
   traversal is expressed as recursive Datalog rather than graph pattern steps.
3. **Neo4j / KuzuDB** vector k-NN: Neo4j has a vector index (5.x+); KuzuDB has
   vector extensions. Expressible but engine-version-dependent — **not measured
   here**.
4. **XTDB** has no native ANN / vector index — **not expressible** natively.
5. **Hybrid graph+vector in one query** (traverse then rank by embedding
   similarity in a single planned operation) is AletheiaDB-specific; the others
   would need two separate systems/queries stitched application-side, so the
   single-query hybrid is **not expressible** as one workload.

This structural asymmetry — no other single engine has all three axes (graph
interactive + bi-temporal + hybrid vector) — is exactly the point Issue #3373
asks the suite to make legible, and it is stated here as a capability fact, not
a performance claim.
