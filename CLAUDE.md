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

## Write-Ahead Log (WAL)

**See [docs/WAL.md](docs/WAL.md) for comprehensive WAL documentation.**

### Concurrent WAL Architecture

GallifreyDB uses a **Striped Lock-Free Ring Buffers** architecture for high-throughput concurrent writes:

```
                    ┌─────────────────────┐
                    │    LSN Allocator    │
                    │  AtomicU64::fetch_add
                    └──────────┬──────────┘
                               │
       ┌───────────────────────┼───────────────────────┐
       ▼                       ▼                       ▼
┌─────────────┐         ┌─────────────┐         ┌─────────────┐
│   Stripe 0  │         │   Stripe 1  │         │  Stripe N   │
│ Ring Buffer │         │ Ring Buffer │         │ Ring Buffer │
│ (Lock-free) │         │ (Lock-free) │         │ (Lock-free) │
└──────┬──────┘         └──────┬──────┘         └──────┬──────┘
       └───────────────────────┼───────────────────────┘
                               ▼
                    ┌─────────────────────┐
                    │  Flush Coordinator  │
                    │  - Sorts by LSN     │
                    │  - Writes segment   │
                    └─────────────────────┘
```

**Key Design Principles:**
- **Lock-free append**: Multiple threads append without mutex contention
- **Global LSN ordering**: Single atomic counter ensures total ordering
- **Sorted flush**: Entries sorted by LSN before writing to disk
- **ACID preserved**: Synchronous and GroupCommit modes remain fully ACID

### Performance

| Mode | Latency | Throughput | ACID |
|------|---------|------------|------|
| Synchronous | ~1.5ms | ~600/sec | ✅ Full |
| GroupCommit | ~10-50ms | ~100K+/sec | ✅ Full |
| Async | <100ns | ~500K+/sec | ❌ Eventual |

### Quick Reference

**Format**: Binary with "GWAL" magic bytes, version 1

**Key Features**:
- Lock-free concurrent append path
- Full property and temporal interval serialization
- CRC32 checksum verification

**Documentation:**
- [ADR-0020: Concurrent WAL Architecture](docs/adr/0020-concurrent-wal-architecture.md)
- [Durability Modes](docs/architecture/durability-modes.md)

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

#### Snapshot Policies

Control when vector snapshots are created using `SnapshotStrategy`:

```rust
use gallifreydb::index::vector::temporal::{SnapshotStrategy, RetentionPolicy};

// Every N transactions (predictable overhead)
let config = TemporalVectorConfig {
    snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
    retention_policy: RetentionPolicy::KeepN(100),
    full_snapshot_interval: 10,  // Full snapshot every 10 snapshots
    ..Default::default()
};

// Time-based snapshots (e.g., hourly)
let config = TemporalVectorConfig {
    snapshot_strategy: SnapshotStrategy::TimeInterval(3600),  // seconds
    ..Default::default()
};

// Change threshold (when X% of vectors change)
let config = TemporalVectorConfig {
    snapshot_strategy: SnapshotStrategy::ChangeThreshold(0.1),  // 10% changed
    ..Default::default()
};

// Hybrid: whichever fires first
let config = TemporalVectorConfig {
    snapshot_strategy: SnapshotStrategy::Hybrid {
        transaction_interval: 100,
        time_interval_secs: 3600,
        change_threshold: 0.05,
    },
    ..Default::default()
};
```

**Snapshot Types**:
- **Full Snapshots**: Complete HNSW index, created every `full_snapshot_interval` (default: 10)
- **Delta Snapshots**: Only changed vectors since last Full snapshot
  - Reduces creation time from O(N log N) to O(M log M) where M = changes
  - Query merges delta + base with deduplication

**Retention Policies** control memory usage:
- `RetentionPolicy::KeepAll` - No pruning (unbounded growth)
- `RetentionPolicy::KeepN(100)` - Keep 100 most recent snapshots (default)
- `RetentionPolicy::KeepDuration(Duration::from_secs(86400))` - Time-based retention

#### Semantic Drift Tracking

Track how embeddings evolve over time to detect knowledge changes:

