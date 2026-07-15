# Comparative incumbents — framework only (NOT executed in this environment)

This directory ships the **runner configuration and query mappings** to run the
same logical queries against incumbent engines on the same generated data and
hardware. **None of these were executed in the sandbox that produced the
committed AletheiaDB results** (no Docker, no Neo4j/KuzuDB/XTDB binaries, no
network). Every incumbent cell in the capability table below is therefore
marked either **not measured here** (expressible, but not run in this
environment) or **not expressible** (the engine cannot represent the workload).
No incumbent numbers are fabricated.

## One command per engine (intended)

```bash
# Bring up the incumbents (requires Docker + docker compose):
docker compose -f incumbents/docker-compose.yml up -d

# AletheiaDB (this crate — actually runnable here):
cargo run -p aletheia-bench-ldbc --bin ldbc-bench -- --scale sf0.1 --out results/aletheiadb.json

# Neo4j: load the generated CSV, then run the mapped queries:
#   incumbents/neo4j/run.sh        (loads + times incumbents/neo4j/queries.cypher)
# KuzuDB:
#   incumbents/kuzu/run.sh         (loads + times incumbents/kuzu/queries.cypher)
# XTDB (temporal extension only, where expressible):
#   incumbents/xtdb/run.sh         (times incumbents/xtdb/queries.edn)
```

`run.sh` scripts are documented stubs (they print the exact steps and the query
files to execute); wiring them to live drivers is a follow-up once an
environment with the engines is available. They deliberately do **not**
synthesize numbers.

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
