# GallifreyDB

[![CI](https://github.com/madmax983/GallifreyDB/actions/workflows/ci.yml/badge.svg)](https://github.com/madmax983/GallifreyDB/actions/workflows/ci.yml) [![codecov](https://codecov.io/gh/madmax983/GallifreyDB/branch/trunk/graph/badge.svg)](https://codecov.io/gh/madmax983/GallifreyDB) [![Security Policy](https://img.shields.io/badge/security-policy-blue.svg)](SECURITY.md) [![Documentation](https://img.shields.io/badge/docs-GitHub%20Pages-blue.svg)](https://madmax983.github.io/GallifreyDB/)

A high-performance bi-temporal graph database in Rust, designed for LLM integration and temporal reasoning.

## Overview

GallifreyDB tracks both **valid time** (when facts were true in reality) and **transaction time** (when facts were recorded in the database). This enables powerful time-traveling queries and historical analysis, making it ideal for LLM applications that need to understand how knowledge evolves over time.

### Key Features

- **Bi-Temporal Model**: Track both valid time and transaction time for full temporal reasoning
- **Hybrid Storage**: Separate current state (fast path) from historical data (temporal path)
- **Tiered Storage**: Hot/warm/cold architecture for unlimited historical depth with disk-backed cold storage
- **Anchor+Delta Compression**: 5-6X storage reduction while maintaining query performance
- **ACID Transactions**: Full snapshot isolation with write conflict detection
- **Write-Ahead Log (WAL)**: Striped lock-free ring buffer architecture, ~100K+ writes/sec (GroupCommit)
- **Index Persistence**: Fast cold starts (6-30x faster) with Zstd compression and memory-mapped loading
- **Vector Search**: HNSW indexing for k-NN semantic search with full temporal versioning
- **Multi-Property Vector Indexes**: Multiple independent vector properties per database
- **Hybrid Query API**: Combine graph traversal + vector similarity + bi-temporal queries
- **Query Language**: Cypher-like GQL with temporal and vector extensions
- **MCP Server**: Model Context Protocol server for LLM integration (Claude, etc.)
- **Graph Sharding**: Domain-based horizontal scaling with 2PC distributed transactions
- **Semantic Drift Tracking**: Detect how embeddings evolve over time for knowledge evolution analysis
- **Production Observability**: Distributed tracing, metrics, and profiling (optional)
- **High Performance**: Sub-microsecond traversals (~22ns node lookup, ~23ns edge traversal)
- **LLM-Friendly API**: Natural query patterns for reasoning about temporal knowledge

## Quick Start

### Prerequisites

- Rust 1.92+ (edition 2024)
- [just](https://github.com/casey/just) - Command runner (optional but recommended)
- [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov) - For coverage reports
- [Tracy Profiler](https://github.com/wolfpld/tracy) - For performance profiling (optional)

### Installation

```bash
# Clone the repository
git clone https://github.com/madmax983/GallifreyDB
cd GallifreyDB

# Install development tools
cargo install just cargo-llvm-cov

# Build the project
cargo build

# Run tests
cargo test

# Or use just
just test
```

### Development Commands

```bash
# Run tests
just test

# Check code coverage (must meet 85% threshold)
just coverage-check

# Generate coverage report (HTML)
just coverage

# Run linter
just lint

# Format code
just fmt

# Run all pre-commit checks
just pre-commit

# Full quality check (format, lint, test, coverage)
just check-all

# Run benchmarks
just bench

# Run benchmarks and generate HTML tables
just bench-tables
```

See `justfile` for all available commands.

## Feature Flags

GallifreyDB uses Cargo feature flags for optional functionality:

### Default Features
```toml
[dependencies]
gallifreydb = "0.1"  # Includes config-toml by default
```

| Feature | Description | Default |
|---------|-------------|---------|
| `config-toml` | TOML configuration file support | ✅ Yes |

### Observability Features
```toml
[dependencies]
gallifreydb = { version = "0.1", features = ["observability"] }
```

| Feature | Description | Dependencies |
|---------|-------------|--------------|
| `observability` | Core observability (tracing + metrics) | `tracing`, `tracing-subscriber` |
| `observability-tracy` | Tracy CPU profiling integration | `tracing-tracy`, `tracy-client` |
| `observability-honeycomb` | Honeycomb distributed tracing | `tracing-honeycomb`, `libhoney-rust` |
| `observability-prometheus` | Prometheus metrics HTTP server | `metrics`, `metrics-exporter-prometheus` |

### Embedding Provider Features
```toml
[dependencies]
gallifreydb = { version = "0.1", features = ["embedding-openai"] }
```

| Feature | Description | Dependencies |
|---------|-------------|--------------|
| `embeddings` | Core embedding types and service | `tokio`, `async-trait`, `serde` |
| `embedding-openai` | OpenAI embedding provider | `embeddings`, `reqwest` |
| `embedding-huggingface` | HuggingFace embedding provider | `embeddings`, `reqwest` |
| `embedding-ollama` | Ollama local embedding provider | `embeddings`, `reqwest` |
| `embedding-onnx` | ONNX local inference (⚠️ placeholder) | `embeddings`, `ort`, `tokenizers` |
| `embedding-all` | Enable all embedding providers | All of the above |

**Note**: Embedding features are **completely optional** and add zero overhead when disabled. The database core has no embedding dependencies.

### MCP Server Features
```toml
[dependencies]
gallifreydb = { version = "0.1", features = ["mcp-server"] }
```

| Feature | Description | Dependencies |
|---------|-------------|--------------|
| `mcp-server` | Model Context Protocol server for LLM integration | `rmcp`, `tokio`, `serde` |

### Sharding Features
```toml
[dependencies]
gallifreydb = { version = "0.1", features = ["sharding-rpc"] }
```

| Feature | Description | Dependencies |
|---------|-------------|--------------|
| `sharding-rpc` | RPC client for sharding coordination | `reqwest`, `serde` |

Note: Tiered storage with Redb cold storage backend is included by default (no feature flag needed).

## Performance & Benchmarks

GallifreyDB is designed for high performance with minimal temporal overhead. View live benchmark results:

- **[📊 Latest Benchmarks](https://madmax983.github.io/GallifreyDB/benchmarks/)** - Comprehensive tables with all metrics
- **[📈 Historical Trends](https://madmax983.github.io/GallifreyDB/dev/bench/)** - Performance over time with regression tracking

### Current Performance

| Operation | Target | Actual |
|-----------|--------|--------|
| Current-state node lookup | <1µs | ~22ns ✅ |
| Current-state edge traversal | <1µs | ~23ns ✅ |
| 3-hop traversal | <100µs | ~20ns per hop ✅ |
| k-NN search (k=10, 1M vectors) | <10ms | ~4-8ms ✅ |
| Graph+Vector hybrid query | <20ms | ~15ms ✅ |
| Time-travel reconstruction | <10ms | TBD |

**Note**: Time-travel query benchmarks are being improved to measure realistic historical reconstruction scenarios.

Benchmarks are automatically run on every push to trunk and published to GitHub Pages. See [docs/BENCHMARKING.md](docs/BENCHMARKING.md) for detailed benchmarking guide.

## Project Status

**Current Phase**: Vector Search Complete (Phases 1-4), Core Features Complete ✅

### Core Features (Complete ✅)
- [x] Core ID types (NodeId, EdgeId, VersionId)
- [x] Temporal primitives (BiTemporalInterval, TimeRange)
- [x] Property system with Arc-based deduplication
- [x] String interning for memory efficiency
- [x] Error types and Result handling
- [x] Test coverage infrastructure (85%+ threshold enforced)
- [x] Current storage layer with CSR adjacency indexes
- [x] Historical storage with anchor+delta compression
- [x] ACID transactions with snapshot isolation
- [x] Write conflict detection
- [x] Write-Ahead Log (WAL) with striped lock-free ring buffers
- [x] Index persistence with Zstd compression and memory-mapped loading
- [x] Time-travel queries (as_of, get_node_at_time)
- [x] Public API with read/write transactions

### Vector Search (Phases 1-4 Complete ✅)

#### Phase 1-2: Storage + HNSW Indexing
- [x] Vector type with validation (VS-001 to VS-010)
- [x] Similarity functions: cosine, Euclidean, dot product
- [x] Vector normalization utilities
- [x] Distance metric abstraction
- [x] Property-attached vector embeddings
- [x] Historical vector versioning (temporal vectors)
- [x] HNSW indexing for k-NN search
- [x] Auto-indexing on create/update with rollback
- [x] Vector similarity search API
- [x] Multi-property vector indexes (VS-072)
- [x] Optional embedding providers (OpenAI, HuggingFace, Ollama, ONNX)

#### Phase 3: Temporal Vector Integration
- [x] Temporal vector indexes with snapshot/delta architecture
- [x] Pre-anchor hooks for provenance tracking
- [x] Post-commit observers for extensibility
- [x] Semantic drift tracking (detect embedding evolution)
- [x] Point-in-time and range vector queries
- [x] Full/delta snapshot strategies with retention policies

#### Phase 4: Hybrid Query API
- [x] Query builder with type-safe state machine
- [x] Graph + Vector hybrid queries (traverse then rank)
- [x] Temporal + Vector queries (semantic time-travel)
- [x] Full hybrid queries (graph + vector + temporal)
- [x] Predicate filtering and property-specific operations
- [x] Direct functions, builder API, and convenience methods

### Observability (Complete ✅)
- [x] Structured logging with `tracing`
- [x] Tracy profiler integration for CPU profiling
- [x] Honeycomb distributed tracing (via git dependency - [see #271](https://github.com/madmax983/GallifreyDB/issues/271))
- [x] Prometheus metrics HTTP server (stub - [see #272](https://github.com/madmax983/GallifreyDB/issues/272))
- [x] Critical error detection (lock poisons, timestamp violations, WAL checksum failures)
- [x] Error categorization metrics

### MCP Server (Complete ✅)
- [x] Model Context Protocol server binary (`gallifrey-mcp`)
- [x] Node operations (get, create, update, delete, list, count)
- [x] Edge operations (get, create, update, delete, list, count)
- [x] Graph traversal (outgoing, incoming, multi-hop)
- [x] Vector search (find similar, enable/list indexes)
- [x] Temporal queries (get at time)
- [x] Hybrid queries (graph + vector + temporal)

### Query Language (Complete ✅)
- [x] Cypher-like parser (MATCH, WHERE, RETURN, ORDER BY, LIMIT)
- [x] Vector search syntax (SIMILAR TO, RANK BY SIMILARITY)
- [x] Bi-temporal syntax (AS OF, BETWEEN)
- [x] AST-to-IR converter with planner integration
- [x] Comprehensive query documentation

### Graph Sharding (Complete ✅)
- [x] Domain-based node partitioning by label
- [x] Edge replication for cross-shard traversal
- [x] Two-Phase Commit (2PC) distributed transactions
- [x] Circuit breakers for fault tolerance
- [x] Online migration with dual-write support
- [x] Connection pooling and query executor

### Tiered Storage (Complete ✅)
- [x] Three-tier architecture (hot/warm/cold)
- [x] File-based cold storage backend
- [x] Redb cold storage backend (pure Rust, built-in)
- [x] Configurable migration policies
- [x] Latency metrics with percentiles
- [x] LSN-based WAL truncation

### In Progress / Planned
- [ ] Vector Search Phase 5: Streaming and incremental updates
- [ ] Custom Honeycomb client wrapper ([#271](https://github.com/madmax983/GallifreyDB/issues/271))
- [ ] Comprehensive Prometheus metrics suite ([#272](https://github.com/madmax983/GallifreyDB/issues/272))
- [ ] GraphQL/REST API layer
- [ ] Distributed replication

**Test Coverage**: 671+ tests passing, 86%+ line coverage (enforced: 85% minimum)

## Architecture

GallifreyDB uses a hybrid storage architecture:

```
┌─────────────────────────────────────────────────────┐
│              Query Engine                            │
│  - Temporal Query Planner                           │
│  - Graph Traversal Engine                           │
│  - Hybrid Query Optimizer                           │
└─────────────────────────────────────────────────────┘
                        │
        ┌───────────────┴───────────────┐
        │                               │
┌───────▼─────────┐          ┌─────────▼─────────┐
│ Current Storage │          │ Historical Storage │
│ - Live Graph    │          │ - Anchor+Delta     │
│ - Hot Indexes   │          │ - Compressed       │
│ - Vector HNSW   │          │ - Time Indexes     │
│ - Fast Path     │          │ - Vector Snapshots │
└─────────────────┘          └────────────────────┘
```

**Key Design Decisions**:
- Current state separated for zero-overhead queries
- Anchor+delta compression for 5-6X storage savings
- Copy-on-write properties with Arc for deduplication
- String interning for memory efficiency
- Lock-free concurrent access (DashMap)
- Hybrid pre-anchor hooks + post-commit observers for temporal vector integration

See **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** for complete architecture documentation.

## Usage Examples

### Basic Graph Operations

```rust
use gallifreydb::{GallifreyDB, PropertyMap};

// Create a new database
let db = GallifreyDB::new();

// Create nodes using write transactions
let alice_id = db.write(|tx| {
    tx.create_node("Person", PropertyMap::from_iter([
        ("name".into(), "Alice".into()),
        ("age".into(), 30.into()),
    ]))
})?;

let bob_id = db.write(|tx| {
    tx.create_node("Person", PropertyMap::from_iter([
        ("name".into(), "Bob".into()),
    ]))
})?;

// Create relationships
db.write(|tx| {
    tx.create_edge(alice_id, bob_id, "KNOWS", PropertyMap::new())
})?;

// Read current state
let alice = db.get_node(alice_id)?;
```

### Time-Travel Queries

```rust
use gallifreydb::core::temporal::Timestamp;

// Get node at a specific point in time
let historical_alice = db.get_node_at_time(
    alice_id,
    Timestamp::from(past_time),  // valid time
    Timestamp::from(past_time),  // transaction time
)?;

// Track how properties changed
if let Some(old_alice) = historical_alice {
    println!("Alice's age was: {:?}", old_alice.properties.get("age"));
}
```

### Vector Search with HNSW

```rust
use gallifreydb::{GallifreyDB, PropertyMapBuilder};
use gallifreydb::index::vector::{HnswConfig, DistanceMetric};

let db = GallifreyDB::new();

// Enable vector indexing
db.vector_index("embedding")
    .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    .enable()?;

// Store node with embedding - automatically indexed!
let doc_id = db.create_node("Document",
    PropertyMapBuilder::new()
        .insert("title", "Introduction to Rust")
        .insert_vector("embedding", &embedding)
        .build()
)?;

// Find similar nodes
let similar = db.find_similar(doc_id, 10)?;
```

### Hybrid Queries (Graph + Vector + Temporal)

```rust
use gallifreydb::query::QueryBuilder;
use gallifreydb::query::ir::Predicate;

// Simple: Graph + Vector hybrid
let results = db.traverse_and_rank(alice_id, "KNOWS", &query_embedding, 10)?;

// Complex: Full hybrid with builder
let results = db.query()
    .as_of(valid_time, tx_time)        // Temporal: point-in-time
    .start(alice_id)                   // Graph: start node
    .traverse("KNOWS")                 // Graph: traverse edges
    .rank_by_similarity(&embedding, 10) // Vector: rank by similarity
    .filter(Predicate::gt("score", 0.8)) // Filter: high similarity only
    .with_provenance()                 // Include metadata
    .execute(&db)?;

// Property-specific vector queries
let results = db.query()
    .find_similar_builder(&embedding, 10)
    .property("content_embedding")  // Query specific property
    .metric(DistanceMetric::Cosine)
    .finish()
    .execute(&db)?;
```

See **[docs/guides/hybrid-query-guide.md](docs/guides/hybrid-query-guide.md)** for complete API reference.

### Semantic Drift Tracking

```rust
use gallifreydb::index::vector::temporal::DriftMetric;
use gallifreydb::core::temporal::TimeRange;

// Find all nodes with significant semantic drift
let time_range = TimeRange::new(timestamp_2023, timestamp_2024);
let drifted_nodes = db.find_drift_in(
    "embedding",              // Property name
    0.3,                      // Cosine distance threshold
    time_range,
    DriftMetric::Cosine,
)?;

for (node_id, drift_score) in drifted_nodes {
    println!("Node {} drifted by {:.3}", node_id, drift_score);
}
```

### Index Persistence (Fast Cold Starts)

```rust
use gallifreydb::{GallifreyDB, config::GallifreyDBConfig};
use gallifreydb::storage::index_persistence::PersistenceConfig;

// Enable index persistence for 6-30x faster startup
let config = GallifreyDBConfig::builder()
    .persistence(PersistenceConfig {
        enabled: true,
        data_dir: "data/my-database".into(),
        load_on_startup: true,  // Load indexes on startup
        use_mmap: true,         // Memory-map large indexes
        ..Default::default()
    })
    .build();

let db = GallifreyDB::with_unified_config(config);

// Indexes automatically persist in background
// On restart: 2-5s cold start vs 30-60s WAL replay (1M nodes)
```

See **[docs/guides/index-persistence-guide.md](docs/guides/index-persistence-guide.md)** for complete guide.

### Configuration

```rust
use gallifreydb::{GallifreyDB, config::GallifreyDBConfig};
use gallifreydb::storage::wal::DurabilityMode;

// Load from TOML file
let config = GallifreyDBConfig::from_toml_file("config/production.toml")?;
let db = GallifreyDB::with_unified_config(config);

// Or programmatic configuration
let config = GallifreyDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .num_stripes(64).unwrap()  // High concurrency
        .durability_mode(DurabilityMode::group_commit_default())
        .build())
    .build();
```

See **[docs/CONFIGURATION.md](docs/CONFIGURATION.md)** for all configuration options and presets.

### MCP Server (Claude Integration)

Run the MCP server for LLM integration:

```bash
# Start the MCP server (communicates over stdio)
cargo run --bin gallifrey-mcp --features mcp-server
```

Available MCP tools for LLMs:
- **Node Operations**: `get_node`, `create_node`, `update_node`, `delete_node`, `list_nodes`, `count_nodes`
- **Edge Operations**: `get_edge`, `create_edge`, `update_edge`, `delete_edge`, `get_outgoing_edges`, `get_incoming_edges`
- **Traversal**: `traverse` (multi-hop graph traversal)
- **Vector Search**: `find_similar`, `enable_vector_index`, `list_vector_indexes`
- **Temporal**: `get_node_at_time`, `get_edge_at_time`
- **Hybrid**: `hybrid_query` (combined graph + vector + temporal)

### Query Language (GQL)

GallifreyDB supports a Cypher-like query language with temporal and vector extensions:

```cypher
-- Basic graph query
MATCH (n:Person {name: "Alice"})-[:KNOWS]->(friend:Person)
RETURN friend

-- Vector similarity search
SIMILAR TO $embedding LIMIT 10

-- Hybrid graph + vector query
MATCH (a:Person {name: "Alice"})-[:KNOWS]->(friend)
RANK BY SIMILARITY TO $bob_embedding TOP 10
RETURN friend

-- Bi-temporal query (point-in-time)
AS OF '2024-01-15T10:00:00Z'
MATCH (n:Person {name: "Alice"})
RETURN n

-- Full hybrid: temporal + graph + vector
AS OF '2024-06-01T00:00:00Z'
MATCH (user:User {id: $user_id})-[:VIEWED]->(item:Product)
RANK BY SIMILARITY TO $recommendation_embedding TOP 20
WHERE item.price < 100
RETURN item
ORDER BY score DESC
LIMIT 10
```

See **[docs/query-language-design.md](docs/query-language-design.md)** for complete grammar and examples.

### Graph Sharding

For horizontal scaling with datasets exceeding single-machine capacity:

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

// Create coordinator
let coordinator = ShardCoordinator::new(config);

// Route queries to appropriate shards
let shard = coordinator.router().route_node("Person");
```

See **[docs/guides/sharding-guide.md](docs/guides/sharding-guide.md)** for complete guide.

### Tiered Storage

For unlimited historical depth with disk-backed cold storage:

```rust
use gallifreydb::storage::{
    HistoricalStorage, TieredStorage, TieredStorageConfig,
    FileColdStorage, ColdStorageConfig,
};
use std::sync::Arc;

// Create cold storage backend
let cold = FileColdStorage::new("data/cold", ColdStorageConfig::default())?;

// Create tiered storage
let tiered = TieredStorage::new(TieredStorageConfig::default(), Box::new(cold));

// Configure historical storage
let mut historical = HistoricalStorage::new();
historical.set_tiered_storage(Arc::new(tiered));
```

See **[docs/guides/tiered-storage-guide.md](docs/guides/tiered-storage-guide.md)** for complete guide.

### Transactions

```rust
// Explicit read transaction
let result = db.read(|tx| {
    let node = tx.get_node(alice_id)?;
    Ok(node.label.clone())
})?;

// Explicit write transaction with multiple operations
db.write(|tx| {
    let node1 = tx.create_node("Event", PropertyMap::new())?;
    let node2 = tx.create_node("Event", PropertyMap::new())?;
    tx.create_edge(node1, node2, "FOLLOWS", PropertyMap::new())?;
    Ok(())
})?;
```

### Embedding Generation (Optional)

GallifreyDB includes an optional embedding generation system for semantic search:

```rust
use gallifreydb::{GallifreyDB, PropertyMapBuilder};
use gallifreydb::embeddings::{EmbeddingService, providers::openai::*};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Enable in Cargo.toml: features = ["embedding-openai"]

    // 1. Create embedding service
    let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;
    let provider = Arc::new(OpenAIProvider::new(config)?);
    let service = EmbeddingService::new(provider);

    // 2. Generate embeddings
    let documents = vec![
        "GallifreyDB is a bi-temporal graph database",
        "It tracks both valid time and transaction time",
    ];
    let embeddings = service.embed_batch(&documents).await?;

    // 3. Store with vectors
    let db = GallifreyDB::new();
    for (text, embedding) in documents.iter().zip(embeddings.iter()) {
        db.create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("content", *text)
                .insert_vector("embedding", embedding)
                .build(),
        )?;
    }

    Ok(())
}
```

**Available Providers**:
- **OpenAI**: Best quality, API-based (~100-200ms)
- **HuggingFace**: Open-source models, free tier (~200-500ms)
- **Ollama**: Local inference, privacy-focused (~20-50ms)
- **ONNX**: Ultra-fast local, requires setup (~1-10ms)

See **[docs/EMBEDDINGS.md](docs/EMBEDDINGS.md)** for complete documentation.

### Production Observability (Optional)

GallifreyDB includes comprehensive observability features for production deployments:

```bash
# Enable in Cargo.toml:
features = [
    "observability",              # Core: structured logging + metrics
    "observability-tracy",        # Tracy CPU profiling
    "observability-honeycomb",    # Honeycomb distributed tracing
    "observability-prometheus",   # Prometheus metrics HTTP server
]
```

**Basic usage:**

```rust
use gallifreydb::observability;

fn main() {
    // Initialize observability (call once at startup)
    let config = observability::Config::from_env();
    observability::init(config);

    let db = gallifreydb::GallifreyDB::new();

    // Metrics automatically collected
    // Check for critical errors
    let metrics = observability::metrics();
    if metrics.has_critical_errors() {
        panic!("Data corruption detected!");
    }
}
```

**Environment Variables:**
- `RUST_LOG`: Control log level (e.g., `gallifreydb=debug`)
- `HONEYCOMB_API_KEY`: Enable Honeycomb tracing
- `HONEYCOMB_DATASET`: Dataset name (default: "gallifreydb")
- `PROMETHEUS_BIND_ADDR`: Prometheus HTTP endpoint (e.g., "127.0.0.1:9090")

**Critical Metrics** (should NEVER be >0):
- `lock_poison_count`: Thread panicked while holding lock
- `timestamp_violations`: Transaction time not monotonic
- `wal_checksum_failures`: WAL corruption detected

**Backends:**
- **Stdout**: Structured JSON logging (always available)
- **Tracy**: CPU profiling with flamegraphs and zone tracking
- **Honeycomb**: Distributed tracing for span analysis (⚠️ uses git dependency, [see #271](https://github.com/madmax983/GallifreyDB/issues/271))
- **Prometheus**: `/metrics` HTTP endpoint (⚠️ stub implementation, [see #272](https://github.com/madmax983/GallifreyDB/issues/272))

Run the demo:
```bash
export HONEYCOMB_API_KEY="your-key"
export PROMETHEUS_BIND_ADDR="127.0.0.1:9090"
cargo run --example observability_demo --all-features
```

## Documentation

### Core Documentation
- **[CLAUDE.md](CLAUDE.md)** - Quick reference for AI assistants and contributors
- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** - Architecture principles, design patterns, system design
- **[docs/CONFIGURATION.md](docs/CONFIGURATION.md)** - Configuration options, presets, tuning guide
- **[docs/DEVELOPMENT_WORKFLOW.md](docs/DEVELOPMENT_WORKFLOW.md)** - Complete development workflow
- **[docs/CODING_STANDARDS.md](docs/CODING_STANDARDS.md)** - Rust coding standards and best practices
- **[TESTING.md](TESTING.md)** - Testing, coverage, and profiling guide
- **[WORKTREE_WORKFLOW.md](WORKTREE_WORKFLOW.md)** - Parallel development workflow with git worktrees

### Feature Documentation
- **[docs/VECTOR_SEARCH_DESIGN.md](docs/VECTOR_SEARCH_DESIGN.md)** - Vector search architecture (Phases 1-5)
- **[docs/EMBEDDINGS.md](docs/EMBEDDINGS.md)** - Embedding generation guide (optional providers)
- **[docs/WAL.md](docs/WAL.md)** - Write-Ahead Log format and architecture
- **[docs/query-language-design.md](docs/query-language-design.md)** - Query language grammar and semantics

### User Guides
- **[docs/guides/vector-search-integration.md](docs/guides/vector-search-integration.md)** - Complete vector search API
- **[docs/guides/vector-search-performance.md](docs/guides/vector-search-performance.md)** - Performance tuning
- **[docs/guides/hybrid-query-guide.md](docs/guides/hybrid-query-guide.md)** - Hybrid query API reference
- **[docs/guides/index-persistence-guide.md](docs/guides/index-persistence-guide.md)** - Index persistence details
- **[docs/guides/sharding-guide.md](docs/guides/sharding-guide.md)** - Graph sharding and distributed deployment
- **[docs/guides/tiered-storage-guide.md](docs/guides/tiered-storage-guide.md)** - Tiered storage configuration
- **[docs/guides/query-pipeline-guide.md](docs/guides/query-pipeline-guide.md)** - Query execution pipeline

### Architecture Decision Records (ADRs)
- **[docs/adr/0013-tiered-storage-architecture.md](docs/adr/0013-tiered-storage-architecture.md)** - Tiered storage architecture
- **[docs/adr/0014-graph-sharding-strategy.md](docs/adr/0014-graph-sharding-strategy.md)** - Graph sharding strategy
- **[docs/adr/0016-embedding-providers.md](docs/adr/0016-embedding-providers.md)** - Embedding provider architecture
- **[docs/adr/0018-temporal-vector-historical-integration.md](docs/adr/0018-temporal-vector-historical-integration.md)** - Temporal vector integration
- **[docs/adr/0019-hybrid-query-planner.md](docs/adr/0019-hybrid-query-planner.md)** - Hybrid query architecture
- **[docs/adr/0020-concurrent-wal-architecture.md](docs/adr/0020-concurrent-wal-architecture.md)** - Concurrent WAL design
- **[docs/adr/0022-multi-property-vector-index.md](docs/adr/0022-multi-property-vector-index.md)** - Multi-property vector indexes
- **[docs/adr/0023-index-persistence-layer.md](docs/adr/0023-index-persistence-layer.md)** - Index persistence architecture
- **[docs/adr/0024-hybrid-logical-clock-timestamps.md](docs/adr/0024-hybrid-logical-clock-timestamps.md)** - HLC timestamp design

See `docs/adr/` for all architectural decisions.

### Examples

**Recovery Examples:**
- `examples/recovery/basic_recovery.rs` - Automatic database recovery after crash
- `examples/recovery/manual_recovery.rs` - Manual recovery control with statistics
- `examples/recovery/progress_callback.rs` - Recovery with progress tracking

**Other Examples:**
- `examples/observability_demo.rs` - Production observability features
- `examples/doctor_who_demo.rs` - Temporal graph modeling example

## Use Cases

### LLM Temporal Reasoning

Enable LLMs to:
- Query "What did we know about X at time T?"
- Track how relationships evolved over time
- Detect contradictions through provenance
- Reason about causality and change
- Track semantic drift in knowledge over time
- Combine graph structure, semantic similarity, and temporal queries

### Knowledge Graph Evolution

Track how your knowledge graph changes:
- Audit trails for compliance
- Historical analysis and trend detection
- Rollback capabilities
- Provenance tracking
- Semantic evolution analysis

### Retrieval-Augmented Generation (RAG)

Advanced RAG patterns:
- Multi-property semantic search (title, content, image embeddings)
- Hybrid graph+vector queries (traverse then rank by similarity)
- Temporal RAG (retrieve knowledge as it existed at specific times)
- Semantic drift detection (identify when knowledge changed)

## Contributing

1. Fork the repository
2. Create a feature branch (use worktrees: `just worktree-new feature/name`)
3. Run tests: `just test`
4. Check coverage: `just coverage-check`
5. Run pre-commit checks: `just pre-commit`
6. Submit a pull request

All contributions must:
- Pass all tests
- Maintain ≥85% code coverage (line, function, and region)
- Follow coding guidelines in [docs/CODING_STANDARDS.md](docs/CODING_STANDARDS.md)
- Include appropriate documentation
- Never commit directly to trunk (use worktrees and PRs)

See **[docs/DEVELOPMENT_WORKFLOW.md](docs/DEVELOPMENT_WORKFLOW.md)** for complete workflow documentation.

## Testing

```bash
# Run all tests
just test

# Generate coverage report
just coverage

# Profile with Tracy
just profile-tracy

# Run benchmarks
just bench
```

See [TESTING.md](TESTING.md) for detailed testing guidelines.

## License

Licensed under the MIT License. See [LICENSE](LICENSE) for details.

## References

- [AeonG: Efficient Temporal Graph Database](https://arxiv.org/abs/2304.12212)
- [XTDB Bi-temporality](https://v1-docs.xtdb.com/concepts/bitemporality/)
- [Temporal Database Concepts](https://en.wikipedia.org/wiki/Temporal_database)
- [Rust Performance Book](https://nnethercote.github.io/perf-book/)