```rust
use gallifreydb::index::vector::temporal::DriftMetric;
use gallifreydb::core::temporal::TimeRange;

// Get temporal vector index
let temporal_index = db.get_temporal_vector_index("embedding")?;

// Example 1: Find all nodes with significant semantic drift
let time_range = TimeRange::new(timestamp_2023, timestamp_2024);
let drifted_nodes = temporal_index.find_semantic_drift(
    0.3,  // Threshold: cosine distance > 0.3
    time_range,
    DriftMetric::Cosine,
)?;

for (node_id, drift_score) in drifted_nodes {
    println!("Node {} drifted by {:.3}", node_id, drift_score);
}

// Example 2: Track specific node's drift over time
// Note: For Cosine distance, embeddings should be normalized (unit vectors)
let reference_embedding = vec![0.5f32; 384];  // Example only - not normalized
let drift_timeline = temporal_index.track_semantic_drift(
    node_id,
    &reference_embedding,
    time_range,
)?;

for (timestamp, distance) in drift_timeline {
    println!("At {}: drift = {:.3}", timestamp, distance);
}
```

**Drift Metrics**:
- `DriftMetric::Cosine` - Angular distance (1.0 - similarity), range [0, 2]
- `DriftMetric::Euclidean` - L2 distance between vectors
- `DriftMetric::Angular` - Geometric angle in radians

**Use Cases**:
- **Content Versioning**: Detect when document meanings diverge
- **Knowledge Evolution**: Track concept definition changes over time
- **Anomaly Detection**: Identify sudden semantic shifts
- **Contradiction Detection**: Find facts that changed meaning
- **LLM Reasoning**: Understand when/why knowledge evolved

#### Temporal Vector Queries

Perform semantic searches at any point in time:

```rust
// Point-in-time query: "What was similar in 2023?"
let query_embedding = vec![0.1f32; 384];
let results = temporal_index.find_similar_as_of(
    &query_embedding,
    10,  // k
    timestamp_2023,
)?;

// Range query: "What was similar across 2023-2024?"
let time_range = TimeRange::new(timestamp_2023, timestamp_2024);
let results = temporal_index.find_similar_in_range(
    &query_embedding,
    10,
    time_range,
)?;

// Iterate over results from each snapshot in range
for (timestamp, snapshot_results) in results {
    println!("At {}: {:?}", timestamp, snapshot_results);
}
```

#### Performance Characteristics

| Operation | Complexity | Target | Actual (1M vectors) |
|-----------|------------|--------|---------------------|
| Full snapshot creation | O(N log N) where N = vectors | <1s | ~950ms |
| Delta snapshot creation | O(M log M) where M = changes | <100ms | ~50ms (M=1000) |
| Point-in-time query | O(log N) where N = vectors | <10ms | ~4-8ms |
| Range query (K snapshots) | O(K × log N) where K = snapshots | <100ms | ~40-80ms (K=10) |
| Drift detection | O(S × N) where S = snapshots, N = vectors | <50ms | ~30ms (S=5 snapshots) |

**Memory Budget** (assuming 10:1 delta:full snapshot ratio):
- Small DB (10K vectors, 10 snapshots): ~100MB
  - 1 full + 9 deltas (~10% changes): ~25MB + ~75MB
- Medium DB (100K vectors, 50 snapshots): ~5GB
  - 5 full + 45 deltas (~10% changes): ~1.25GB + ~3.75GB
- Large DB (1M vectors, 100 snapshots): ~100GB
  - 10 full + 90 deltas (~10% changes): ~25GB + ~75GB

## Hybrid Query API

GallifreyDB provides a unified hybrid query API that combines **graph traversal**, **vector similarity**, and **bi-temporal queries** into a single fluent interface. This enables queries like "Who did Alice know in 2023 that was similar to Bob?"

### Quick Start

```rust
use gallifreydb::query::hybrid::{traverse_and_rank, find_similar_as_of};
use gallifreydb::query::QueryBuilder;
use gallifreydb::query::ir::Predicate;

// Simple: Graph + Vector hybrid
let results = traverse_and_rank(&db, alice_id, "KNOWS", &bob_embedding, 10)?;

// Simple: Temporal + Vector
let results = find_similar_as_of(&db, &query_embedding, 10, timestamp)?;

// Complex: Full hybrid with fluent builder
let results = db.query()
    .as_of(valid_time, tx_time)
    .start(alice_id)
    .traverse("KNOWS")
    .rank_by_similarity(&bob_embedding, 10)
    .filter(Predicate::gt("score", 0.8))
    .with_provenance()
    .execute(&db)?;
```

### Three-Layer API

