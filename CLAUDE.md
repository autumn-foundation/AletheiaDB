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

## Architecture Principles

### 1. Performance First

**Current-State Queries Must Be Fast:**
- Current state stored separately from historical data (hybrid storage architecture)
- Zero abstraction overhead for non-temporal queries
- CSR (Compressed Sparse Row) adjacency representation for cache-friendly traversals
- **Target**: <1µs single-hop traversal, <100µs for 3-hop traversal

**Temporal Queries Must Be Efficient:**
- Anchor+delta compression reduces storage 5-6X
- Temporal B-Tree indexes for range queries
- Anchor-based reconstruction skips unnecessary versions
- **Target**: <10ms for point-in-time reconstruction

### 2. Storage Efficiency

**Compression Strategy:**
- Create anchor (full snapshot) every 10 versions (configurable)
- Delta encoding for incremental changes
- Copy-on-write with `Arc<T>` for property deduplication
- String interning for labels and property keys
- **Target**: <2X overhead vs non-temporal storage

**Immutable History:**
- Historical versions are immutable after creation
- Enables aggressive caching and compression
- Safe for concurrent access without locks

### 3. Correctness Guarantees

**Temporal Consistency:**
- Transaction time is monotonically increasing
- Valid time can be retroactive but must be consistent
- No temporal paradoxes (e.g., deleting an entity before it was created)

**ACID Properties:**
- **Atomicity**: WAL ensures atomic commits
- **Consistency**: Invariants checked on write
- **Isolation**: MVCC provides snapshot isolation
- **Durability**: WAL + fsync guarantees

## Design Patterns

### Hybrid Storage Architecture

```
┌─────────────────────────────────────────────────────┐
│              Query Engine                            │
│  - Temporal Query Planner                           │
│  - Graph Traversal Engine                           │
└─────────────────────────────────────────────────────┘
                        │
        ┌───────────────┴───────────────┐
        │                               │
┌───────▼─────────┐          ┌─────────▼─────────┐
│ Current Storage │          │ Historical Storage │
│  (Fast Path)    │          │  (Temporal Path)  │
│                 │          │                   │
│ - Live graph    │          │ - Version chains  │
│ - Hot indexes   │          │ - Anchor+delta    │
│ - No temporal   │          │ - Compressed      │
└─────────────────┘          └───────────────────┘
```

**When to Use Each:**
- **Current**: All non-temporal queries, latest state access
- **Historical**: Time-travel, audit trails, temporal analysis, LLM reasoning

### Temporal Query Processing

**Query Types:**

1. **Time Point Query** (as of timestamp T): Lookup in temporal index → Find nearest anchor ≤ T → Apply deltas → Return state
2. **Time Range Query** (between T1 and T2): Range scan temporal index → Reconstruct each version → Stream results
3. **Knowledge Evolution Query** (for LLMs): Track how entity changed over time → Provenance and sources → Identify when understanding shifted

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

GallifreyDB enforces strict code coverage thresholds:
- **Minimum 85% line coverage** (current: 86.45%)
- **Minimum 88% function coverage** (current: 89.10%)
- **Minimum 88% region coverage** (current: 88.91%)

### Quick Commands

```bash
just test              # Run all tests
just coverage          # Generate HTML coverage report
just coverage-check    # Verify coverage meets thresholds
just bench             # Run benchmarks
just check-all         # Full quality check (tests, coverage, lint)
```

### Test Types Required

1. **Unit Tests**: Test each module in isolation
2. **Integration Tests**: End-to-end workflows
3. **Property-Based Tests**: Use `proptest` for temporal invariants
4. **Performance Benchmarks**: Required for critical paths (see below)

### Performance Benchmarks

**Required Benchmarks:**
1. Current-state single-hop traversal (<1µs)
2. Current-state 3-hop traversal (<100µs)
3. Time-travel reconstruction (<10ms)
4. Batch insertion throughput (>100k edges/sec)
5. Storage overhead (<2X vs non-temporal)

## WAL Format and Migration

**See [docs/WAL.md](docs/WAL.md) for comprehensive WAL documentation.**

### Quick Reference

**Current Version**: 2 (binary format with "GWAL" magic bytes)

**Key Features**:
- Version-aware format with automatic detection
- Full property and temporal interval serialization
- Checksum verification for data integrity
- Backward compatible with V1 (with data loss warnings)

