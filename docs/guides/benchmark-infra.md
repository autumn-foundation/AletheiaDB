# Benchmark Infrastructure Guide (Issue #3628)

This guide covers AletheiaDB's two benchmark paths and how the **heavy** runs are
moved off the shared GitHub-hosted CI runners onto a dedicated self-hosted box.

- **[1. Overview](#1-overview)**
- **[2. Registering the Hetzner box as a self-hosted runner](#2-registering-the-hetzner-box-as-a-self-hosted-runner)**
- **[3. Running via the workflow](#3-running-via-the-workflow)**
- **[4. Running manually via SSH](#4-running-manually-via-ssh)**
- **[5. Official LDBC Datagen SF1](#5-official-ldbc-datagen-sf1)**
- **[6. Incumbents (Neo4j / KuzuDB / XTDB)](#6-incumbents-neo4j--kuzudb--xtdb)**
- **[7. Interpreting results](#7-interpreting-results)**

---

## 1. Overview

AletheiaDB has two complementary benchmark paths:

| Path | What it measures | Where it lives | Runner |
|------|------------------|----------------|--------|
| **LDBC-style harness** (`aletheia-bench-ldbc`) | End-to-end SNB-shaped workloads: short/complex graph reads, an insert/update stream, plus temporal and vector **extensions**, reported with p50/p95/p99 per operation | `crates/aletheia-bench-ldbc/` | `ldbc-bench` binary |
| **Criterion micro-benches** | Fine-grained hot-path latencies (single-hop traversal, temporal reconstruction, vector k-NN, recovery/checkpoints, …) | `benches/*.rs` | `cargo bench` |

Both are **informational and non-gating** on the shared CI (they never block a
merge). The lightweight subset already runs on GitHub-hosted runners
(`.github/workflows/ldbc-bench-nightly.yml`, `smoke`/`sf0.1`). **Issue #3628**
moves the resource-hungry runs — **SF1**, the **1M-vector** k-NN run, and the
**Docker-based incumbent** comparisons — onto a self-hosted
[Hetzner](https://www.hetzner.com/) box with the cores, RAM, and disk headroom
those runs need, via a separate workflow
(`.github/workflows/benchmark-selfhosted.yml`). That workflow has **no**
`push`/`pull_request` triggers, so it can never gate a PR.

> **Honesty note.** This is an *LDBC-style* suite, **not** an audited LDBC
> result. See [`../../crates/aletheia-bench-ldbc/docs/METHODOLOGY.md`](../../crates/aletheia-bench-ldbc/docs/METHODOLOGY.md)
> for every deviation from the LDBC audit rules.

---

## 2. Registering the Hetzner box as a self-hosted runner

The self-hosted runner is **your** infrastructure — Claude/CI cannot reach the
box. You register it once, and thereafter the workflow targets it purely by
runner **label**. No hostname, IP, or secret appears anywhere in the repo.

### Hardware assumptions

The heavy runs are sized for a machine roughly like a Hetzner dedicated box:

- **CPU:** 8+ physical cores (SF1 load and the 1M-vector HNSW build are
  CPU-heavy; the report captures the actual CPU model and core count).
- **RAM:** 32 GB+ recommended. SF1 (~10× SF0.1 proportions) plus a 1M×384-dim
  `f32` embedding corpus (~1.5 GB raw, more with the HNSW index) must fit in RAM
  — the hot tier is in-memory.
- **Disk:** a data volume with **tens of GB free**. An official LDBC SNB Datagen
  SF1 CSV export is **tens of GB**; stage it on a data volume (e.g.
  `/data/ldbc/sf1`), **not** the OS disk, and point `--datagen-dir` at it.

### Registration steps (performed by you, in the GitHub UI + on the box)

1. In GitHub: **Settings → Actions → Runners → New self-hosted runner**.
   Choose **Linux** / **x64**.
2. On the box, run the **download** commands GitHub shows (they include a
   one-time registration token):
   ```bash
   mkdir -p ~/actions-runner && cd ~/actions-runner
   curl -o actions-runner-linux-x64.tar.gz -L <url-github-shows>
   tar xzf actions-runner-linux-x64.tar.gz
   ```
3. **Configure** it, adding the `benchmarks` label the workflow selects on:
   ```bash
   ./config.sh --url https://github.com/autumn-foundation/AletheiaDB \
       --token <registration-token-github-shows> \
       --labels benchmarks
   ```
   (The `self-hosted` label is added automatically; the workflow requires
   **both** `self-hosted` and `benchmarks`.)
4. **Run it as a service** so it survives reboots and picks up jobs
   unattended:
   ```bash
   sudo ./svc.sh install
   sudo ./svc.sh start
   ```
   Verify it shows **Idle** under Settings → Actions → Runners.
5. Install the toolchain the jobs assume on the box: a Rust toolchain
   (the workflow also runs `dtolnay/rust-toolchain@stable`, but a warm
   `~/.cargo` and `~/.rustup` speed cold builds), plus **Docker + docker
   compose v2** if you want the incumbent comparison, and `python3` (for the
   summary/percentile scripts). See §6 for the incumbent prerequisites.

> Runner security: a self-hosted runner executes whatever the workflow contains.
> Keep the box dedicated to this repo, and prefer running the service under an
> unprivileged user.

---

## 3. Running via the workflow

From the GitHub **Actions** tab, pick **“Benchmarks (self-hosted, heavy)”** and
click **Run workflow** (`workflow_dispatch`). Inputs:

| Input | Meaning | Default |
|-------|---------|---------|
| `scale` | LDBC scale point: `smoke`, `sf0.1`, or `sf1` | `sf0.1` |
| `vector_count` | Vector-extension corpus size (e.g. `1000000`). Empty ⇒ preset | *(empty)* |
| `vector_dim` | Embedding dimensionality (e.g. `384`). Empty ⇒ preset | *(empty)* |
| `iterations` | Measured iterations per operation | `200` |
| `warmup` | Warmup iterations (discarded) | `20` |
| `datagen_dir` | Absolute path **on the box** to an official LDBC SNB Datagen CSV dir. Empty ⇒ synthetic generator | *(empty)* |
| `run_incumbents` | Also run the Docker-based incumbent comparison (needs Docker on the box) | `false` |

Empty `vector_count` / `vector_dim` / `datagen_dir` are **omitted** from the CLI
invocation (the harness then uses its presets), via shell conditionals in the
workflow.

Two jobs run on `runs-on: [self-hosted, benchmarks]`:

- **`ldbc`** — builds and runs `ldbc-bench` at the requested scale/vector size,
  writes `ldbc_results_<scale>.json`, uploads it as the
  `ldbc-results-<scale>` artifact, and appends a Markdown table (key,
  description, p50/p95/p99, vector count) plus the hardware and disclaimer to the
  **job summary**. If `run_incumbents` is `true`, it additionally runs the
  incumbent drivers as a `continue-on-error` step and uploads
  `incumbent-results`.
- **`criterion`** — runs a **curated** subset of `cargo bench` (the five benches
  that cover the project's stated performance axes: `performance_targets`,
  `current_state`, `temporal_query`, `vector_similarity`, `checkpoints`) and
  uploads `target/criterion/**` as the `criterion-results` artifact. A full
  `cargo bench` is minutes-to-tens-of-minutes; run it manually (§4) when you want
  every bench target.

Both jobs are also wired to an **optional nightly** `schedule` (04:30 UTC),
guarded to `github.repository == 'autumn-foundation/AletheiaDB'` so forks that
register their own runner don't fire it. Artifacts and the rendered summary
appear on the workflow run's page under **Artifacts** and **Summary**.

---

## 4. Running manually via SSH

SSH to the box, `cd` into a checkout, and drive the harness directly. The
`ldbc-bench` CLI flags (from
[`src/bin/ldbc_bench.rs`](../../crates/aletheia-bench-ldbc/src/bin/ldbc_bench.rs)):

```
--scale <smoke|sf0.1|sf1>   Scale point (default: smoke)
--seed <N>                  PRNG seed (default: 42)
--iterations <N>            Measured iterations per op (default: 200)
--warmup <N>                Warmup iterations discarded (default: 20)
--vector-count <N>          Override the vector-extension corpus size (e.g. 1000000)
--vector-dim <D>            Override embedding dim (must be > 0, e.g. 384)
--datagen-dir <PATH>        Ingest an official LDBC SNB Datagen CSV dir instead
                            of the synthetic generator
--out <PATH>                JSON report output (default: ldbc_results.json)
--write-baseline <PATH>     Also write this run as a baseline JSON
--check-gate                Compare against --baseline, exit 2 on regression
--baseline <PATH>           Baseline JSON for --check-gate
--threshold <PCT>           p99 regression threshold percent (default: 10)
--min-abs-delta-us <US>     Absolute p99 noise floor in µs (default: 5)
```

Common invocations:

```bash
# Smoke (tiny; fast sanity check)
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- \
    --scale smoke --out ldbc_results_smoke.json

# SF0.1-equivalent
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- \
    --scale sf0.1 --iterations 300 --warmup 30 --out ldbc_results_sf0.1.json

# SF1-equivalent (heavy — self-hosted box territory)
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- \
    --scale sf1 --iterations 200 --warmup 20 --out ldbc_results_sf1.json

# 1M-vector k-NN run over an SF0.1-sized graph
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- \
    --scale sf0.1 --vector-count 1000000 --vector-dim 384 \
    --out ldbc_results_1m_vectors.json

# Official LDBC SNB Datagen ingest (see §5), with a 1M-vector overlay
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- \
    --datagen-dir /data/ldbc/sf1 --vector-count 1000000 --vector-dim 384 \
    --out ldbc_results_datagen_sf1.json
```

`just` shortcuts (see the `justfile`):

```bash
just bench-ldbc                 # smoke, writes ldbc_results.json
just bench-ldbc sf0.1 300 30    # SF0.1-equivalent, 300 iters, 30 warmup
```

### Regression gate & baselines

The harness has an **AletheiaDB-only** regression gate: a run's p99 must not
exceed the committed baseline for that scale by more than the threshold
(default 10%) **and** the absolute noise floor (default 5 µs). Exit code `2`
means a regression; `0` means pass; `1` is a runtime error.

```bash
# Refresh a committed baseline (run on a stable reference machine, then commit):
just bench-ldbc-baseline sf0.1 300 30
# ...equivalently:
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- \
    --scale sf0.1 --iterations 300 --warmup 30 \
    --out crates/aletheia-bench-ldbc/baselines/ldbc_sf0.1_baseline.json \
    --write-baseline crates/aletheia-bench-ldbc/baselines/ldbc_sf0.1_baseline.json

# Check a fresh run against the committed baseline (exits 2 on regression):
just bench-ldbc-gate sf0.1 200 20
# ...equivalently:
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- \
    --scale sf0.1 --iterations 200 --warmup 20 \
    --out ldbc_results.json --check-gate \
    --baseline crates/aletheia-bench-ldbc/baselines/ldbc_sf0.1_baseline.json
```

### Criterion micro-benches

```bash
cargo bench                       # every bench target (long!)
just bench                        # same (cargo bench)
# The curated subset the self-hosted workflow runs:
cargo bench --features "config-toml,observability,semantic-search,embeddings" \
    --bench performance_targets --bench current_state --bench temporal_query \
    --bench vector_similarity --bench checkpoints
```

Criterion writes HTML + raw estimates under `target/criterion/`.

---

## 5. Official LDBC Datagen SF1

By default the harness uses its built-in deterministic **synthetic** generator
(shaped like SNB, but not the official data). For a run over the real dataset,
generate an official **[LDBC SNB Datagen](https://github.com/ldbc/ldbc_snb_datagen_spark)**
(Spark) export at scale factor 1 and point `--datagen-dir` at it.

- **Format ingested:** the `composite-merged-fk` dynamic-entity CSV output —
  `|`-delimited, one header row per file. Columns are located by **header name**
  (case-insensitive), never by position, because Datagen column order varies by
  version. CRLF line endings and a single trailing empty field are tolerated.
- **Disk:** the SF1 CSV export is **tens of GB** — stage it on a data volume.

**Ingested vs omitted SNB subset** (mirrors the doc table in
[`src/datagen.rs`](../../crates/aletheia-bench-ldbc/src/datagen.rs)). The 15
suite operations only touch a subset of the SNB schema, so the ingest reads
exactly the files needed:

| File | Populates |
|------|-----------|
| `Person.csv` | `Person` nodes (`id`, `firstName`) |
| `Person_knows_Person.csv` | `KNOWS` edges |
| `Forum.csv` | `Forum` nodes (`id`, `title`, moderator FK) |
| `Forum_containerOf_Post.csv` | `CONTAINER_OF` (post → forum) |
| `Post.csv` | `Post` nodes (`length`, `CreatorPersonId`) |
| `Comment.csv` | `Comment` nodes (`CreatorPersonId`) |
| `Comment_replyOf_Post.csv` | `REPLY_OF` (comment → post) + `HAS_CREATOR` |
| `Tag.csv` | `Tag` nodes (target of `HAS_TAG`) |

Deliberate limitations (documented, not fudged):

- **Embeddings are synthetic.** Official Datagen ships **no** vectors; the loader
  mandates a `Post.embedding` vector index, so each ingested post gets a
  deterministic *synthetic* embedding. `--vector-count`/`--vector-dim` layer an
  additional standalone synthetic corpus on top. The **graph is real; the
  vectors are not.**
- **No update/delete stream.** A static dump has no revisions, so the
  temporal-reconstruction op has nothing to reconstruct (it no-ops, reported
  honestly).
- **Not mapped:** `Comment_replyOf_Comment.csv`, `Person_hasInterest_Tag.csv`,
  `Post_hasTag_Tag.csv`, and every other SNB entity/edge (Place, Organisation,
  TagClass, memberships, likes, workAt/studyAt, …) — no suite operation
  traverses them.

Ingest command:

```bash
cargo run -p aletheia-bench-ldbc --release --bin ldbc-bench -- \
    --datagen-dir /data/ldbc/sf1 --out ldbc_results_datagen_sf1.json
```

When `--datagen-dir` is set, the synthetic `--scale` sizing is ignored (the
report records `source: "datagen"`, `scale: "datagen"`); `--vector-count` /
`--vector-dim` still size the synthetic vector corpus layered on the real graph.

---

## 6. Incumbents (Neo4j / KuzuDB / XTDB)

The `crates/aletheia-bench-ldbc/incumbents/` directory holds **runnable drivers**
(plus query mappings) to run the same logical queries against incumbent engines
on the same box, for a like-for-like comparison. They are **never run in CI's
sandbox** — only on the self-hosted box, which has Docker.

**Prerequisites on the box:** `docker` + `docker compose` v2, `bash` 5+
(microsecond timing via `$EPOCHREALTIME`), `python3` (percentile math / JSON
emit), and `curl` (the XTDB HTTP client). The engine CLIs (`cypher-shell`,
`kuzu`) run **inside** the containers.

```bash
cd crates/aletheia-bench-ldbc/incumbents
docker compose up -d                    # neo4j, kuzu, xtdb (pinned versions)
# Load the SHARED graph into each engine first (ETL is engine/dataset specific;
# the drivers verify the graph is non-empty and fail loudly over an empty DB).
./run_all.sh --iterations 200 --warmup 20 [--datagen-dir /data/ldbc/sf1]
# ...or one engine at a time (env-overridable ITERATIONS/WARMUP):
ITERATIONS=200 WARMUP=20 ./neo4j/run.sh
```

Each driver **actually executes** its mapped queries — `neo4j/run.sh` via
`cypher-shell`, `kuzu/run.sh` by piping Cypher into the `kuzu` CLI,
`xtdb/run.sh` by POSTing EDN Datalog to the XTDB HTTP API — timing every
iteration and writing `results/<engine>_results.json` with the **same**
nearest-rank p50/p95/p99 math as the AletheiaDB harness, so the numbers line up.

**Honest capability caveats** — the incumbents are structurally asymmetric, and
the drivers record that as a fact, never as a fabricated latency
(`{not_expressible: true}`):

- **Neo4j / KuzuDB:** no native bi-temporal reconstruction → the temporal
  extension ops are **not expressible** natively.
- **XTDB:** bi-temporal (its native axis) but **not** a property graph → the SNB
  graph reads are a different modeling exercise, and it has **no** native
  ANN/vector index → the vector ops are **not expressible**.
- **Hybrid graph + vector in one planned query** is AletheiaDB-specific → **not
  expressible** as a single operation on any of the three.

See [`incumbents/README.md`](../../crates/aletheia-bench-ldbc/incumbents/README.md)
for the full capability table and footnotes. **Never** claim an engine "ran"
unless you actually ran its driver on the box against loaded data.

---

## 7. Interpreting results

The harness emits a machine-readable JSON report (schema in
[`src/report.rs`](../../crates/aletheia-bench-ldbc/src/report.rs)):

```jsonc
{
  "schema_version": 1,
  "suite": "...",
  "disclaimer": "LDBC-style, not an audited LDBC result ...",
  "generated_at": "2026-01-01T00:00:00Z",
  "hardware": { "arch": "...", "os": "...", "logical_cpus": 16, "cpu_model": "...", "total_mem_mib": 32768 },
  "config": { "source": "synthetic|datagen", "scale": "...", "seed": 42,
              "warmup": 20, "iterations": 200,
              "node_count": ..., "edge_count": ..., "vector_count": ..., "vector_dim": ... },
  "operations": [
    { "key": "is1_person_profile", "description": "...", "snb_analogue": "IS1",
      "category": "short_read", "iterations": 200,
      "stats": { "p50_us": 0.42, "p95_us": 0.7, "p99_us": 0.9, ... } }
  ]
}
```

- **p50 / p95 / p99** are **nearest-rank** percentiles (in microseconds) over the
  measured (post-warmup) iterations. p50 is typical latency; p99 is the tail the
  regression gate watches.
- **`vector_count`** is the number of embeddings **actually indexed** (never the
  aspirational 1M target — the report tells the truth about what ran).
- **Extension ops** (`ext_temporal_*`, `ext_vector_*`) are clearly labeled
  **non-standard** and check against the project's stated targets (temporal
  reconstruction and k-NN both target `< 10 ms`, i.e. p99 `< 10000 µs`).

**Regression gate:** a run **fails** (exit 2) if any operation's p99 exceeds the
committed baseline by more than **10%** (`--threshold`) *and* the absolute noise
floor (`--min-abs-delta-us`, default 5 µs). A missing operation is treated as a
failure. The gate is **AletheiaDB-only** and **non-gating on shared CI** — it is
informational unless you wire it into a required check.

**Honesty disclaimer:** this is an *LDBC-style* suite, **not** an audited LDBC
result, and (unless `--datagen-dir` is used) it runs synthetic data. Absolute
numbers are hardware-dependent (the report captures the CPU/RAM it ran on);
compare like-for-like on the **same** box. Full methodology and every deviation
from the LDBC audit rules:
[`../../crates/aletheia-bench-ldbc/docs/METHODOLOGY.md`](../../crates/aletheia-bench-ldbc/docs/METHODOLOGY.md).