| Layer | Use Case | Example |
|-------|----------|---------|
| **Direct Functions** | Simple patterns | `traverse_and_rank(&db, node, "KNOWS", &emb, k)` |
| **Query Builder** | Complex compositions | `db.query().start(n).traverse("X").rank_by_similarity(&e, k)` |
| **Convenience Methods** | Quick access | `db.traverse_and_rank(node, "KNOWS", &emb, k)` |

### Query Builder State Machine

The builder uses phantom types for compile-time safety:

```rust
// Valid: Source → Traverse → Rank
let q = db.query()
    .start(node_id)           // Initial → HasNodes
    .traverse("KNOWS")        // HasNodes → HasTraversalResults
    .rank_by_similarity(&e, 10)  // → HasVectorResults
    .execute(&db)?;

// Invalid: Won't compile - no source before traverse
let q = db.query().traverse("KNOWS"); // ERROR: traverse not available in Initial state
```

### Available Operations

**Source Operations** (Initial state):
- `start(NodeId)` / `start_from(Vec<NodeId>)` - Start from node(s)
- `scan(Option<&str>)` / `scan_label(&str)` - Scan nodes
- `find_similar(&[f32], k)` - Vector k-NN search

**Graph Operations** (HasNodes/HasTraversalResults):
- `traverse("LABEL")` / `traverse_all()` - Single-hop traversal
- `traverse_n("LABEL", depth)` - Multi-hop exact
- `traverse_in("LABEL")` / `traverse_both("LABEL")` - Direction variants

**Vector Operations**:
- `rank_by_similarity(&[f32], k)` - Rank results by similarity
- `similar_to(NodeId, k)` - Node-based k-NN search

**Temporal Operations** (any state):
- `as_of(valid_time, tx_time)` - Point-in-time query
- `between(start, end)` - Time range query

**Filter/Control** (any state):
- `filter(Predicate)` - Property filtering
- `with_label(&str)` - Label filtering
- `limit(n)` / `skip(n)` - Pagination
- `with_provenance()` - Include metadata
- `parallel()` - Enable parallel execution

### Predicates

```rust
Predicate::eq("name", "Alice")     // Equality
Predicate::gt("age", 18)           // Comparison
Predicate::exists("email")         // Property exists
Predicate::contains("bio", "rust") // String contains

// Combine predicates
let p = Predicate::eq("status", "active")
    .and(Predicate::gt("score", 0.5));
```

### Query Results

```rust
let results = query.execute(&db)?;
for row in results {
    let row = row?;
    match row.entity {
        EntityResult::Node(node) => println!("Node: {:?}", node),
        EntityResult::NodeId(id) => println!("ID: {:?}", id),
        _ => {}
    }
    if let Some(score) = row.score {
        println!("  Similarity: {:.3}", score);
    }
}
```

### Performance Targets

| Query Type | Target Latency |
|------------|----------------|
| Single node lookup | <1µs |
| 3-hop traversal | <100µs |
| k-NN search (k=10, 1M vectors) | <10ms |
| Graph+Vector hybrid | <20ms |
| Full hybrid (temporal) | <30ms |

### Documentation

- **[Design Document](docs/VECTOR_SEARCH_DESIGN.md)** - Complete Phase 4 documentation
- **[ADR-0019: Hybrid Query Planner](docs/adr/0019-hybrid-query-planner.md)** - Architecture decisions
- **[Hybrid Query Guide](docs/guides/hybrid-query-guide.md)** - Comprehensive user guide

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

### Configuration System

GallifreyDB provides a unified configuration system via `GallifreyDBConfig` that consolidates all settings for WAL, historical storage, and vector indexes.

#### Programmatic Configuration

```rust
use gallifreydb::{GallifreyDB, config::{GallifreyDBConfig, WalConfigBuilder, HistoricalConfigBuilder}};
use gallifreydb::storage::wal::DurabilityMode;

// Build configuration programmatically
let config = GallifreyDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .num_stripes(32).unwrap()               // 32 concurrent append stripes
        .stripe_capacity(2048).unwrap()          // 2048 entries per stripe
        .write_buffer_size(128 * 1024).unwrap() // 128KB write buffer
        .segment_size(128 * 1024 * 1024).unwrap() // 128MB segments
        .durability_mode(DurabilityMode::group_commit_default())
        .build())
    .historical(HistoricalConfigBuilder::new()
        .max_versions_per_entity(5000).unwrap()
        .max_reconstruction_depth(200).unwrap()
        .reconstruction_cache_size(20000).unwrap()
        .build())
    .build();

let db = GallifreyDB::with_unified_config(config);
```

#### TOML Configuration Files