**Migration**:
```rust
use gallifreydb::storage::wal::migrate_wal_directory;

// Migrate all segments (creates .bak backups)
let results = migrate_wal_directory(Path::new("data/wal/"))?;
```

**Adding New Versions**: See [docs/WAL.md](docs/WAL.md#adding-new-wal-versions) for the 5-step process.

## Vector Storage & Indexing

GallifreyDB supports dense vector embeddings as first-class properties with HNSW k-NN search for semantic similarity, enabling RAG workflows and LLM integration with full bi-temporal versioning.

### Quick Start

```rust
use gallifreydb::{GallifreyDB, PropertyMapBuilder};
use gallifreydb::index::vector::{HnswConfig, DistanceMetric};

let db = GallifreyDB::new();

// Enable vector indexing
let config = HnswConfig::new(384, DistanceMetric::Cosine);
db.enable_vector_index("embedding", config)?;

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

### Key Features

| Feature | Details |
|---------|---------|
| **Storage** | Vectors as `Arc<[f32]>`, efficient cloning |
| **Dimensions** | Up to 100,000 (common: 384, 768, 1536, 3072) |
| **Distance Metrics** | Cosine, Euclidean, DotProduct |
| **Indexing** | HNSW for sub-linear k-NN search |
| **Temporal** | Full versioning - track embedding evolution |
| **Auto-indexing** | Automatic on create/update with rollback |

### Similarity Functions

```rust
use gallifreydb::core::vector::{cosine_similarity, euclidean_distance, dot_product};

// Choose the right metric for your use case
let sim = cosine_similarity(&a, &b)?;      // Semantic similarity
let dist = euclidean_distance(&a, &b)?;    // Spatial clustering
let dot = dot_product(&a, &b)?;            // MaxSim, ColBERT
```

### Comprehensive Documentation

- **[Integration Guide](docs/guides/vector-search-integration.md)** - Complete API reference and examples
- **[Performance Guide](docs/guides/vector-search-performance.md)** - Tuning HNSW parameters
- **[Troubleshooting Guide](docs/guides/vector-search-troubleshooting.md)** - Common issues and solutions
- **[Design Document](docs/VECTOR_SEARCH_DESIGN.md)** - Architecture and roadmap (Phases 3-5)

### Temporal Vector Integration (VS-047)

GallifreyDB integrates temporal vector indexes with historical storage using a **hybrid pre-anchor hooks + post-commit observers** pattern. This enables provenance tracking between graph data anchors and vector snapshots.

#### Key Features

| Feature | Details |
|---------|---------|
| **Strong Consistency** | Snapshot IDs stored atomically with anchors - no consistency window |
| **Provenance Tracking** | Direct linkage from graph anchor → vector snapshot |
| **Graceful Degradation** | Hook failures don't block anchor creation |
| **Observer Extensibility** | Post-commit notifications for metrics, logging |

#### Quick Start

```rust
use gallifreydb::index::vector::temporal::TemporalVectorConfig;

let db = GallifreyDB::with_config(AnchorConfig {
    anchor_interval: 10,  // Create anchor every 10 versions
    max_delta_chain: 10,
});

// Enable temporal vector indexing (registers hooks + observers)
let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);
db.enable_temporal_vector_index("embedding", temporal_config)?;

// Now graph anchors automatically trigger vector snapshots
let node_id = db.create_node("Document",
    PropertyMapBuilder::new()
        .insert_vector("embedding", &embedding)
        .build()
)?;

// Update multiple times - snapshot created when anchor triggered
for i in 0..20 {
    db.update_node(node_id,
        PropertyMapBuilder::new()
            .insert_vector("embedding", &updated_embedding)
            .build()
    )?;
}
// Anchors at v0, v10, v20 each have vector snapshot IDs
```

#### Architecture: Hooks vs Observers

**Pre-Anchor Hooks** (strong consistency):
- Fire **BEFORE** anchor storage
- Return `Option<snapshot_id>` to be stored atomically
- Enable provenance: `anchor.vector_snapshot_id → temporal_index.snapshot(id)`
- Use case: Snapshot ID provenance tracking

**Post-Commit Observers** (extensibility):
- Fire **AFTER** anchor storage
- Notify of events for metrics, logging
- Don't block storage operations
- Use case: Observability, notifications, future indexes

See **[ADR-0018](docs/adr/0018-temporal-vector-historical-integration.md)** for complete architecture and design decisions.

## Embedding Generation (Optional)

GallifreyDB provides **optional** embedding providers via feature flags. Embedding generation is separate from the database - generate embeddings first, then store them.

### Quick Start

```rust
use gallifreydb::embeddings::{EmbeddingService, providers::openai::*};

