# GallifreyDB Architecture & Development Guidelines

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

GallifreyDB is a high-performance bi-temporal graph database written in Rust. It tracks both **valid time** (when facts were true in reality) and **transaction time** (when facts were recorded in the database), while maintaining performance comparable to regular graph databases for current-state queries.

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

### Index Persistence

Fast cold starts by loading indexes from disk instead of WAL replay.

**Key Features:**
- 6-30x faster startup (2-5s vs 30-60s for 1M nodes)
- Zstd compression (60-75% size reduction)
- Memory-mapped loading for multi-GB indexes
- Parallel loading (graph + temporal + vector concurrently)

**Quick Start:**
```rust
use gallifreydb::{GallifreyDB, config::GallifreyDBConfig};
use gallifreydb::storage::index_persistence::PersistenceConfig;

let config = GallifreyDBConfig::builder()
    .persistence(PersistenceConfig {
        enabled: true,
        data_dir: "data/my-database".into(),
        load_on_startup: true,
        ..Default::default()
    })
    .build();

let db = GallifreyDB::with_unified_config(config);
```

**See [docs/guides/index-persistence-guide.md](docs/guides/index-persistence-guide.md) for complete guide.**

### Vector Storage & Indexing

Dense vector embeddings as first-class properties with HNSW k-NN search for semantic similarity.

**Quick Start:**
```rust
use gallifreydb::{GallifreyDB, PropertyMapBuilder};
use gallifreydb::index::vector::{HnswConfig, DistanceMetric};

let db = GallifreyDB::new();

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
use gallifreydb::query::QueryBuilder;

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
use gallifreydb::storage::{
    TieredStorage, TieredStorageConfig,
    RedbColdStorage, RedbConfig,
};

// Create Redb cold storage backend
let cold = RedbColdStorage::new("data/cold.redb", RedbConfig::new())?;

// Create tiered storage
let tiered = TieredStorage::new(TieredStorageConfig::default(), Arc::new(cold));

// Configure historical storage to use tiered access
historical.set_tiered_storage(Arc::new(tiered));
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

### MCP Server (AI Agent Integration)

Model Context Protocol server enabling LLMs to interact with GallifreyDB.

**Quick Start:**
```bash
# Run the MCP server (communicates over stdio)
cargo run --bin gallifrey-mcp --features mcp-server
```

**Available Tools:**
| Category | Tools |
|----------|-------|
| **Nodes** | `get_node`, `create_node`, `update_node`, `delete_node`, `delete_node_cascade`, `list_nodes`, `count_nodes` |
| **Edges** | `get_edge`, `create_edge`, `update_edge`, `delete_edge`, `get_outgoing_edges`, `get_incoming_edges` |
| **Traversal** | `traverse` (multi-hop graph traversal) |
| **Vector** | `find_similar`, `enable_vector_index`, `list_vector_indexes` |
| **Temporal** | `get_node_at_time`, `get_edge_at_time` |
| **Hybrid** | `hybrid_query` (combined graph + vector + temporal) |

**Programmatic Usage:**
```rust
use gallifreydb::mcp::GallifreyMcpServer;
use gallifreydb::GallifreyDB;
use std::sync::Arc;

let db = Arc::new(GallifreyDB::new()?);
let server = GallifreyMcpServer::new(db);
server.serve_stdio().await?;
```

### Query Language (GQL)

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
use gallifreydb::storage::sharding::{
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

## Configuration

GallifreyDB uses a unified configuration system for WAL, historical storage, vector indexes, and persistence.

**Quick Start:**
```rust
use gallifreydb::{GallifreyDB, config::GallifreyDBConfig};

// Default configuration
let db = GallifreyDB::new();

// Load from TOML file
let config = GallifreyDBConfig::from_toml_file("config/production.toml")?;
let db = GallifreyDB::with_unified_config(config);

// Programmatic configuration
let config = GallifreyDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .num_stripes(32).unwrap()
        .durability_mode(DurabilityMode::group_commit_default())
        .build())
    .build();
```

**See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) for all configuration options and presets.**

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

This enables multiple AI agent instances to work in parallel without conflicts.

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
- [ ] Temporal invariants preserved
- [ ] No performance regression on benchmarks
- [ ] Error handling is comprehensive (no unwrap/expect)
- [ ] Tests cover edge cases
- [ ] Documentation updated
- [ ] No unsafe without safety comments
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

GallifreyDB is designed for LLM integration with temporal query patterns:

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
2. MCP Server (for AI agent integration)
3. REST/GraphQL API (for general LLM tool use)

**See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for complete LLM integration patterns.**

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