Configuration can be loaded from TOML files (requires default `config-toml` feature):

```toml
# config/production.toml
[wal]
num_stripes = 64
stripe_capacity = 4096
write_buffer_size = 262144    # 256KB
segment_size = 268435456      # 256MB
flush_interval_ms = 10
wal_dir = "data/wal"
segments_to_retain = 20

[historical]
max_versions_per_entity = 10000
max_reconstruction_depth = 200
reconstruction_cache_size = 100000

[vector]
max_k = 10000
max_layer = 16
```

```rust
use gallifreydb::{GallifreyDB, config::GallifreyDBConfig};

let config = GallifreyDBConfig::from_toml_file("config/production.toml")?;
let db = GallifreyDB::with_unified_config(config);
```

**Configuring Durability Mode in TOML:**

```toml
# Synchronous mode (maximum durability, ~1-5ms latency)
[wal]
[wal.durability_mode]
Synchronous = {}

# Group commit mode (high throughput ACID, ~2-10ms latency)
[wal]
[wal.durability_mode.GroupCommit]
max_delay_ms = 10
max_batch_size = 200

# Async mode (highest throughput, eventual durability)
[wal]
[wal.durability_mode.Async]
flush_interval_ms = 100

# Async batched mode (combines benefits of both)
[wal]
[wal.durability_mode.AsyncBatched]
max_delay_ms = 50
max_batch_size = 1000
```

#### Configuration Presets

**Embedded Systems** (minimal memory):
```rust
let config = GallifreyDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .num_stripes(4).unwrap()
        .stripe_capacity(256).unwrap()
        .write_buffer_size(16 * 1024).unwrap()
        .segment_size(16 * 1024 * 1024).unwrap()
        .build())
    .historical(HistoricalConfigBuilder::new()
        .max_versions_per_entity(100).unwrap()
        .reconstruction_cache_size(1000).unwrap()
        .build())
    .build();
```

**Cloud Deployment** (high throughput):
```rust
let config = GallifreyDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .num_stripes(64).unwrap()
        .stripe_capacity(4096).unwrap()
        .write_buffer_size(256 * 1024).unwrap()
        .segment_size(256 * 1024 * 1024).unwrap()
        .build())
    .historical(HistoricalConfigBuilder::new()
        .max_versions_per_entity(10000).unwrap()
        .reconstruction_cache_size(100000).unwrap()
        .build())
    .build();
```

#### Key Configuration Parameters

**WAL Configuration:**
- `num_stripes`: Concurrency level (must be power of 2, default: 16)
- `stripe_capacity`: Ring buffer size per stripe (default: 1024)
- `write_buffer_size`: I/O buffer size in bytes (default: 64KB)
- `segment_size`: WAL segment file size (default: 64MB, min: 1MB)
- `segments_to_retain`: Number of segments to keep (default: 10)
- `durability_mode`: Synchronous, GroupCommit, Async, or AsyncBatched

**Historical Storage Configuration:**
- `max_versions_per_entity`: Version limit per entity (default: 1000)
- `max_reconstruction_depth`: Max anchor chain depth (default: 100, max: 1000)
- `reconstruction_cache_size`: LFU cache size (default: 10000)

**Vector Index Configuration:**
- `max_k`: Maximum k for k-NN queries (default: 10000, DoS protection)
- `max_layer`: Maximum HNSW layers (default: 16)

#### Builder Validation

All builder methods validate inputs and return `Result<Self, ConfigError>`:

```rust
// This will error with ConfigError::InvalidValue
let result = WalConfigBuilder::new()
    .num_stripes(0);  // Error: must be > 0

assert!(result.is_err());
```

#### Feature Flags

- **`config-toml`** (default): Enable TOML configuration file support
  - Adds `serde` and `toml` dependencies
  - Enables `from_toml_file()`, `from_toml_str()`, `to_toml_file()`, `to_toml_string()` methods
  - Disable with `default-features = false` if only using programmatic configuration

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

### Tracy Profiler

Use Tracy for detailed CPU profiling:

1. Download Tracy from [releases](https://github.com/wolfpld/tracy/releases)
2. Build with profiling: `cargo build --release --features tracy`
3. Run profiled build: `just profile-tracy`

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

### Development Tools

All common tasks via `just`:
- `just test` - Run tests
- `just coverage` - Generate coverage report
- `just lint` - Run clippy
- `just fmt` - Format code
- `just pre-commit` - Quick pre-commit checks
- `just check-all` - Full quality check

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
