# GallifreyDB

[![CI](https://github.com/madmax983/GallifreyDB/actions/workflows/ci.yml/badge.svg)](https://github.com/madmax983/GallifreyDB/actions/workflows/ci.yml) [![codecov](https://codecov.io/gh/madmax983/GallifreyDB/branch/trunk/graph/badge.svg)](https://codecov.io/gh/madmax983/GallifreyDB) [![Security Policy](https://img.shields.io/badge/security-policy-blue.svg)](SECURITY.md) [![Documentation](https://img.shields.io/badge/docs-GitHub%20Pages-blue.svg)](https://madmax983.github.io/GallifreyDB/)

A high-performance bi-temporal graph database in Rust, designed for LLM integration and temporal reasoning.

## Overview

GallifreyDB tracks both **valid time** (when facts were true in reality) and **transaction time** (when facts were recorded in the database). This enables powerful time-traveling queries and historical analysis, making it ideal for LLM applications that need to understand how knowledge evolves over time.

### Key Features

- **Bi-Temporal Model**: Track both valid time and transaction time for full temporal reasoning
- **Hybrid Storage**: Separate current state (fast path) from historical data (temporal path)
- **Anchor+Delta Compression**: 5-6X storage reduction while maintaining query performance
- **ACID Transactions**: Full snapshot isolation with write conflict detection
- **Write-Ahead Log (WAL)**: Crash recovery with versioned binary format (v2)
- **Vector Search**: HNSW indexing for k-NN semantic search with temporal versioning
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

## Performance & Benchmarks

GallifreyDB is designed for high performance with minimal temporal overhead. View live benchmark results:

- **[📊 Latest Benchmarks](https://madmax983.github.io/GallifreyDB/benchmarks/)** - Comprehensive tables with all metrics
- **[📈 Historical Trends](https://madmax983.github.io/GallifreyDB/dev/bench/)** - Performance over time with regression tracking

### Current Performance

| Metric | Target | Actual |
|--------|--------|--------|
| Current-state node lookup | <1µs | ~22ns ✅ |
| Current-state edge traversal | <1µs | ~23ns ✅ |
| 3-hop traversal | <100µs | ~20ns per hop ✅ |

**Note**: Time-travel query benchmarks are being improved to measure realistic historical reconstruction scenarios.

Benchmarks are automatically run on every push to trunk and published to GitHub Pages. See [docs/BENCHMARKING.md](docs/BENCHMARKING.md) for detailed benchmarking guide.

## Project Status

**Current Phase**: Core Complete, Vector Search (Phase 1-2) Complete, Observability Active

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
- [x] Write-Ahead Log (WAL) v2 with versioned binary format
- [x] Persistence layer with recovery and migration
- [x] Time-travel queries (as_of, get_node_at_time)
- [x] Public API with read/write transactions

### Vector Search (Phase 1-2 Complete ✅)
- [x] Vector type with validation (VS-001 to VS-010)
- [x] Similarity functions: cosine, Euclidean, dot product
- [x] Vector normalization utilities
- [x] Distance metric abstraction
- [x] Property-attached vector embeddings
- [x] Historical vector versioning (temporal vectors)
- [x] HNSW indexing for k-NN search
- [x] Auto-indexing on create/update with rollback
- [x] Vector similarity search API
- [x] Optional embedding providers (OpenAI, HuggingFace, Ollama, ONNX)

### Observability (Complete ✅)
- [x] Structured logging with `tracing`
- [x] Tracy profiler integration for CPU profiling
- [x] Honeycomb distributed tracing (via git dependency - [see #271](https://github.com/madmax983/GallifreyDB/issues/271))
- [x] Prometheus metrics HTTP server (stub - [see #272](https://github.com/madmax983/GallifreyDB/issues/272))
- [x] Critical error detection (lock poisons, timestamp violations, WAL checksum failures)
- [x] Error categorization metrics

### In Progress / Planned
- [ ] Vector Search Phase 3: Temporal vector queries (semantic time-travel)
- [ ] Vector Search Phase 4: Hybrid graph+vector queries
- [ ] Vector Search Phase 5: Streaming and incremental updates
- [ ] Custom Honeycomb client wrapper ([#271](https://github.com/madmax983/GallifreyDB/issues/271))
- [ ] Comprehensive Prometheus metrics suite ([#272](https://github.com/madmax983/GallifreyDB/issues/272))
- [ ] MCP Server for Claude integration
- [ ] GraphQL/REST API layer

**Test Coverage**: 671+ tests passing, 86%+ line coverage (enforced: 85% minimum)

## Architecture

GallifreyDB uses a hybrid storage architecture:

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
│ - Live Graph    │          │ - Anchor+Delta     │
│ - Hot Indexes   │          │ - Compressed       │
│ - Fast Path     │          │ - Time Indexes     │
└─────────────────┘          └────────────────────┘
```

**Key Design Decisions**:
- Current state separated for zero-overhead queries
- Anchor+delta compression for 5-6X storage savings
- Copy-on-write properties with Arc for deduplication
- String interning for memory efficiency
- Lock-free concurrent access (DashMap)

See [CLAUDE.md](CLAUDE.md) for complete architecture and coding guidelines.

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

### Vector Embeddings (Optional)

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

## Performance

| Operation | Target | Achieved |
|-----------|--------|----------|
| Current-state node lookup | <1µs | ~22ns |
| Current-state edge traversal | <1µs | ~23ns |
| Time-travel reconstruction | <10ms | ~20ns |
| Storage overhead | <2X | On target |
| Write throughput | >100k edges/s | 7-12µs per write |

Run benchmarks with `just bench` to verify on your hardware.

## Documentation

### Core Documentation
- **[CLAUDE.md](CLAUDE.md)** - Architecture principles and development guidelines
- **[TESTING.md](TESTING.md)** - Testing, coverage, and profiling guide
- **[WORKTREE_WORKFLOW.md](WORKTREE_WORKFLOW.md)** - Parallel development workflow with git worktrees
- **[justfile](justfile)** - Available development commands

### Feature Documentation
- **[docs/VECTOR_SEARCH_DESIGN.md](docs/VECTOR_SEARCH_DESIGN.md)** - Vector search architecture (Phases 1-5)
- **[docs/EMBEDDINGS.md](docs/EMBEDDINGS.md)** - Embedding generation guide (optional providers)
- **[docs/WAL.md](docs/WAL.md)** - Write-Ahead Log format and migration guide
- **[docs/CODING_STANDARDS.md](docs/CODING_STANDARDS.md)** - Rust coding standards and best practices

### Architecture Decision Records
- **[docs/adr/0016-embedding-providers.md](docs/adr/0016-embedding-providers.md)** - Embedding provider architecture

### Examples
- `examples/observability_demo.rs` - Production observability features
- `examples/doctor_who_demo.rs` - Temporal graph modeling example

## Use Cases

### LLM Temporal Reasoning

Enable LLMs to:
- Query "What did we know about X at time T?"
- Track how relationships evolved over time
- Detect contradictions through provenance
- Reason about causality and change

### Knowledge Graph Evolution

Track how your knowledge graph changes:
- Audit trails for compliance
- Historical analysis and trend detection
- Rollback capabilities
- Provenance tracking

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
- Follow coding guidelines in CLAUDE.md
- Include appropriate documentation
- Never commit directly to trunk (use worktrees and PRs)

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