// 1. Generate embedding
let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;
let service = EmbeddingService::new(Arc::new(OpenAIProvider::new(config)?));
let embedding = service.embed(text).await?;

// 2. Store in database
let node_id = db.create_node("Document",
    PropertyMapBuilder::new()
        .insert_vector("embedding", &embedding)
        .build()
)?;
```

### Available Providers

| Provider | Type | Latency | Cost | Privacy | Feature Flag |
|----------|------|---------|------|---------|--------------|
| OpenAI | API | ~100-200ms | $$$ | ❌ Cloud | `embedding-openai` |
| HuggingFace | API | ~200-500ms | $ | ❌ Cloud | `embedding-huggingface` |
| Ollama | Local | ~20-50ms | Free | ✅ Local | `embedding-ollama` |
| ONNX | Local | ~1-10ms* | Free | ✅ Local | `embedding-onnx` |

*ONNX is currently a placeholder

### Why Separate?

- **Zero Coupling**: DB layer stays lightweight, focused on storage/indexing
- **Flexibility**: Easy to swap providers without changing DB code
- **Optional**: Zero runtime overhead when features disabled

### Documentation

- **[docs/EMBEDDINGS.md](docs/EMBEDDINGS.md)** - Comprehensive user guide with examples
- **[docs/adr/0016-embedding-providers.md](docs/adr/0016-embedding-providers.md)** - Architecture decision record

## LLM Integration Patterns

### Temporal Query API for LLMs

**Natural Language-Like Queries:**
```rust
db.as_of("2024-01-15T10:00:00Z").find_node("Person", "name" == "Alice").get_relationships("KNOWS")
db.between("2024-01-01", "2024-12-31").track_changes(node_id).with_provenance()
```

**Query Patterns LLMs Can Use:**
- "What did we know about X at time T?" → `db.as_of(T).get(X)`
- "How has Y changed?" → `db.history(Y).changes()`
- "When did we first record F?" → `db.first_occurrence(F)`
- "Show changes to E between T1 and T2" → `db.between(T1, T2).track_changes(E)`

### Integration Methods

1. **Direct Rust API** (for embedded use)
2. **MCP Server** (for Claude integration)
3. **REST/GraphQL API** (for general LLM tool use)
4. **Natural Query Language** (intuitive for LLMs to generate)

## Development Workflow

### IMPORTANT: Worktree-First Development

**When starting ANY implementation task, Claude instances MUST:**

1. **Create a worktree first** before making any code changes:
   ```bash
   just worktree-new feature/descriptive-name   # For new features
   just worktree-new fix/descriptive-name       # For bug fixes
   ```

2. **Navigate to the worktree** and work there:
   ```bash
   cd agents/feature-descriptive-name
   ```

3. **After completing work**, commit, create PR, and clean up:
   ```bash
   git add . && git commit -m "feat: description"
   just worktree-pr "PR Title" "Description"
   # After merge: just worktree-remove feature/descriptive-name
   ```

This enables multiple Claude instances to work in parallel without conflicts. Each instance gets an isolated copy of the codebase.

**Skip worktree creation only if:**
- You're already in a worktree (check with `git worktree list`)
- The task is read-only (exploration, answering questions)
- The user explicitly asks you to work in the main repo

See `WORKTREE_WORKFLOW.md` for complete documentation.

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

**Recommended workflow:**
```bash
# Make code changes
# ...

# Run quality checks
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all

# If clippy or fmt made changes, review them
git diff

# Run tests
cargo test

