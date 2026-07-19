# AletheiaDB Architecture & Development Guidelines

## ⚠️ CRITICAL: NEVER COMMIT DIRECTLY TO TRUNK ⚠️

**TRUNK IS A PROTECTED BRANCH. YOU MUST ALWAYS USE WORKTREES AND PULL REQUESTS.**

Before making ANY code changes:
1. Check current branch: `git branch --show-current`
2. If on `trunk`, STOP and create a worktree: `just worktree-new feature/your-feature-name`
3. Work in the worktree, commit there, push, and create a PR
4. NEVER use `git commit` when on trunk - there is a pre-commit hook to prevent this

**The ONLY acceptable commits to trunk are automated merges from approved PRs.**

This is enforced by a pre-commit hook that will block direct commits to trunk.

## Project Overview

AletheiaDB is a high-performance bi-temporal graph database written in Rust. It tracks both **valid time** (when facts were true in reality) and **transaction time** (when facts were recorded in the database), while maintaining performance comparable to regular graph databases for current-state queries.

**Primary Use Case - LLM Integration**: Enable reasoning LLMs to query not just current knowledge, but see how that knowledge evolved over time. This allows LLMs to understand temporal context, track when facts changed, reason about causality, and detect contradictions through provenance tracking.

## Quick Architecture Reference

### Core Principles

1. **Performance First**: Current-state queries <1µs single-hop, temporal queries <10ms reconstruction
2. **Storage Efficiency**: Anchor+delta compression, <2X overhead vs non-temporal storage
3. **Correctness**: ACID guarantees via WAL, MVCC snapshot isolation, crash recovery with checkpoint-based replay

### Hybrid Storage

```
Query Engine
     │
     ├─→ Current Storage (fast path, no temporal overhead)
     └─→ Historical Storage (temporal path, anchor+delta compressed)

Recovery Flow:
   Startup → Load Checkpoint → Replay WAL → Restore Indexes → Ready
```

**Durability & Recovery:**
- **Synchronous Mode**: fsync on every commit, no data loss
- **GroupCommit Mode**: batched fsync with waiting, ACID-compliant, ~100K+/sec
- **Async Mode**: background fsync, eventual consistency, ~500K+/sec
- **Recovery**: Checkpoint-based WAL replay, <5s for 10K nodes/50K edges
- **Checkpoints**: Periodic snapshots for fast startup without full WAL replay

**See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for complete architecture documentation.**

## Rust Coding Standards

**See [docs/CODING_STANDARDS.md](docs/CODING_STANDARDS.md) for comprehensive coding guidelines.**

### Quick Reference

| Guideline | Rule | Why |
|-----------|------|-----|
| **Type Safety** | Use newtype wrappers for IDs (NodeId, EdgeId) | Prevents ID mix-ups |
| **ID Security** | Always validate IDs, keep `new_unchecked()` as `pub(crate)` | Prevents overflow/DoS attacks |
| **Error Handling** | Never `.unwrap()` in production | Prevents panics |
| **Allocations** | Reuse buffers, pre-allocate when size known | Reduces GC pressure |
| **Concurrency** | Lock-free structures for hot paths | Maximizes throughput |
| **Async** | Only for I/O, not CPU-bound work | Avoid overhead |
| **Unsafe** | Document safety invariants with `// SAFETY:` | Required for correctness |

**Critical**: IDs exceeding `MAX_VALID_ID` (u64::MAX - 1000) are rejected to prevent DoS attacks.

### Lock Acquisition Order

When code needs more than one AletheiaDB write-path synchronization primitive, always acquire them in this order to prevent deadlocks:

1. `current_timestamp`
2. `wal`
3. `historical`
4. `temporal_indexes`
5. `id generators`
6. `outgoing`
7. `incoming`

Current implementation notes: `wal` is a `ConcurrentWalSystem`, `temporal_indexes` uses internal DashMap sharding, and `node_id_gen`, `edge_id_gen`, and `version_id_gen` are atomic `IdGenerator`s rather than Mutexes. The order still defines the contract for future changes: never acquire an earlier primitive while holding a later one.

If code must acquire both adjacency indexes, acquire `outgoing` before `incoming`. Neither adjacency index may call back into `historical`, `wal`, or `current_timestamp` while held.

## Testing Requirements

**See [TESTING.md](TESTING.md) for detailed testing instructions.**

### Coverage Requirements

- **Minimum 85% line coverage** (current: 86.45%)
- **Minimum 88% function coverage** (current: 89.10%)
- **Minimum 88% region coverage** (current: 88.91%)

### Quick Commands

```bash
just test              # Run all tests
just coverage          # Generate HTML coverage report
just coverage-check    # Verify coverage meets thresholds
just bench             # Run benchmarks (includes recovery benchmarks)
just check-all         # Full quality check (tests, coverage, lint)
```

### Mandatory Benchmarks

All performance-critical features must include benchmarks:

- **Current-state queries**: <1µs single-hop traversal
- **Temporal queries**: <10ms reconstruction
- **WAL operations**: Throughput targets per durability mode
- **Recovery operations**: <5s for medium datasets (10K nodes, 50K edges)
  - Checkpoint creation/loading
  - WAL replay with various entry counts
  - Full crash recovery scenarios
- **Vector search**: <10ms k-NN (k=10, 1M vectors)
- **Hybrid queries**: <30ms graph+vector+temporal

### Miri - Undefined Behavior Detection

