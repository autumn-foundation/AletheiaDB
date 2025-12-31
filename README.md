# GallifreyDB

A high-performance bi-temporal graph database in Rust, designed for LLM integration and temporal reasoning.

## Overview

GallifreyDB tracks both **valid time** (when facts were true in reality) and **transaction time** (when facts were recorded in the database). This enables powerful time-traveling queries and historical analysis, making it ideal for LLM applications that need to understand how knowledge evolves over time.

### Key Features

- **Bi-Temporal Model**: Track both valid time and transaction time for full temporal reasoning
- **Hybrid Storage**: Separate current state (fast path) from historical data (temporal path)
- **Anchor+Delta Compression**: 5-6X storage reduction while maintaining query performance
- **High Performance**: <1µs single-hop traversal, <100µs for 3-hop (target)
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
git clone https://github.com/yourusername/gallifreydb
cd gallifreydb

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
```

See `justfile` for all available commands.

## Project Status

**Current Phase**: Core Foundation ✓

- [x] Core ID types (NodeId, EdgeId, VersionId)
- [x] Temporal primitives (BiTemporalInterval, TimeRange)
- [x] Property system with Arc-based deduplication
- [x] String interning for memory efficiency
- [x] Error types and Result handling
- [x] Test coverage infrastructure (80% threshold)
- [x] Tracy profiling integration
- [ ] Current storage layer (in progress)
- [ ] Historical storage with anchor+delta
- [ ] Query engine
- [ ] Persistence & WAL
- [ ] Public API

**Test Coverage**: 49 tests passing, coverage tracking enabled

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

See [CLAUDE.md](CLAUDE.md) for complete architecture and coding guidelines.

## Documentation

- **[CLAUDE.md](CLAUDE.md)** - Architecture principles and development guidelines
- **[TESTING.md](TESTING.md)** - Testing, coverage, and profiling guide
- **[justfile](justfile)** - Available development commands

## Use Cases

### LLM Temporal Reasoning

Enable LLMs to:
- Query "What did we know about X at time T?"
- Track how relationships evolved over time
- Detect contradictions through provenance
- Reason about causality and change

Example:
```rust
// Query current state
let current = db.get_node(alice)?;

// Time-travel to see historical state
let historical = db.as_of(timestamp).get_node(alice)?;

// Track how knowledge changed
let changes = db.between(t1, t2).track_changes(alice)?;
```

## Performance Targets

| Operation | Target | Status |
|-----------|--------|--------|
| Current-state single-hop | <1µs | Pending |
| Current-state 3-hop | <100µs | Pending |
| Time-travel reconstruction | <10ms | Pending |
| Storage overhead | <2X | Pending |
| Write throughput | >100k edges/s | Pending |

## Contributing

1. Fork the repository
2. Create a feature branch
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
```

See [TESTING.md](TESTING.md) for detailed testing guidelines.

## License

Licensed under the MIT License. See [LICENSE](LICENSE) for details.

## References

- [AeonG: Efficient Temporal Graph Database](https://arxiv.org/abs/2304.12212)
- [XTDB Bi-temporality](https://v1-docs.xtdb.com/concepts/bitemporality/)
- [Temporal Database Concepts](https://en.wikipedia.org/wiki/Temporal_database)
- [Rust Performance Book](https://nnethercote.github.io/perf-book/)
