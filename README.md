# GallifreyDB

[![CI](https://github.com/madmax983/GallifreyDB/actions/workflows/ci.yml/badge.svg)](https://github.com/madmax983/GallifreyDB/actions/workflows/ci.yml) [![codecov](https://codecov.io/gh/madmax983/GallifreyDB/branch/trunk/graph/badge.svg)](https://codecov.io/gh/madmax983/GallifreyDB) [![Security Policy](https://img.shields.io/badge/security-policy-blue.svg)](SECURITY.md)

A high-performance bi-temporal graph database in Rust, designed for LLM integration and temporal reasoning.

## Overview

GallifreyDB tracks both **valid time** (when facts were true in reality) and **transaction time** (when facts were recorded in the database). This enables powerful time-traveling queries and historical analysis, making it ideal for LLM applications that need to understand how knowledge evolves over time.

### Key Features

- **Bi-Temporal Model**: Track both valid time and transaction time for full temporal reasoning
- **Hybrid Storage**: Separate current state (fast path) from historical data (temporal path)
- **Anchor+Delta Compression**: 5-6X storage reduction while maintaining query performance
- **ACID Transactions**: Full snapshot isolation with write conflict detection
- **Write-Ahead Log (WAL)**: Crash recovery with versioned binary format
- **Vector Storage**: Embeddings support for semantic search (Phase 1 complete)
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

# Check code coverage (must meet 80% threshold)
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
```

See `justfile` for all available commands.

## Project Status

**Current Phase**: Core Complete, Vector Search in Progress

### Core Features (Complete)
- [x] Core ID types (NodeId, EdgeId, VersionId)
- [x] Temporal primitives (BiTemporalInterval, TimeRange)
- [x] Property system with Arc-based deduplication
- [x] String interning for memory efficiency
- [x] Error types and Result handling
- [x] Test coverage infrastructure (80% threshold)
- [x] Tracy profiling integration
- [x] Current storage layer with CSR adjacency indexes
- [x] Historical storage with anchor+delta compression
- [x] ACID transactions with snapshot isolation
- [x] Write conflict detection (Issue #8)
- [x] Write-Ahead Log (WAL) with versioned format
- [x] Persistence layer with recovery
- [x] Time-travel queries (as_of, get_node_at_time)
- [x] Public API with read/write transactions

### Vector Storage (Phase 1 Complete)
- [x] Vector type with validation (VS-001 to VS-010)
- [x] Similarity functions: cosine, Euclidean, dot product
- [x] Vector normalization utilities
- [x] Distance metric abstraction
- [x] Property-attached vector embeddings
- [x] Historical vector versioning

### In Progress
- [ ] Vector indexing (HNSW integration)
- [ ] Graph + Vector hybrid queries
- [ ] Temporal vector drift tracking
- [ ] MCP Server for Claude integration

**Test Coverage**: 455+ tests passing, coverage tracking enabled

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

- **[CLAUDE.md](CLAUDE.md)** - Architecture principles and development guidelines
- **[TESTING.md](TESTING.md)** - Testing, coverage, and profiling guide
- **[WORKTREE_WORKFLOW.md](WORKTREE_WORKFLOW.md)** - Parallel development workflow
- **[docs/VECTOR_SEARCH_DESIGN.md](docs/VECTOR_SEARCH_DESIGN.md)** - Vector search architecture
- **[justfile](justfile)** - Available development commands

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
- Maintain ≥80% code coverage
- Follow coding guidelines in CLAUDE.md
- Include appropriate documentation

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