# Only then commit
git add .
git commit -m "feat: your change"
```

**Note:** The `just pre-commit` command includes these checks, but you should run them explicitly to see any issues immediately.

### Feature Development Process

1. **Design First**: Document design in issue/PR description
2. **API Before Implementation**: Define public API surface
3. **Test-Driven**: Write tests before implementation
4. **Benchmark**: Add benchmarks for performance-critical code
5. **Document**: Update docs if architecture changes

### Code Review Checklist

- [ ] **Clippy passes**: `cargo clippy --all-targets --all-features -- -D warnings` with no errors
- [ ] **Code formatted**: `cargo fmt --all` applied
- [ ] **Tests pass**: All tests passing
- [ ] Temporal invariants preserved
- [ ] No performance regression on benchmarks
- [ ] Error handling is comprehensive (no unwrap/expect)
- [ ] Tests cover edge cases
- [ ] Documentation updated
- [ ] No unsafe without safety comments
- [ ] Strong typing used (no raw primitives for IDs)
- [ ] Code follows [CODING_STANDARDS.md](docs/CODING_STANDARDS.md)

## Profiling and Performance Tools

### Tracy Profiler via Observability Framework

GallifreyDB uses the observability framework with Tracy integration for CPU profiling. Tracing spans automatically map to Tracy zones for flame graph analysis. See **[docs/PROFILING.md](docs/PROFILING.md)** for comprehensive profiling guide.

**Quick start:**
```bash
# Terminal 1: Start Tracy GUI
./tracy-profiler

# Terminal 2: Run profiling benchmark with observability-tracy
cargo bench --bench profiling_commit --features observability-tracy
```

**Architecture**:
- Instrumentation: `tracing` spans (single layer)
- Backend: `observability-tracy` bridges to Tracy profiler
- Benefit: Single instrumentation serves multiple backends (logs, Tracy, Honeycomb)

**Known bottlenecks** (as of performance investigation):
- Lock contention: ~10-15% of transaction time (timestamp + WAL locks)
- Graph operations (apply_changes): ~85-90% of time
- Target: Use Tracy to break down apply_changes into specific operations

**Key instrumentation points:**
- Transaction commit critical path (commit_critical_section, apply_changes)
- Historical storage version chain operations (add_node_version, add_edge_version)
- Current storage graph mutations (insert_node, insert_edge)
- Adjacency index rebuilds

**Profiling scenarios:**
- `sequential` - Baseline single-threaded performance
- `concurrent` - Exposes lock contention under load
- `heavy` - Stresses apply_changes with large transactions
- `mixed` - Realistic mixed workload

See **[docs/PROFILING.md](docs/PROFILING.md)** for detailed workflow, Tracy span hierarchy, analysis guide, and troubleshooting.

### Development Tools

All common tasks via `just`:
- `just test` - Run tests
- `just coverage` - Generate coverage report
- `just lint` - Run clippy
- `just fmt` - Format code
- `just pre-commit` - Quick pre-commit checks
- `just check-all` - Full quality check
- `just profile-commit` - Run Tracy profiling benchmark

See `justfile` for complete list of commands.

## Future Considerations

### Vector Search (SUPERRAG) - Remaining Phases

**Status**: Phases 1-2 complete (storage + HNSW indexing), Phases 3-5 pending

**Remaining phases will add:**
- **Phase 3**: Temporal vector queries (semantic time-travel)
- **Phase 4**: Hybrid graph+vector queries
- **Phase 5**: Advanced features (streaming, incremental updates)

**Key query patterns Phase 3+ will enable:**
```rust
// Semantic time-travel
db.as_of(timestamp_2023).find_similar(embedding, k)

// Graph + Vector: traverse then rank
db.traverse(alice_id, "KNOWS").rank_by_similarity(bob_embedding, 10)

// Knowledge evolution
db.track_semantic_drift(node_id, time_range)
```

See **[docs/VECTOR_SEARCH_DESIGN.md](docs/VECTOR_SEARCH_DESIGN.md)** for complete roadmap.

### Scalability

- Sharding for horizontal scale
- Distributed transaction coordination
- Replication for high availability

### Query Language

- Cypher-like temporal extensions
- SQL:2011 temporal syntax
- Time-aware pattern matching

### Advanced Features

- Temporal graph algorithms (shortest path over time)
- Streaming temporal queries
- Incremental materialized views
- LLM-assisted query generation

## References

- [AeonG: Efficient Temporal Graph Database](https://arxiv.org/abs/2304.12212)
- [XTDB Bi-temporality](https://v1-docs.xtdb.com/concepts/bitemporality/)
- [Temporal Database Concepts](https://en.wikipedia.org/wiki/Temporal_database)
- [Rust Performance Book](https://nnethercote.github.io/perf-book/)