All `unsafe` code must be validated with [Miri](https://github.com/rust-lang/miri).

**Quick Commands:**
```bash
just miri-setup        # Install miri (one-time)
just miri              # Run miri on all tests
just miri-test name    # Run specific test
```

**When to run Miri:**
- Before committing any changes to `unsafe` blocks
- After modifying SIMD, lock-free, or concurrent code
- When working with raw pointers, transmute, or FFI

**See [docs/MIRI.md](docs/MIRI.md) for complete guide, configuration, and troubleshooting.**

## Major Features

### Write-Ahead Log (WAL)

Striped lock-free ring buffer architecture for high-throughput concurrent writes.

**Performance:**
- **Synchronous**: ~1.5ms latency, ~600/sec throughput, Full ACID
- **GroupCommit**: ~10-50ms latency, ~100K+/sec throughput, Full ACID
- **Async**: <100ns latency, ~500K+/sec throughput, Eventual durability

**Batch Append API (Issue #219):**
For high-throughput workloads with multiple operations, use `append_batch()` for significant performance improvements:
- Single atomic LSN allocation for all operations
- Better CPU cache locality during serialization
- 20-50% throughput improvement for batch sizes > 10

**See [docs/WAL.md](docs/WAL.md) for comprehensive WAL documentation.**

### Persistence Systems

AletheiaDB provides four persistence layers for different needs:

**1. WAL (Write-Ahead Log)**
- Transaction durability and crash recovery
- Required for data safety
- ~100K+/sec throughput (GroupCommit mode)

**2. Index Persistence**
- Fast cold starts (6-30x faster than WAL replay)
- Saves current state to disk
- Zstd compression (60-75% size reduction)

**3. Cold Storage (Redb)**
- Unlimited bi-temporal history on disk
- Three-tier architecture (Hot RAM → Warm Cache → Cold Disk)
- Enables time-travel queries over years of data

**4. Backup / Restore (`*.albk`)**
- Portable single-file artifact capturing complete bi-temporal state (hot + cold tiers)
- Atomic write (temp → rename); consistent point-in-time snapshot at WAL LSN
- `AletheiaDB::backup(path)` / `::restore(path)` / `::restore_to_data_dir(path, dir)`
- CLI: `aletheia backup <path>` / `aletheia restore <path>`
- See [docs/guides/backup-restore.md](docs/guides/backup-restore.md)

**See [docs/guides/PERSISTENCE.md](docs/guides/PERSISTENCE.md) for comprehensive persistence documentation.**

### Vector Storage & Indexing

Dense vector embeddings as first-class properties with HNSW k-NN search for semantic similarity.

**Quick Start:**
```rust
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use aletheiadb::index::vector::{HnswConfig, DistanceMetric};

let db = AletheiaDB::new();

// Enable vector indexing
db.vector_index("embedding")
    .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    .enable()?;

// Store node with embedding - automatically indexed!
let node_id = db.create_node("Document",
    PropertyMapBuilder::new()
        .insert("title", "Introduction to Rust")
        .insert_vector("embedding", &embedding)
        .build()
)?;

// Find similar nodes
let similar = db.find_similar(node_id, 10)?;
```

**Key Features:**
- Multi-property vector indexes (multiple embeddings per database)
- Temporal vector indexes (track embedding evolution over time)
- Semantic drift tracking (detect knowledge changes)
- Full bi-temporal versioning

**See:**
- [docs/guides/vector-search-integration.md](docs/guides/vector-search-integration.md) - Complete API reference
- [docs/guides/vector-search-performance.md](docs/guides/vector-search-performance.md) - Tuning guide
- [docs/VECTOR_SEARCH_DESIGN.md](docs/VECTOR_SEARCH_DESIGN.md) - Architecture and roadmap

### Hybrid Query API

Unified API combining **graph traversal + vector similarity + bi-temporal queries**.

**Quick Start:**
```rust
use aletheiadb::query::QueryBuilder;

// Simple: Graph + Vector hybrid
let results = db.traverse_and_rank(alice_id, "KNOWS", &bob_embedding, 10)?;

// Complex: Full hybrid with builder
let results = db.query()
    .as_of(valid_time, tx_time)
    .start(alice_id)
    .traverse("KNOWS")
    .rank_by_similarity(&bob_embedding, 10)
    .filter(Predicate::gt("score", 0.8))
    .execute(&db)?;
```

**Performance Targets:**
- Single node lookup: <1µs
- 3-hop traversal: <100µs
- k-NN search (k=10, 1M vectors): <10ms
- Graph+Vector hybrid: <20ms
- Full hybrid (temporal): <30ms

**See [docs/guides/hybrid-query-guide.md](docs/guides/hybrid-query-guide.md) for complete guide.**

### Tiered Storage

Three-tier storage architecture for unlimited historical depth while preserving current-state query performance.

**Architecture:**
- **Hot Tier (RAM)**: Current state, 22-70ns lookup
- **Warm Tier (LRU Cache)**: Recently accessed history, <1µs lookup
- **Cold Tier (Redb)**: Compressed historical versions, <1ms lookup

**Quick Start:**
```rust
use aletheiadb::{AletheiaDB, config::AletheiaDBConfig};
use aletheiadb::config::HistoricalConfigBuilder;
use std::time::Duration;

// Configure cold storage via unified config builder
let config = AletheiaDBConfig::builder()
    .historical(
        HistoricalConfigBuilder::new()
            .enable_cold_storage(true)
            .cold_storage_path("data/cold.redb")
            .migration_age_threshold(Duration::from_secs(3600)) // 1 hour
            .max_hot_versions(1000)
            .build(),
    )
    .build();

// Cold storage automatically initialized!
let db = AletheiaDB::with_unified_config(config)?;
```

**Key Features:**
- Unlimited historical depth on disk
- Pure Rust implementation (no FFI dependencies)
- Current-state performance unchanged (22-70ns)
- Configurable migration policies (age, memory thresholds)
- Zstd/LZ4 compression (3-5x compression ratio)
- LSN-based WAL truncation for efficient recovery
- Latency metrics with percentiles (p50, p95, p99)

**See [docs/guides/tiered-storage-guide.md](docs/guides/tiered-storage-guide.md) for complete guide.**

### MCP Server (Claude Integration)

Model Context Protocol server enabling LLMs to interact with AletheiaDB.

**Quick Start:**
```bash
# Run the MCP server (communicates over stdio). Authentication is required
# by default: without a credential the server exits 1 at startup.
ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$(openssl rand -base64 32)" \
  cargo run --bin aletheia-mcp --features mcp-server

# Or explicitly opt into anonymous mode (local development only):
ALETHEIADB_AUTH_MODE=anonymous cargo run --bin aletheia-mcp --features mcp-server
```

**Available Tools:**
| Category | Tools |
|----------|-------|
| **Nodes** | `get_node`, `create_node`, `update_node`, `delete_node`, `delete_node_cascade`, `retract_node`, `list_nodes`, `count_nodes` |
| **Edges** | `get_edge`, `create_edge`, `update_edge`, `delete_edge`, `retract_edge`, `get_outgoing_edges`, `get_incoming_edges` |
| **Batch** | `apply_batch` (ordered multi-op write batch committing all-or-nothing in one transaction; edge ops may reference batch-created nodes via `$alias`/`$<index>` local refs; see below) |
| **Traversal** | `traverse` (multi-hop graph traversal; optional bi-temporal `as_of_valid_time`/`as_of_transaction_time`) |
| **Vector** | `find_similar`, `enable_vector_index`, `list_vector_indexes` |
| **Embeddings** | `embed_query`, `embed_text`, `semantic_search`, `create_node_with_embedding`, `update_node_embedding` (generate embeddings from text and run text-based semantic search; require the `embeddings` feature + a configured model, else return a structured unavailable/precondition error — see [docs/EMBEDDINGS.md](docs/EMBEDDINGS.md#mcp-embedding-tools)) |
| **Semantic** | `semantic_path`, `concept_analogy`, `concept_mean`, `find_duplicate_candidates`, `semantic_horizon`, `context_aspects` (read-only analysis over the stable `semantic-search` cohort; gated on the `semantic-search` feature — return `FAILED_PRECONDITION` with `required_feature` when absent; see [docs/guides/mcp-semantic-search-tools.md](docs/guides/mcp-semantic-search-tools.md)) |
| **Temporal** | `get_node_at_time`, `get_edge_at_time`, `find_nodes_at_time` (point-in-time find by label/property, no NodeId needed), `temporal_extent` (dataset's queryable bi-temporal extent; optional by_label breakdown), `get_belief_revisions` (audit when/why the database changed its mind about a node/edge — classified revision sequence + confidence trajectory; requires the `semantic-temporal` feature; see below) |
| **Changefeed** | `list_changes` (pull: what changed in a tx-time window), `await_changes` (push long-poll: block for the next committed changes; see below) |
| **Hybrid** | `hybrid_query` (combined graph + vector + temporal) |
| **Lineage** | `lineage_upstream` / `lineage_downstream` (fact-to-fact derivation closure in both directions; the write tools take an optional `derived_from`) |
| **Query** | `query` (execute a single read-only Cypher/AQL statement; see below) |
| **Schema** | `get_schema` (node labels, edge types, and property keys, each with counts; optional bi-temporal `as_of_valid_time`/`as_of_transaction_time`) |
| **Stats** | `database_stats` (holistic snapshot: current size, bi-temporal depth + anchor/delta compression, hot/warm/cold tier distribution, WAL state; no arguments) |

**Atomic multi-write batches (Issue #3231)**: `apply_batch` accepts an
**ordered** array of write operations (`create_node`, `create_edge`,
`update_node`, `update_edge`, `delete_node`, `delete_edge`, each supporting
the #3221 optional `valid_time`) committing **all-or-nothing** in one
`WriteTransaction` (single WAL batch append / GroupCommit fsync) — an LLM
builds an entity-with-relationships subgraph in ONE call instead of N calls
with N−1 possible partially-committed states. A `create_node` may carry a
`ref` alias; later edge operations reference batch-created nodes as
`"$alias"` or positional `"$<index>"` endpoints, freely mixed with committed
integer ids; forward/unknown/duplicate refs, malformed ops, and over-cap
batches (default 1000 ops, `with_max_batch_operations`, limit echoed per
#3226) are rejected statically **before any transaction opens**. Every
per-operation error carries `details.failed_op_index` (JSON `null` for
commit-phase failures like a retriable `CONFLICT` and for the over-cap
rejection; absent on top-level malformed-request errors); any acknowledged
failure means **zero** writes take effect (narrow crash-during-commit-flush
caveat until WAL transaction framing lands: #3413).
In-batch `delete_node` honors the #3209 DETACH contract against committed
AND batch-created edges (batch-local adjacency ledger; distinct edges, a
self-loop counts once). Success returns per-op results in input order (ids +
version ids for creates/updates) and a `ref_map` alias→committed-id. v1
limits: no update/delete of batch-created refs, one write per committed
entity per batch, no version_id on deletes. See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#atomic-multi-write-batches-apply_batch).

**`query` tool (read-only Cypher/AQL):** Lets an LLM answer a multi-hop,
filtered, temporally-scoped question with **one declarative statement** instead
of chaining `get_node`/`traverse`/filter calls. Accepts `language`
(`"cypher"` | `"aql"`), `query` (the statement), optional `params` (`$param`
bindings, Cypher only — numeric arrays are treated as embeddings), and `limit`
(default 100, max 10000). Returns `{language, columns, rows, row_count,
truncated}`. It is **read-only**: mutating clauses
(CREATE/MERGE/SET/DELETE/REMOVE/DETACH/DROP/CALL/FOREACH/LOAD) are rejected
before execution and never write. Errors come back as a structured
`{error:{kind, message, clause?, language}}` payload (kinds: `invalid_request`,
`read_only_violation`, `language_unavailable`, `parse_error`,
`unsupported_construct`, `invalid_params`, `runtime_error`) so the caller can
self-correct. When the `cypher` feature is not compiled in, `language:"cypher"`
returns `language_unavailable` (AQL is always available). The query tool's
`kind` field is preserved verbatim; the uniform `code`/`retriable` fields
below are carried additively alongside it. See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md).

**Structured error codes with retriable flag (Issue #3234)**: Every MCP tool
error response is `{"error": {"code", "message", "retriable", "details"?}}`.
`code` is drawn from a small, stable enum -- `NOT_FOUND`, `INVALID_ARGUMENT`,
`CONSTRAINT_VIOLATION`, `FAILED_PRECONDITION`, `CONFLICT`, `UNAVAILABLE`,
`INTERNAL` -- so an LLM/caller branches on category (retry transient errors,
repair invalid arguments, escalate the rest) with zero substring matching.
`message` preserves the pre-existing free text (the change is additive);
`retriable` is `true` **only** for transient classes (timeouts, clock skew,
serialization/write conflicts -- `UNAVAILABLE` and most `CONFLICT`s), always
`false` for caller-fault classes (not-found, invalid-argument, constraint,
failed-precondition); `details` carries per-code structured metadata (e.g.
the #3209 DETACH refusal is `FAILED_PRECONDITION` with
`details.connected_edges`; a unique violation is `CONSTRAINT_VIOLATION` with
`details.existing_node_id` -- the legacy top-level fields remain alongside).
Recovery loop: `retriable: true` -> retry with backoff; `INVALID_ARGUMENT` /
`FAILED_PRECONDITION` / `CONSTRAINT_VIOLATION` -> repair the call from
`message` + `details` and re-issue; otherwise escalate. Codes may be added
over time but never change meaning; treat unknown codes as non-retriable. See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#structured-error-codes-and-the-retriable-contract).
**The HTTP surface now shares this exact nested envelope (breaking change):**
`AletheiaHttpError` emits `{"error":{"code","message","retriable","details"?}}`
(with `trace_id`, when present, a **top-level** sibling of `error`) — the legacy
flat HTTP body (`{"success":false,"error":"<msg>","code":…}`, top-level
`success`/`error`/`code`/`retriable`/`details`) has been **removed**, so HTTP and
MCP error bodies are byte-shape-identical. Success responses are unchanged
(`{"success":true,"data":…}`).

**Database stats (Issue #3222)**: `database_stats` (no arguments) returns a
holistic snapshot in one call so an LLM/operator can orient itself before
querying: `current` (node/edge counts), `historical` (total/unique version
counts plus anchor/delta breakdown and `compression_ratio` — the bi-temporal
depth held **in RAM**; versions migrated to the cold tier are counted under
`cold_storage` instead), `cold_storage` (`{enabled: false}` when the
disk tier is not configured — never misleading zeros — or counters plus a
`tier_access` hot/warm/cold read distribution when it is), and `wal`
(`enabled`, `durability_mode` token, `current_lsn`, `total_appends`,
`healthy`). Backed by the public `AletheiaDB::stats()` returning a
serializable `DatabaseStats`; every field is an O(1)/cached counter read
(no version scans; see Issue #212), so it is safe to call frequently. See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#database-stats-and-storage-tier-health-database_stats).

**Per-query resource limits (Issue #3368)**: the wall-clock-timeout and
result-byte-cap enforcement that guards the `query` tool now also governs the
read tools — `traverse`, `hybrid_query`, `find_similar`, `get_node_at_time`,
`get_edge_at_time`, `find_nodes_at_time`, plus the six #2907 semantic-search
analysis tools (`semantic_path`, `concept_analogy`, `concept_mean`,
`find_duplicate_candidates`, `semantic_horizon`, `context_aspects`) enrolled for
uniform coverage — wrapped at the dispatch seam
(`RESOURCE_LIMITED_READ_TOOLS`), reusing the `query` tool's timeout thread-race
and bounded in-flight-worker DoS guard. A breach returns `RESOURCE_EXHAUSTED`
with `details.dimension` (`wall_clock_timeout`, retriable; `result_bytes`,
non-retriable) — but via **tool-agnostic** emitters that produce the plain
#3234 envelope (`{error:{code,message,retriable,details}}`); unlike the `query`
tool's builders these carry **no** `kind` and **no** `language` field (a
wrapped read tool is not a query language) and their remediation is
tool-neutral (no `limits.timeout_ms`/`limits.max_response_bytes` advice, since
these tools have no per-call `limits` override in v1). Ordering is cursor
(#3360) → resource cap → token budget (#3353). **Overhead:** the output is
unchanged under the default config, but the zero-overhead inline path applies
**only** when the effective timeout is `0` (the `disabled()` config); under the
*default* config the effective timeout is 30_000 ms, so each covered call runs
on a timeout-race worker (thread-spawn + mpsc + in-flight-CAS, exactly like the
`query` tool) — response-identical but not free. The per-call worker-spawn cost
on hot-path reads (including cheap `get_node_at_time`/`get_edge_at_time`) has a
quantifying micro-benchmark deferred to Lane-2. The `max_in_flight_queries` cap
(default 64) is a **single shared pool** across the `query` tool and these
wrapped read tools, so a flood of slow calls to one can make the others return
`UNAVAILABLE` (bounded, retriable); a per-class sub-budget is a Lane-2
follow-up. `database_stats` additively surfaces a
`resource_limits` block (`timeout_terminations`, `byte_cap_terminations`,
`override_rejections`) from process-lifetime atomic counters (the
`DatabaseStats` struct/storage layer are untouched; row-cap breaches are **not**
counted — they self-disclose via `truncated`/`has_more`). **v1 scope for these
read tools:** server defaults only (no per-call `limits` override), **post-hoc**
byte cap (the response is fully serialized then rejected if over cap). **Deferred
to Lane-2:** memory-budget dimension, true engine-level cancellation, Rust
builder API, benchmark-gated fast-path proof, concurrency soak, HTTP in-flight
parity, incremental byte-cap for these tools. See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#extended-to-the-read-tools-issue-3368-residue).

**Valid-time writes (Issue #3221)**: `create_node`, `create_edge`,
`update_node`, `update_edge`, `delete_node`, and `delete_edge` accept an
optional `valid_time` (ISO 8601 / RFC 3339 or microseconds since epoch) so a
caller/LLM can record when a fact became (or stopped being) true in the real
world, independent of when it was recorded. Omitting it reproduces prior
behavior exactly (valid time defaults to the transaction time). On
`delete_node`, `valid_time` is not supported together with `detach: true`
(cascade delete does not support backdating). Transaction time is always
system-assigned and cannot be set. See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#recording-facts-at-a-specific-valid-time).

**Valid-time retraction (Issue #3230)**: `retract_node` / `retract_edge`
close an entity's valid-time interval at an optional `valid_time` (default
now) **without deleting its history** -- `AS OF VALID_TIME` before `T` still
returns the fact, at/after `T` does not, and `AS OF SYSTEM_TIME` before the
retraction's commit still shows it open-ended (append-only). `retract_node`
mirrors the #3209 safe-by-default contract (refuses with `connected_edges`
unless `detach: true`, which co-retracts edges and reports
`edges_retracted`); re-retraction is an idempotent no-op returning the
existing `[valid_from, valid_to)` interval. Rust API:
`retract_node(_detach)` / `retract_edge` on `AletheiaDB`. See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#retracting-a-fact-closing-valid-time).

**Point-in-time (AS OF) traversal (Issue #3225)**: `traverse` accepts optional
`as_of_valid_time` / `as_of_transaction_time` (ISO 8601 / RFC 3339 or
microseconds since epoch), independently settable (valid-time only, tx-time
only, both, or neither), so an LLM can ask "who did Alice know on
2024-01-01?" in one call instead of stitching together point-in-time node
lookups edge-by-edge. When a temporal coordinate is supplied, traversal
follows only edges and nodes valid at that bi-temporal point (edges created
after the coordinate, or whose valid interval doesn't contain it, are
excluded; a node no longer valid at the coordinate stops traversal from
continuing past it) and node properties reflect their state at that
coordinate; when neither is supplied, behavior is unchanged (current-state
traversal). Each dimension defaults independently to the current time when
the *other* one is supplied but it isn't, mirroring `get_schema`'s `as_of_*`
convention -- note that recalling a since-deleted edge requires anchoring
*both* dimensions before the deletion, not just `as_of_valid_time` (see the
guide below for why). See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#point-in-time-as-of-graph-traversal).

**Point-in-time (AS OF) node find (Issue #3236)**: `find_nodes_at_time`
resolves *"the Person named Alice, as of 2024-01-01"* in one call, without a
prior `NodeId` -- the entry-point resolver the #3225 AS OF traversal assumes
the caller already has. It accepts `label` (required), optional
`property_key` + `property_value` (both-or-neither, mirroring `list_nodes`),
`valid_time` (required, ISO 8601 / RFC 3339 or microseconds since epoch),
optional `transaction_time` (defaults to now), and `limit`/`offset` (same
clamps as `list_nodes`; results sorted by node id for stable pagination).
Each returned node is reconstructed **as it existed** at
`(valid_time, transaction_time)` -- not its current state -- and nodes that
did not exist (or whose property value did not hold) at that point are
excluded. With both dimensions at now, the result set equals the
current-state `list_nodes` property lookup *for nodes whose valid interval
has begun* (a #3221 future-dated `valid_from` node is in current state but
not yet visible at `(now, now)`). The response echoes the resolved
`valid_time`/`transaction_time` (RFC 3339). Nodes since deleted from current
state are found too (candidates come from history, not the live index), but
recalling superseded or deleted states requires anchoring *both* dimensions
before the superseding write, exactly as with #3225. Backed by the
`AletheiaDB::find_nodes_at_time` / `find_nodes_by_property_at` convenience
API (returning `NodesAtTime`). v1 scans historical version heads, capped at
the same `max_schema_as_of_entities` limit bi-temporal `get_schema` uses
(default 50,000, lowest node ids kept); when truncated the response sets
`sampled: true` and `total_matching` counts matches within the sampled
candidate set only. Properties are reconstructed only for label matches; a
temporal label index is a deliberate follow-up. See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#point-in-time-as-of-node-find-by-label-and-property).

**Temporal bounds on read responses (Issue #3232)**: every node/edge read
response (`get_node`, `create_node`, `update_node`, `get_edge`, `create_edge`,
`update_edge`, `list_nodes`, `traverse`, `get_outgoing_edges`,
`get_incoming_edges`, `find_similar`, `hybrid_query`, and all
`get_*_at_time`/`at_valid_time`/`at_transaction_time` tools) carries an
additive `temporal` block stamping the bi-temporal bounds of the exact
version returned: `valid_from`/`valid_to`/`transaction_from`/`transaction_to`
as RFC 3339 strings (UTC, `Z` suffix) plus `is_current`. Open-ended bounds are
explicit JSON `null` (present, never omitted); `is_current` is `true` iff the
version's transaction interval is open AND the wallclock now falls within its
valid interval (false for superseded versions returned by point-in-time reads
and for expired or not-yet-valid facts). In the rare case version metadata
cannot be loaded, the whole `temporal` block is omitted (mirroring
`provenance`). The shape is identical for nodes
and edges, current and point-in-time; `get_node_history` keeps its existing
microseconds-as-string format. See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#temporal-bounds-on-read-responses).

**Vector properties are elided by default (Issue #3220)**: `get_node`,
`list_nodes`, `get_edge`, `list_edges`, `get_outgoing_edges`,
`get_incoming_edges`, `traverse`, `find_similar`, `hybrid_query`, and
`find_nodes_at_time` replace vector/embedding properties with a
`{type, dim, elided: true}` descriptor
(or `{type: "sparse_vector", dim, nnz, elided: true}` for sparse vectors)
instead of the raw float array -- a single embedding can otherwise cost
thousands of tokens of context an LLM can't reason over. Pass
`include_vectors: true` on the request to receive the full array. This does
not affect `find_similar`'s `score` or `hybrid_query`'s `similarity_score`,
which are always returned in full, nor the write path (`create_node`,
`update_node`, `create_edge`, `update_edge`) or the single-entity
temporal/history tools (`get_node_at_time`, `get_edge_at_time`,
`get_node_history`), which have no `include_vectors` flag and always return
full vectors.

**Token-budget-aware responses (Issue #3353)**: the twenty budgetable read
tools — `get_node`, `list_nodes`, `get_edge`, `list_edges`,
`get_outgoing_edges`, `get_incoming_edges`, `traverse`, `find_similar`,
`semantic_search`, `hybrid_query`, `query`, `find_nodes_at_time`,
`get_node_history`, `get_schema`, `semantic_path`, `concept_analogy`,
`concept_mean`, `find_duplicate_candidates`, `semantic_horizon`,
`context_aspects`
(the single source of truth is `BUDGETABLE_READ_TOOLS`; not *every* read tool —
e.g. `get_node_at_time`, `get_edge_history`, `diff_node_versions`,
`temporal_extent`, `database_stats`, `count_nodes` are out of scope) — accept an
optional `max_response_tokens` (estimated as `ceil(utf8_bytes / 4)`) or the
byte-exact `max_response_bytes`, so a context-bounded caller can say "spend at
most N tokens answering this" instead of guessing a row `limit`. These three
parameters (`max_response_tokens`, `max_response_bytes`, `priority_properties`)
are injected into each budgetable tool's advertised `inputSchema.properties`, so
they are machine-discoverable, not just described in prose. The serialized
response — **including its own truncation metadata** — is guaranteed not to
exceed the stated budget (hard contract, CI conformance sweep 256..32K tokens,
0 overruns). This bound governs **success** responses; a structured *error*
response (e.g. the too-small-budget `INVALID_ARGUMENT` below) is itself small
and returned intact. The rare non-object success payload (JSON scalar/array or
plain text) cannot degrade along the entity ladder but is still held to the byte
cap via a disclosed truncation marker (never emitted unbounded). Over budget the
response degrades along a deterministic, disclosed ladder — full →
`elided_properties` (bulky property values become `{elided: true, ...}`
descriptors, reusing #3220's convention, and only when the descriptor is
actually smaller than the value, so the ladder never enlarges the response) →
`entity_summaries` (properties reduced to protected keys;
ids/labels/relationships/temporal coordinates/provenance/scores always survive)
→ `counts_and_handles` (entity arrays truncated to the prefix that fits, with the
object's own `count`/`row_count`/`has_more`/`next_offset`/`truncated` siblings
rewritten to describe the retained prefix so a paginating caller sees no gap and
no duplicate) — carrying a `budget` block that names the rung applied per
section. Every elision/truncation site carries a **fetch handle**: a concrete
`get_node`/`get_edge` call with `include_vectors: true` for an elided entity, a
`get_node_at_time`/`get_edge_at_time` call for an elided history version (parent
id + that version's own coordinates, not the current state), and for a truncated
array a concrete `offset`-advancing resume call on paginated tools
(`list_nodes`/`traverse`/`find_nodes_at_time`) or an honest "re-request with a
larger budget" disclosure on non-paginated ones — so nothing is lost: an agent
following handles reconstructs the full response. `priority_properties` names
properties to protect; they out-survive unprotected ones at every rung.
`find_similar`/`hybrid_query` never drop or reorder ranked results to meet a
budget (only per-result payloads degrade), and temporal responses never omit
temporal coordinates. A budget too small for even the minimal rung returns a
#3234 `INVALID_ARGUMENT` stating a minimum viable budget that is self-consistent
(re-issuing at the reported `min_viable_tokens` succeeds) — never a silently
empty success. Omitting the budget parameters reproduces prior behavior exactly,
and a **misspelled/unknown budget key** (e.g. `max_tokens`) is ignored — the
full response is returned — so use the exact key names. Write/admin tools are out
of scope. See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#token-budget-aware-responses-issue-3353).

**Temporal extent (Issue #3238)**: `temporal_extent` reports the dataset's
queryable bi-temporal extent — the earliest/latest valid-time and
transaction-time coordinates across recorded history (including
expired/superseded versions and delete tombstones) as RFC3339 strings — so
an LLM can calibrate `AS OF` queries to land inside real data instead of
misreading an out-of-range empty result as "the fact never existed". An
empty database returns explicit `null`s (never epoch 0); `latest` is the
max of interval starts and *closed* ends, so the open-interval sentinel
never leaks. Overall bounds are O(1) reads of a write-time-maintained
aggregate and only ever widen while the server runs (cacheable per
session). Optional `by_label: true` adds per-node-label / per-edge-type
bounds folded from hot-tier history. Overall bounds also span history
migrated to the cold tier across restarts (Issue #3389): the cold store
persists its per-dimension extent bounds and they are merged into the
aggregate at startup (the per-label breakdown remains hot-tier-only). See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#discovering-the-queryable-temporal-extent-temporal_extent).

**Cursor continuation for large scans (Issue #3360)**: the bounded read tools
(`list_nodes`, `find_nodes_at_time`, `get_outgoing_edges`,
`get_incoming_edges`, `traverse`) accept an additive `use_cursor: true` on the
first call (returning an opaque `cursor` token) and a `cursor` continuation
token thereafter — passed back **with no other parameters** — for
**snapshot-anchored** paging that is consistent, duplicate-free, and gap-free
under concurrent writes. Every page of one scan is evaluated at the
bi-temporal coordinate captured on the first page (disclosed as
`snapshot_valid_time`/`snapshot_transaction_time`), leveraging the existing
point-in-time read semantics: a row **created** after the first page is never
seen, a row **deleted** after it is still seen, and the union of all pages
equals exactly the unbounded result at that one moment — **up to the candidate
cap** (the node scans route through the #3236 finders, capped at
`max_schema_as_of_entities`, default 50,000, lowest ids; a page with
`sampled: true` means the scan is bounded by that cap, not exhausted — narrow
with a property filter for full coverage). The node/adjacency tools use a
**keyset** (ascending id) that avoids re-emitting prior result pages (no
dup/gap); candidate enumeration is still O(total) per page in v1 (each page
re-runs the full candidate/adjacency scan — a true depth-independent keyset
seek is a follow-up); `traverse` pins the snapshot but continues by an internal
offset over its deterministic DFS in v1. Note `use_cursor:true` on
`get_outgoing_edges`/`get_incoming_edges`/`traverse` with no `as_of` answers
"as of first-page now" via the bi-temporal-at-now path, and therefore
**excludes future-valid** (`valid_from > now`) edges/nodes that a plain
current-state (non-cursor) call would return — the same tradeoff #3236
documents for point-in-time reads. Tokens are opaque,
printable, bounded base64url strings signed with a per-process secret (so
tampered/wrong-tool tokens are rejected `INVALID_ARGUMENT`, never wrong data;
they do not survive a server restart). Cursors have a documented, configurable
**TTL** (default 5 min, `cursor_ttl_seconds`) and a per-connection **cap** on
concurrently live cursors (default 128; both via
`AletheiaMcpServer::with_cursor_config`); resuming after expiry or exceeding
the cap returns `FAILED_PRECONDITION` with remediation guidance, and expired
cursors pin no storage. The design is **stateless** (all resume state is in
the token) with only a tiny in-process registry for cap enforcement. Cursor
composition with #3353 token budgets is **live** (both features have landed):
within one call the cursor page is produced first, then the token budget shapes
that page, so a budget-limited page ends smaller while the cursor still resumes
the same snapshot-anchored scan (v1 caveat: the continuation key advances by the
underlying scan, not the last budget-trimmed row, so page a budgeted cursor scan
losslessly via the budget ladder's offset handle or a larger budget — see the
guide). Offset paging (#3226)
remains unchanged for backward compatibility. The `query` tool returns a structured
`unsupported_construct` error for cursor requests in v1 (no silent fallback);
`list_edges` is not cursorable (it does not enumerate edges — use the
adjacency tools). See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#paging-large-results-cursor-continuation).

**Authentication & RBAC (Issue #3350)**: both server surfaces (MCP and
HTTP) require an API key by default and refuse to start with zero
credentials; anonymous access is an explicit opt-in
(`ALETHEIADB_AUTH_MODE=anonymous`) with a prominent warning. Four roles
(admin/writer/reader/metrics) gate every tool/route via a classification
kept in lockstep with [docs/guides/access-control-matrix.md](docs/guides/access-control-matrix.md)
by CI conformance tests. Keys are stored as SHA-256 hashes
(`{data_dir}/auth/keys.json`, 0600), verified constant-time and re-verified
per call (revocation is immediate); auth failures are a uniform
`UNAUTHENTICATED` (never distinguishing missing/unknown/revoked), role
denials are `PERMISSION_DENIED` with `details.required_class` /
`details.principal_role` — both additive to the #3234 enum, both
`retriable: false`. Since the #3234 HTTP error-envelope unification these
denials render identically on **both** surfaces as the nested
`{"error":{"code","message","retriable","details"}}` body (the HTTP surface no
longer emits the legacy flat `{"success":false,…}` shape, and its 403 now carries
`error.details.{required_class,principal_role}` too). Authenticated writes stamp the principal's name into
version provenance (`provenance.principal`, composing with the
caller-supplied `source`) on the structured create/update node/edge
paths of both surfaces — deletes/retracts and HTTP AQL-statement writes
(`execute_query`/`bulk_execute_query`) do NOT stamp a principal yet
(destructive-op attribution needs a WAL payload extension to survive
crash recovery; tracked as Issue #3427); anonymous writes
record no principal. MCP
sessions authenticate via `ALETHEIADB_MCP_API_KEY`; key lifecycle is served
by the HTTP `/admin/keys*` endpoints. Programmatic
`AletheiaMcpServer::new()` stays anonymous (embedded API); use
`with_auth(db, McpAuthConfig)` to serve. See
[docs/guides/security-quickstart.md](docs/guides/security-quickstart.md).

**Programmatic Usage:**
```rust
use aletheiadb::mcp::AletheiaMcpServer;
use aletheiadb::AletheiaDB;
use std::sync::Arc;

let db = Arc::new(AletheiaDB::new()?);
let server = AletheiaMcpServer::new(db);
server.serve_stdio().await?;
```

### Query Language (AQL)

Cypher-like query language with temporal and vector extensions.

**Grammar Support:**
- Graph patterns: `MATCH (n:Label)-[:REL]->(m)`
- Variable-depth traversal: `-[:KNOWS*1..3]->`
- Vector search: `SIMILAR TO $embedding LIMIT 10`
- Hybrid queries: `RANK BY SIMILARITY TO $embedding TOP 10`
- Bi-temporal: `AS OF '2024-01-15T10:00:00Z'`, `BETWEEN ... AND ...`
- Filtering: `WHERE`, `ORDER BY`, `LIMIT`, `SKIP`

**Example Queries:**
```cypher
-- Basic graph query
MATCH (n:Person {name: "Alice"})-[:KNOWS]->(friend:Person)
RETURN friend

-- Hybrid: temporal + graph + vector
AS OF '2024-06-01T00:00:00Z'
MATCH (user:User {id: $user_id})-[:VIEWED]->(item:Product)
RANK BY SIMILARITY TO $recommendation_embedding TOP 20
WHERE item.price < 100
RETURN item
ORDER BY score DESC
LIMIT 10
```

**See [docs/query-language-design.md](docs/query-language-design.md) for complete grammar and semantics.**

### Cypher Query Language

OpenCypher-compatible query language with temporal and vector extensions.

**Quick Start:**
```rust
// Enable the feature: cypher = [] in Cargo.toml
use aletheiadb::AletheiaDB;

let db = AletheiaDB::new()?;

// Basic graph query
let results = db.execute_cypher("MATCH (n:Person {name: 'Alice'})-[:KNOWS]->(friend) RETURN friend")?;

// With parameters
use std::collections::HashMap;
use aletheiadb::cypher::CypherParameterValue;
let mut params = HashMap::new();
params.insert("name".into(), CypherParameterValue::String("Alice".into()));
let results = db.execute_cypher_with_params("MATCH (n:Person {name: $name}) RETURN n", params)?;
```

**Supported Syntax:**
- Graph patterns: `MATCH (n:Label {prop: value})-[:REL]->(m)`
- Left-outer patterns (Issue #557): `OPTIONAL MATCH (a)-[:KNOWS]->(x)` -- unmatched
  patterns preserve the base row and bind null; the clause's `WHERE` and inline
  properties are scoped inside the optional pattern (they decide matched vs
  unmatched); multiple/leading `OPTIONAL MATCH` clauses supported. The first
  node of a subsequent `OPTIONAL MATCH` must be unlabeled and either unnamed
  (`()`) or name the previous clause's binding (the last node of the prior
  MATCH/OPTIONAL MATCH, or the `WITH`-projected variable) -- re-anchoring on an
  earlier variable is rejected, and comma-separated patterns per optional
  clause are rejected (no variable-binding analysis yet -- all three would
  silently produce wrong rows)
- Variable-depth: `-[:KNOWS*1..3]->` -- binds the far node to every node
  reachable within the range (Issue #548); `*min..max`, `*..max`, `*min..`,
  `*n`, and bare `*` are all honored. **Known v1 limitations**: (1) matching is
  **node-distinct / shortest-path reachability**, a deliberate simplification of
  openCypher trail (path-enumeration) semantics -- each distinct target is bound
  once at its *shortest* hop-distance, so a node whose shortest path is below
  `min` (or an anchor reached only via an in-range cycle) is not re-emitted at a
  longer in-range depth (full trail semantics is a tracked follow-up); (2) the
  open-ended upper bounds (`*` and `*min..`) are capped at depth **10**
  (`DEFAULT_MAX_TRAVERSAL_DEPTH`; a configurable cap is a follow-up).
- Directions: `->` (outgoing), `<-` (incoming), `-` (both)
- Filtering: `WHERE n.age > 18 AND n.name = 'Alice'`
- Results: `RETURN`, `RETURN DISTINCT`, `AS` aliases
- Aggregation (Issue #558): `count(*)`, `count(expr)`, `count(DISTINCT expr)`,
  `sum`/`avg`/`min`/`max`/`collect` (each with optional `DISTINCT`), with
  openCypher **implicit grouping** — non-aggregate `RETURN` items become the
  group key (`RETURN n.dept, count(*)` groups by `n.dept`; a keyless
  `RETURN count(*)` is one global row, `0` over empty input). `ORDER BY` over
  aggregate output sorts by the output column / aggregate alias.
- Ordering: `ORDER BY n.age DESC` — multi-key `ORDER BY a, b` sorts by `a`
  (primary) then `b`; openCypher null placement (nulls **last** for `ASC`,
  **first** for `DESC`)
- Pagination: `SKIP 10 LIMIT 20`
- Query chaining: `WITH b WHERE b.score > 0.5 RETURN b`
- Parameters: `$paramName`

**Aggregation v1 limitations (Issue #558):**
- **Grouping by a whole node/edge is rejected** (`RETURN n, count(*)` returns a
  structured `UnsupportedFeature` error): the single-entity row model cannot
  express node-identity grouping — group by a property (`n.id`) instead.
  `count(n)` (bare variable) is allowed and counts non-null bindings.
  `count(DISTINCT *)` is a parse error (openCypher disallows it).
- **`min`/`max`/`sum`/`avg` over mixed or non-numeric types are lenient**:
  non-numeric values are skipped by `sum`/`avg`, and `min`/`max` treat
  incomparable pairs as equal (retain input order) rather than erroring. An
  all-integer `sum` that overflows `i64` promotes to `Float` (never silently
  wraps).
- **`RETURN DISTINCT <scalar projection>`** (e.g. `RETURN DISTINCT n.dept`)
  deduplicates by entity id, **not** the projected value — a pre-existing
  projection-model limitation (property projection is not yet lowered into the
  row), independent of aggregation.
- **MCP `query`-tool rendering (cross-lane)**: aggregate rows are carried on
  `QueryRow.columns` with a null entity, but the MCP serializer
  (`query_row_to_json`, `src/mcp/server.rs`) ignores `columns` and renders the
  row as `{"entity": null, ...}`. Aggregation is correct at the
  `execute_cypher`/Rust-API level; surfacing it through MCP is a one-branch
  follow-up for the MCP lane owner (serialize `row.columns` via
  `property_value_to_json` and make `query_columns()` dynamic).

**Temporal Extensions:**
- `AS OF TIMESTAMP '2024-01-15T10:00:00Z'`
- `AS OF VALID_TIME '2024-01-15'`
- `AS OF SYSTEM_TIME '2024-01-15'` / `FOR SYSTEM_TIME AS OF '...'`
- Bi-temporal: `AS OF VALID_TIME '...' AS OF SYSTEM_TIME '...'`
- `BETWEEN '2024-01-01' AND '2024-12-31'`

**Vector Extensions:**
- `ORDER BY vector.similarity(n.embedding, $query) DESC LIMIT 10`
- `vector.cosine()`, `vector.euclidean()` distance functions
- Hybrid: graph traversal + vector ranking in one query

**See [docs/plans/2026-03-26-cypher-query-language.md](docs/plans/2026-03-26-cypher-query-language.md) for complete Cypher grammar and implementation details.**

### Graph Sharding

Domain-based horizontal scaling for datasets exceeding single-machine capacity.

**Key Features:**
- Domain-based partitioning (nodes partitioned by label)
- Edge replication for cross-shard traversal
- Two-Phase Commit (2PC) distributed transactions
- Circuit breakers for fault tolerance
- Online migration with dual-write support

**Quick Start:**
```rust
use aletheiadb::storage::sharding::{
    ShardConfig, ShardDefinition, ShardCoordinator,
};

// Define shard topology
let config = ShardConfig::new(vec![
    ShardDefinition::new(0, "shard0:9000", vec!["Person", "User"]),
    ShardDefinition::new(1, "shard1:9000", vec!["Place", "Location"]),
    ShardDefinition::new(2, "shard2:9000", vec!["Event", "Activity"]),
]);

let coordinator = ShardCoordinator::new(config);
let shard = coordinator.router().route_node("Person");
```

**When to Use:**
- Dataset exceeds single-machine RAM (~256GB → ~1.2B nodes)
- Need geographic distribution
- Require isolation between domains

**See [docs/guides/sharding-guide.md](docs/guides/sharding-guide.md) for complete guide.**

### Embedding Generation (Optional)

Optional embedding providers via feature flags (OpenAI, HuggingFace, Ollama, ONNX).

**See [docs/EMBEDDINGS.md](docs/EMBEDDINGS.md) for comprehensive user guide.**

### Derivation Lineage (Issue #3371)

Records **fact-to-fact derivation** at write time — "fact B was computed from
facts A1..An" — the complement to write-time provenance (#3224), which only
records external-source origin. Each reference is **version-pinned**
(`LineageRef { entity, version }`), so lineage refers to exactly the fact
version that was read, immune to later updates of the input.

**Rust API:** `create_node_with_lineage` / `create_edge_with_lineage` /
`update_node_with_lineage` / `update_edge_with_lineage` accept a
`derived_from: &[LineageRef]` (omit/empty == today's behavior); a nonexistent
reference fails the write with a structured error **before any commit**.
`upstream_lineage` ("what was this derived from?") and `downstream_lineage`
("what has been derived from this?" — the retraction **blast radius**) return a
depth-bounded, entry-limited `LineageView` with `has_more` (#3226) and each
entry's current-state `FactStatus` (`Current`/`Superseded`/`Absent`);
`with_as_of(ts)` scopes the closure to lineage recorded by that transaction
time. Lineage records are **immutable** and survive supersession/retraction of
the facts they reference (a retracted input still resolves in the closure,
marked `Absent`). v1 lineage index is **in-memory** (does not survive restart —
keeps the WAL format untouched during #3413; durable rehydration is a
follow-up).

**MCP surface:** the `create_node`/`create_edge`/`update_node`/`update_edge`
tools accept an optional `derived_from` array of version-pinned refs
(`[{entity_kind:"node"|"edge", id, version}]`); `lineage_upstream` /
`lineage_downstream` query the closure (args: `entity_kind`/`id`/`version`
root, `max_depth`, `limit`, `offset`, `as_of_transaction_time`) returning
entries with the version-pinned ref, `depth`, `status`, plus `has_more` /
`next_offset` (#3226). Write params stay `writer`-class; the query tools are
`reader`-class. Errors use the #3234 structured codes (`NOT_FOUND` for a
dangling ref, `INVALID_ARGUMENT` for self/cycle, `FAILED_PRECONDITION` for
already-recorded), all non-retriable. Durable persistence of lineage is a
#3413 follow-up; the #3427 attribution caveat applies. See
[docs/guides/derivation-lineage.md](docs/guides/derivation-lineage.md).

### Schema Constraints: Property Types & Required Keys (Issue #3378)

**Opt-in** per-label (node) / per-edge-type declarations that a property must be
present and/or hold a declared type; a label with no declaration stays fully
schemaless (zero behavior change, zero write-path overhead). Declared via a
builder mirroring `unique_constraint`:

```rust
use aletheiadb::core::{EntityKind, constraint::DeclaredType};
db.schema_constraint(EntityKind::Node, "Person")
    .require("name")                              // required, any type
    .require_typed("age", DeclaredType::Integer)  // required + typed
    .typed("email", DeclaredType::String)         // optional but typed
    .enable()?;                                    // -> ConformanceReport
```

`DeclaredType` = `String|Integer|Float|Boolean|Temporal|Bytes|Vector{dim:Option<usize>}`
(`Temporal` maps to `Int` micros-since-epoch; `Vector{dim:Some(d)}` requires
exactly dim `d`). Enforced at the existing pre-apply commit hook
(`check_constraints`, alongside #3218 uniqueness) for **both nodes and edges**,
so all write paths (incl. bulk import) are covered atomically — one violating op
aborts the whole transaction, zero partial writes. Updates are **PATCH**: checks
run against the effective post-merge map (a patch nulling a required key fails; a
patch not touching it passes). `enable()` scans **current state** only and
returns a `ConformanceReport` (`conforms`, counts, aggregated `violations` with
sample ids); on a populated non-conforming label it returns
`ConstraintError::NonConformingOnEnable` and declares nothing; `.dry_run()`
returns the report without applying. **Forward-only temporal**: history is never
re-scanned/invalidated (time-travel reads keep working; reads never blocked); a
backdated (`valid_time`) write is validated against the constraint set active at
its transaction time = now (AC7). API: `schema_constraint(kind,label)` builder,
`list_schema_constraints()`, `drop_schema_constraint(kind,label)`. `get_schema`
gains `declared_constraints` per label/type (declared vs merely observed keys).

**Errors (#3234):** `TypeViolation`/`MissingRequiredKey` → `CONSTRAINT_VIOLATION`,
`NonConformingOnEnable` → `FAILED_PRECONDITION` (all `retriable:false`).

**Durability:** a bitcode+CRC sidecar `{data_dir}/schema_constraints.dat` (atomic
temp→fsync→rename, tolerant/quarantining load) — ephemeral `new()` is in-memory
only — and folded into the `.albk` backup payload (round-trips through
restore). **Residue:** #3218 uniqueness constraints are WAL-persisted but NOT in
`.albk`; these schema constraints ARE.

**v1 scope:** Rust API + a minimal MCP error-classification hunk only. MCP/CLI
declaration tools and AQL/Cypher DDL (#560) are follow-ups.

**See [docs/guides/schema-constraints.md](docs/guides/schema-constraints.md).**

### Changefeed Subscriptions (Issue #3375)

Push counterpart to the #3216 `list_changes` pull feed: `AletheiaDB::subscribe_changes(filter)`
returns a `Subscription` whose bounded buffer fills with matching `ChangeRecord`s as
transactions commit — no polling. `poll()` drains non-blocking; `recv_timeout(dur)` is a
sync `Mutex`+`Condvar` long-poll (no async dep). A `ChangeFilter`
selects by node label / edge type / change type (unset dimension = match-all on that axis;
setting only labels excludes edges and vice-versa; `change_types` is a kind-independent AND).
The broadcast runs in the commit path **after** the write is durable + applied + visible and
**outside every write-path lock** (the broadcaster's locks are leaves; records are built via a
targeted O(txn-size) `historical.read()` of just that transaction's versions, so they are
byte-identical to `list_changes`). Delivery is **best-effort at-least-once**; the durable
ground truth is `list_changes`. A lagged (bounded-buffer overflow → disconnected, never
back-pressures the writer), reconnecting, or crash-surviving consumer resumes with **zero
loss** by pulling `list_changes` from its last `resume_token` (the encoded `ChangeCursor` of
the last event drained); duplicates on resume dedup by that stable cursor
`(tx_time, kind, entity_id, version_id)`. Caps are configurable via `set_changefeed_config`
(defaults: 128 subscriptions, 1024-event buffer); exceeding the subscription cap fails
`subscribe_changes` with `CapacityExceeded`. Dropping a `Subscription` deregisters it. v1 is
in-memory (no WAL change). See [docs/guides/reacting-to-change.md](docs/guides/reacting-to-change.md).

**MCP `await_changes` long-poll + HTTP SSE stream (changefeed surface):** the
`await_changes` MCP tool (read-class) wraps this primitive as a **stateless**
per-call subscribe→catch-up→block long-poll: it subscribes (capturing the
frontier so nothing is lost between catch-up and blocking), optionally catches
up from a prior `from_token` via `list_changes` (returning immediately if any
change already exists), else blocks up to `timeout_ms` (default 25000, hard cap
60000) for the next matching commit. Response:
`{changes:[…list_changes shape…], count, resume_token, timed_out, has_more}`.
Error mappings (#3234): a lagged subscription → retriable `RESOURCE_EXHAUSTED`
with `details.resume_token` (resume losslessly via `list_changes`); a
subscribe-cap breach → retriable `UNAVAILABLE`; a malformed `from_token` →
`INVALID_ARGUMENT`. It is deliberately **excluded** from the #3368 per-read
timeout, #3353 token-budget, and #3360 cursor wrappers (a long-poll is expected
to block). The HTTP surface adds `POST /changes/await` (the tool projection) and
a **route-only** `GET /changes/stream` Server-Sent Events stream (read-class,
NOT an MCP tool — like `GET /metrics`): one `data:` frame per committed change,
a terminal `event: lagged` frame carrying the resume token on overflow. Filter
via `?node_labels=…&edge_types=…&change_types=…` (comma-separated).
### Named Snapshots — Reproducible Reads (Issue #3370)

Pins a human-readable name to a bi-temporal coordinate
`(valid_time, transaction_time)`; reads through the resulting handle resolve
via the deterministic historical (`*_at_time`) path, so the same handle returns
**identical results regardless of later writes**. A snapshot is a **coordinate,
not a held resource**: it pins no storage and adds no lasting write-path
overhead (the registry is off the data write path). Creation takes the
commit-clock lock just long enough to copy one `Timestamp` (nanosecond-scale,
not literally zero); a snapshot created racing an in-flight commit inherits the
engine's standard committed-but-not-yet-applied visibility window (same caveat
as #3225/#3236). **Rust API:** `create_snapshot(name, description)` defaults
**valid-time = wallclock `time::now()`** (the engine's "now" convention, so
facts actually valid at creation are not dropped) and **transaction-time = the
commit frontier under `current_timestamp`** (race-free monotonic, so post-pin
commits are invisible and pre-pin commits visible) / `create_snapshot_at(name,
vt, tt, description)` (explicit/backdated, not extent-checked) /
`snapshot(name) -> Snapshot` / `get_snapshot` / `list_snapshots` (stable order:
created_at, then name) / `delete_snapshot`. The `Snapshot<'_>` handle pins
`get_node`/`get_edge`/`find_nodes`/`find_nodes_by_property`, adjacency
(`get_outgoing_edges`/`get_incoming_edges`), and a pre-pinned `query()` builder
(traversal at the pin). Errors reuse the #3234 codes (dup name → `CONFLICT`,
missing → `NOT_FOUND` with the name). Durably persisted (atomic
temp+rename+fsync, coordinates as the **full HLC** `{wallclock, logical}` so a
same-microsecond supersession pin resolves correctly after restart; sidecar
`version: 2`, a legacy `version: 1` bare-i64 file still loads as logical 0)
**inside** the persistence dir at `{persistence.data_dir}/snapshots.json`
(`{data_dir}/indexes/snapshots.json` under the durable config) when index
persistence is enabled — survives restart; in-memory-only for ephemeral
`AletheiaDB::new()`. A corrupt/unparseable sidecar does **not** brick startup
(unlike the auth key store): it is quarantined aside (`*.corrupt`) and startup
proceeds with an empty registry. Caveats mirror `temporal_extent` (#3238) /
point-in-time reads: cold-tier/truncation eviction can make a pinned version
unreadable, and pinning "now" excludes future-valid facts. MCP exposure and an
`AS OF SNAPSHOT <name>` query DDL are a coordinated follow-up (this wave is
Rust-API-only). See [docs/guides/snapshot-pin.md](docs/guides/snapshot-pin.md).

### Feature Flags: Stable vs Experimental

Semantic features are split between a stable cohort and four experimental
("Nova") cohorts. Pick a category flag rather than the umbrella when you only
need one slice.

| Flag | Status | Cohort |
|------|--------|--------|
| `semantic-search` | **Stable** | Retrieval, matching, clustering, traversal, entity resolution (Fishing, Gestalt, Cartographer, Highlander, Janus, Chameleon, Semantic Navigator, Concept Algebra, Serendipity, Voyager, Spectre, Telepathy, Tapestry, Horizon) |
| `semantic-reasoning` | Experimental | Prediction & synthesis (Prophet, Dreamer, Omen, Oracle, Hindsight, Muse, Luna, Metaphor, Synergy, Chimera, Alchemy) |
| `semantic-temporal` | Experimental | Bi-temporal + semantic (Sherlock, Chronos, Echo, Kairos, Temporal Narrative, Temporal Diff, Aura, Mnemosyne, Ariadne) |
| `semantic-diagnostics` | Experimental | Anomaly & validation (Dissonance, Sentinel, Fossil, Tremor, Polygraph, Wormhole, Ripple, Entanglement, Thermos) |
| `semantic-characterization` | Experimental | Concept characterization + export (Archetype, Prism, Gravity, Sybil, Synapse, Kaleidoscope, Papyrus, GraphContext) |
| `nova` | Umbrella | Enables every `semantic-*` cohort still in R&D (does **not** include `semantic-search`) |

Verify each flag still compiles standalone with `just check-features`.

**See [docs/adr/0050-experimental-feature-categorization.md](docs/adr/0050-experimental-feature-categorization.md) for the categorization rationale and graduation pattern.**

## Configuration

AletheiaDB uses a unified configuration system for WAL, historical storage, vector indexes, and persistence.

**Quick Start:**
```rust
use aletheiadb::{AletheiaDB, config::AletheiaDBConfig};

// Default configuration
let db = AletheiaDB::new();

// Load from TOML file
let config = AletheiaDBConfig::from_toml_file("config/production.toml")?;
let db = AletheiaDB::with_unified_config(config);

// Programmatic configuration
let config = AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .num_stripes(32).unwrap()
        .durability_mode(DurabilityMode::group_commit_default())
        .build())
    .build();
```

**See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) for all configuration options and presets.**

## Persistence Quickstart

AletheiaDB provides **two persistence systems** (cold storage requires manual setup):

| System | Purpose | Setup |
|--------|---------|-------|
| **WAL** | Transaction durability | ✅ Via `open()` |
| **Index Persistence** | Fast restarts (6-30x) | ✅ Via `open()` |
| **Cold Storage (Redb)** | Unlimited history | ⚙️ Manual (see guide) |

### Quick Setup (WAL + Index Persistence)

The one-line entry point for a durable database is `AletheiaDB::open(path)`:

```rust
use aletheiadb::AletheiaDB;

let db_path = std::env::current_dir()?.join(".my-app-data");

// ✅ Creates directories automatically! Idempotent across restarts.
let db = AletheiaDB::open(&db_path)?;
```

`open(path)` is the durable counterpart to `AletheiaDB::new()` (which is
ephemeral/tempdir-backed). It is exactly
`with_unified_config(durable_config_for_data_dir(path))` under the hood —
WAL + index persistence with `load_on_startup`, group-commit durability —
so power users who need to tune those settings can still call
`with_unified_config` directly with a custom config:

```rust
use aletheiadb::{AletheiaDB, AletheiaDBConfig};
use aletheiadb::config::WalConfigBuilder;
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::storage::wal::DurabilityMode;

let db_path = std::env::current_dir()?.join(".my-app-data");

let config = AletheiaDBConfig::builder()
    // 1. WAL for crash recovery
    .wal(WalConfigBuilder::new()
        .wal_dir(db_path.join("wal"))
        .durability_mode(DurabilityMode::GroupCommit {
            max_delay_ms: 10,
            max_batch_size: 200,
        })
        .build())
    // 2. Index persistence for fast restarts
    .persistence(PersistenceConfig {
        enabled: true,
        data_dir: db_path.join("indexes"),
        load_on_startup: true,
        ..Default::default()
    })
    .build();

// ✅ Creates directories automatically!
let db = AletheiaDB::with_unified_config(config)?;
```

**File Structure:**
```
.my-app-data/
├── wal/                # WAL (transaction durability)
└── indexes/            # Index persistence (fast restarts)
```

### Adding Cold Storage (Optional)

For unlimited bi-temporal history, set up cold storage manually:

```rust
use aletheiadb::storage::tiered::{TieredStorage, TieredStorageConfig};
use aletheiadb::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
use std::sync::Arc;

// After creating database, set up cold storage
let cold = Arc::new(RedbColdStorage::new(
    &db_path.join("cold.redb"),
    RedbConfig::new()
)?);

let tiered = Arc::new(TieredStorage::new(
    TieredStorageConfig::default(),
    cold
));

// Manually set on historical storage (requires db internals access)
// See docs/guides/tiered-storage-guide.md for complete setup
```

**See:**
- **[docs/guides/tiered-storage-guide.md](docs/guides/tiered-storage-guide.md)** - Cold storage setup
- **[examples/file_based_persistence.rs](examples/file_based_persistence.rs)** - Working example

## Development Workflow

### ⚠️ MANDATORY: Pre-Commit Quality Checks

**BEFORE EVERY COMMIT, you MUST run these commands in order:**

```bash
# 1. Run clippy with ALL warnings as errors
cargo clippy --all-targets --all-features -- -D warnings

# 2. Format all code
cargo fmt --all

# 3. Verify tests pass
cargo test
```

**These checks are NON-NEGOTIABLE:**
- `cargo clippy` ensures code quality and catches potential bugs
- `cargo fmt` maintains consistent code style
- Both MUST pass before committing

**Quick command:** `just pre-commit` runs all checks.

### Worktree-First Development

**When starting ANY implementation task, you MUST:**

1. Create a worktree first: `just worktree-new feature/descriptive-name`
2. Navigate to worktree: `cd agents/feature-descriptive-name`
3. Work, commit, and create PR: `just worktree-pr "Title" "Description"`

This enables multiple Claude instances to work in parallel without conflicts.

**Skip worktree creation only if:**
- You're already in a worktree (check with `git worktree list`)
- The task is read-only (exploration, answering questions)
- The user explicitly asks you to work in the main repo

### Feature Development Process

1. **Design First**: Document design in issue/PR description
2. **API Before Implementation**: Define public API surface
3. **Test-Driven**: Write tests before implementation
4. **Benchmark**: Add benchmarks for performance-critical code
5. **Document**: Update docs if architecture changes

**See [docs/DEVELOPMENT_WORKFLOW.md](docs/DEVELOPMENT_WORKFLOW.md) for complete workflow documentation.**

### Code Review Checklist

- [ ] **Clippy passes**: `cargo clippy --all-targets --all-features -- -D warnings` with no errors
- [ ] **Code formatted**: `cargo fmt --all` applied
- [ ] **Tests pass**: All tests passing
- [ ] **Coverage maintained**: Meets coverage thresholds
- [ ] **Miri passes** (if unsafe code changed): `just miri-test <affected_module>`
- [ ] Temporal invariants preserved
- [ ] No performance regression on benchmarks
- [ ] Error handling is comprehensive (no unwrap/expect)
- [ ] Tests cover edge cases
- [ ] Documentation updated
- [ ] No unsafe without safety comments (SAFETY: comments required)
- [ ] Strong typing used (no raw primitives for IDs)
- [ ] Code follows [CODING_STANDARDS.md](docs/CODING_STANDARDS.md)

## Development Tools

All common tasks via `just`:

```bash
just test              # Run tests
just coverage          # Generate coverage report
just lint              # Run clippy
just fmt               # Format code
just pre-commit        # Quick pre-commit checks
just check-all         # Full quality check
just bench             # Run benchmarks
just doc               # Generate docs
just worktree-new      # Create worktree
just worktree-pr       # Create PR from worktree
```

See `justfile` for complete list of commands.

## Profiling and Performance

### Tracy Profiler

```bash
# 1. Download Tracy from releases
# 2. Build with profiling
cargo build --release --features tracy
# 3. Run profiled build
just profile-tracy
```

**Instrumenting code:**
```rust
#[cfg(feature = "tracy")]
use tracy_client::span;

pub fn hot_path_function() {
    #[cfg(feature = "tracy")]
    let _span = span!("hot_path_function");
    // Function body
}
```

## LLM Integration

AletheiaDB is designed for LLM integration with temporal query patterns:

**Natural Language-Like Queries:**
```rust
db.as_of("2024-01-15T10:00:00Z").find_node("Person", "name" == "Alice")
db.between("2024-01-01", "2024-12-31").track_changes(node_id)
```

**Query Patterns:**
- "What did we know about X at time T?" → `db.as_of(T).get(X)`
- "How has Y changed?" → `db.history(Y).changes()`
- "When did we first record F?" → `db.first_occurrence(F)`

**Integration Methods:**
1. Direct Rust API (for embedded use)
2. MCP Server (for Claude integration)
3. REST/GraphQL API (for general LLM tool use)

**See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for complete LLM integration patterns.**

## Known Limitations

### Orphaned Edges on Node Deletion

The **low-level Rust** `delete_node` (`src/storage/current/mod.rs`) removes only the
node itself -- any edges where the deleted node is the source or target **and that
existed at the deleting transaction's snapshot** remain in storage as **orphaned
edges**. This edge-preserving behavior is intentional and is retained for
audit/history use cases where callers manage edge cleanup themselves or need to
preserve edge records. Traversals that follow these orphaned edges may encounter
missing endpoints.

**Concurrent-write exception (Issue #3416)**: the orphan-preserving behavior applies
only to *pre-existing* edges. Under snapshot isolation, a `delete_node`/`retract_node`
(and symmetrically a `create_edge`) that would orphan an edge **committed by a
concurrent transaction after the deleter's snapshot** now **aborts at commit** with
`TransactionError::ValidationFailed` (MCP `FAILED_PRECONDITION`, non-retriable),
first-committer-wins. The check is symmetric under the commit-serialization
(`historical` write) guard: whichever of the concurrent delete-node / create-edge
pair applies second aborts, so neither ordering can commit a new dangling edge. A
single transaction that both creates an edge and deletes its endpoint is unaffected
(that is the caller's own buffered decision), as are pre-existing edges deleted with
no concurrent writer.

**Recommended (Rust API)**: Use `delete_node_cascade` instead, which atomically
deletes the node and all connected edges, preventing orphans. To decide before acting,
call `db.count_connected_edges(node_id)` to learn how many edges reference a node
(DISTINCT edges -- a self-loop counts once, Issue #3416).

**MCP surface is safe-by-default (Issue #3209)**: The MCP `delete_node` tool mirrors
Cypher's `DETACH DELETE` contract -- it never silently orphans edges:

- If the node has connected edges and `detach` is not `true`, the deletion is
  **refused** and the JSON response reports `connected_edges` (the number of edges
  that would be orphaned), so an LLM/caller can decide.
- Passing `detach: true` performs a cascade-equivalent delete and reports
  `edges_removed`.
- A node with no connected edges deletes cleanly (`edges_removed: 0`).

This guarantees zero silent orphan-creating successes through the MCP surface: an LLM
never receives a `success` response that breaks referential integrity.

Valid-time retraction follows the same contract: `retract_node` (MCP and Rust API,
Issue #3230) refuses with `connected_edges` (distinct edges; a self-loop counts once)
unless `detach: true` / `retract_node_detach` co-retracts the connected edges.

## Future Considerations

### Vector Search (SUPERRAG) - Remaining Phases

**Status**: Phases 1-4 complete (storage, indexing, temporal, hybrid queries), Phase 5 pending

**Phase 5 will add:**
- Streaming temporal queries
- Incremental index updates
- Advanced optimization techniques

**See [docs/VECTOR_SEARCH_DESIGN.md](docs/VECTOR_SEARCH_DESIGN.md) for complete roadmap.**

### Scalability

- ✅ Sharding for horizontal scale (implemented)
- ✅ Distributed transaction coordination with 2PC (implemented)
- Replication for high availability (planned)

### Query Language

- ✅ Cypher-like temporal extensions (implemented)
- SQL:2011 temporal syntax (planned)
- ✅ Time-aware pattern matching (implemented)

### Advanced Features

- Temporal graph algorithms (shortest path over time)
- Streaming temporal queries
- Incremental materialized views
- LLM-assisted query generation

## Quick Reference Documentation

### Core Documentation
- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** - Architecture principles, design patterns, system design
- **[docs/CONFIGURATION.md](docs/CONFIGURATION.md)** - All configuration options, presets, tuning
- **[docs/DEVELOPMENT_WORKFLOW.md](docs/DEVELOPMENT_WORKFLOW.md)** - Complete development workflow
- **[docs/CODING_STANDARDS.md](docs/CODING_STANDARDS.md)** - Rust coding standards
- **[TESTING.md](TESTING.md)** - Testing requirements and coverage
- **[docs/MIRI.md](docs/MIRI.md)** - Undefined behavior detection for unsafe code

### Language Bindings
- **[python/README.md](python/README.md)** - Python SDK (PyO3 bindings, `pip install aletheiadb`)

### Feature Documentation
- **[docs/WAL.md](docs/WAL.md)** - Write-ahead log internals
- **[docs/VECTOR_SEARCH_DESIGN.md](docs/VECTOR_SEARCH_DESIGN.md)** - Vector search architecture and roadmap
- **[docs/EMBEDDINGS.md](docs/EMBEDDINGS.md)** - Embedding generation guide
- **[docs/query-language-design.md](docs/query-language-design.md)** - Query language grammar and semantics
- **[docs/guides/derivation-lineage.md](docs/guides/derivation-lineage.md)** - Derivation lineage between facts (Issue #3371)
- **[docs/guides/provenance-hash-chain.md](docs/guides/provenance-hash-chain.md)** - Tamper-evident provenance hash chain: `aletheia verify`, `verify_chain`/`export_chain_head` MCP tools, external anchoring (Issue #3351)

### User Guides
- **[docs/guides/vector-search-integration.md](docs/guides/vector-search-integration.md)** - Complete vector search API
- **[docs/guides/vector-search-performance.md](docs/guides/vector-search-performance.md)** - Performance tuning
- **[docs/guides/hybrid-query-guide.md](docs/guides/hybrid-query-guide.md)** - Hybrid query API reference
- **[docs/guides/index-persistence-guide.md](docs/guides/index-persistence-guide.md)** - Index persistence details
- **[docs/guides/tiered-storage-guide.md](docs/guides/tiered-storage-guide.md)** - Tiered storage configuration and usage
- **[docs/guides/sharding-guide.md](docs/guides/sharding-guide.md)** - Graph sharding and distributed deployment
- **[docs/guides/query-pipeline-guide.md](docs/guides/query-pipeline-guide.md)** - Query execution pipeline
- **[docs/guides/security-quickstart.md](docs/guides/security-quickstart.md)** - Authentication, RBAC roles, API-key lifecycle
- **[docs/guides/access-control-matrix.md](docs/guides/access-control-matrix.md)** - Canonical role/operation authorization matrix
- **[docs/guides/derivation-lineage.md](docs/guides/derivation-lineage.md)** - Fact-to-fact derivation lineage: version-pinned upstream/downstream closures (Issue #3371)

### Architecture Decision Records (ADRs)
See `docs/adr/` for all architectural decisions.

## References

- [AeonG: Efficient Temporal Graph Database](https://arxiv.org/abs/2304.12212)
- [XTDB Bi-temporality](https://v1-docs.xtdb.com/concepts/bitemporality/)
- [Temporal Database Concepts](https://en.wikipedia.org/wiki/Temporal_database)
- [Rust Performance Book](https://nnethercote.github.io/perf-book/)
