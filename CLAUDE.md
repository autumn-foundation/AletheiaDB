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
# Run the MCP server (communicates over stdio)
cargo run --bin aletheia-mcp --features mcp-server
```

**Available Tools:**
| Category | Tools |
|----------|-------|
| **Nodes** | `get_node`, `create_node`, `update_node`, `delete_node`, `delete_node_cascade`, `list_nodes`, `count_nodes` |
| **Edges** | `get_edge`, `create_edge`, `update_edge`, `delete_edge`, `get_outgoing_edges`, `get_incoming_edges` |
| **Traversal** | `traverse` (multi-hop graph traversal; optional bi-temporal `as_of_valid_time`/`as_of_transaction_time`) |
| **Vector** | `find_similar`, `enable_vector_index`, `list_vector_indexes` |
| **Temporal** | `get_node_at_time`, `get_edge_at_time`, `find_nodes_at_time` (point-in-time find by label/property, no NodeId needed), `temporal_extent` (dataset's queryable bi-temporal extent; optional by_label breakdown) |
| **Hybrid** | `hybrid_query` (combined graph + vector + temporal) |
| **Query** | `query` (execute a single read-only Cypher/AQL statement; see below) |
| **Schema** | `get_schema` (node labels, edge types, and property keys, each with counts; optional bi-temporal `as_of_valid_time`/`as_of_transaction_time`) |
| **Stats** | `database_stats` (holistic snapshot: current size, bi-temporal depth + anchor/delta compression, hot/warm/cold tier distribution, WAL state; no arguments) |

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
bounds folded from hot-tier history. Coverage caveat: bounds span the
current process lifetime plus hot-tier history restored at startup —
versions cold-migrated before the last restart are not reflected. See
[docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#discovering-the-queryable-temporal-extent-temporal_extent).

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
- Variable-depth: `-[:KNOWS*1..3]->`
- Directions: `->` (outgoing), `<-` (incoming), `-` (both)
- Filtering: `WHERE n.age > 18 AND n.name = 'Alice'`
- Results: `RETURN`, `RETURN DISTINCT`, `AS` aliases
- Ordering: `ORDER BY n.age DESC`
- Pagination: `SKIP 10 LIMIT 20`
- Query chaining: `WITH b WHERE b.score > 0.5 RETURN b`
- Parameters: `$paramName`

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
node itself -- any edges where the deleted node is the source or target remain in
storage as **orphaned edges**. This edge-preserving behavior is intentional and is
retained for audit/history use cases where callers manage edge cleanup themselves or
need to preserve edge records. Traversals that follow these orphaned edges may
encounter missing endpoints.

**Recommended (Rust API)**: Use `delete_node_cascade` instead, which atomically
deletes the node and all connected edges, preventing orphans. To decide before acting,
call `db.count_connected_edges(node_id)` to learn how many edges reference a node.

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

### User Guides
- **[docs/guides/vector-search-integration.md](docs/guides/vector-search-integration.md)** - Complete vector search API
- **[docs/guides/vector-search-performance.md](docs/guides/vector-search-performance.md)** - Performance tuning
- **[docs/guides/hybrid-query-guide.md](docs/guides/hybrid-query-guide.md)** - Hybrid query API reference
- **[docs/guides/index-persistence-guide.md](docs/guides/index-persistence-guide.md)** - Index persistence details
- **[docs/guides/tiered-storage-guide.md](docs/guides/tiered-storage-guide.md)** - Tiered storage configuration and usage
- **[docs/guides/sharding-guide.md](docs/guides/sharding-guide.md)** - Graph sharding and distributed deployment
- **[docs/guides/query-pipeline-guide.md](docs/guides/query-pipeline-guide.md)** - Query execution pipeline

### Architecture Decision Records (ADRs)
See `docs/adr/` for all architectural decisions.

## References

- [AeonG: Efficient Temporal Graph Database](https://arxiv.org/abs/2304.12212)
- [XTDB Bi-temporality](https://v1-docs.xtdb.com/concepts/bitemporality/)
- [Temporal Database Concepts](https://en.wikipedia.org/wiki/Temporal_database)
- [Rust Performance Book](https://nnethercote.github.io/perf-book/)
