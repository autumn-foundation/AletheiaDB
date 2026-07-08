# Changelog

All notable changes to AletheiaDB will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Valid-time retraction (Issue #3230): `AletheiaDB::retract_node`,
  `retract_node_detach`, and `retract_edge` (plus
  `WriteTransaction::retract_node`/`retract_edge` and the MCP
  `retract_node`/`retract_edge` tools) close an entity's valid-time
  interval at a chosen `valid_to` (default now; backdating and
  up-to-one-year future dating supported) **without deleting its history**.
  `AS OF VALID_TIME` before `valid_to` still returns the fact; at/after it
  does not; `AS OF SYSTEM_TIME` before the retraction's commit still shows
  the fact open-ended (append-only — the past record is never rewritten).
  `retract_node` mirrors the #3209 safe-by-default contract: it refuses
  with the count of **distinct** connected edges (a self-loop counts once)
  unless `detach` co-retracts them atomically at the same `valid_to`.
  Re-retracting an already-retracted (or deleted) entity is an idempotent
  no-op returning the existing interval. New WAL operations
  `RetractNode`/`RetractEdge` replay faithfully on crash recovery, honoring
  the logged `valid_to`. See
  [docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#retracting-a-fact-closing-valid-time).

- Queryable bi-temporal extent (Issue #3238):
  `AletheiaDB::temporal_extent()` / `temporal_extent_by_label()` and the MCP
  `temporal_extent` tool report the dataset's earliest/latest valid-time and
  transaction-time coordinates across recorded history — including
  expired/superseded versions and delete tombstones — so a caller (notably
  an LLM over MCP) can calibrate `AS OF` queries to land inside real data.
  Overall bounds are O(1) reads of an aggregate the temporal indexes
  maintain at write time and only ever widen while the process runs; an
  empty database returns explicit `null`s/`None`s, never epoch 0. Optional
  `by_label: true` adds per-node-label / per-edge-type bounds folded from
  hot-tier history. Known limitation: on databases with cold-storage
  migration, versions migrated to the cold tier before the last restart are
  not reflected (the indexes rebuild from hot-tier versions at startup).

- Valid-time writes on the convenience API and MCP tools (Issue #3221):
  `AletheiaDB::create_node_with_valid_time`, `create_edge_with_valid_time`,
  `update_node_with_valid_time`, `update_edge_with_valid_time`,
  `delete_node_with_valid_time`, and `delete_edge_with_valid_time` expose the
  existing `WriteOps::*_with_valid_time` trait methods on the top-level type.
  The MCP `create_node`, `create_edge`, `update_node`, `update_edge`,
  `delete_node`, and `delete_edge` tools gain an optional `valid_time` field
  (ISO 8601 / RFC 3339 or microseconds since epoch) so an LLM can record a
  fact's real-world effective date — including when it stopped being true —
  in a single tool call. Purely additive; omitting `valid_time` reproduces
  prior behavior exactly. On `delete_node`, `valid_time` is not supported
  together with `detach: true` (cascade delete does not support backdating).

### Changed

- Vector index loading at startup is now parallel with per-index error
  isolation (Issue #451): with index persistence enabled, all per-property
  HNSW vector indexes are loaded concurrently (one rayon task per property)
  and a corrupted or unreadable vector index (bad `meta.idx`,
  `mappings.idx`, or `current.usearch`, unknown metric) is skipped with a
  warning instead of aborting the loading of every remaining vector index.
  Startup now also logs a loaded/skipped summary when any index is skipped.
  A skipped index can be re-enabled and rebuilt from node properties. See
  [docs/guides/index-persistence-guide.md](docs/guides/index-persistence-guide.md#vector-index-persistence).

- MCP tool error responses are now structured (Issue #3234): every error is
  `{"error": {"code", "message", "retriable", "details"?}}` instead of
  `{"error": "<string>"}`. `code` is drawn from a stable seven-value enum
  (`NOT_FOUND`, `INVALID_ARGUMENT`, `CONSTRAINT_VIOLATION`,
  `FAILED_PRECONDITION`, `CONFLICT`, `UNAVAILABLE`, `INTERNAL`); `retriable`
  is `true` only for transient classes (timeouts, clock skew,
  serialization/write conflicts) and always `false` for caller-fault classes;
  `details` carries optional per-code metadata (e.g. the DETACH refusal's
  `connected_edges`, a unique violation's `existing_node_id`). The previous
  free-text error message is preserved verbatim at `error.message`. The
  `query` tool keeps its own `kind` field verbatim, with `code`/`retriable`
  added additively alongside it. **Breaking for consumers that read `error`
  as a string** (e.g. `error.as_str()`): the JSON type of the `error` value
  changed from string to object. See
  [docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#structured-error-codes-and-the-retriable-contract).

### Fixed

- `create_edge_with_valid_time` now enforces the same "not more than one
  year in the future" cap as every other `*_with_valid_time` operation; it
  previously accepted an arbitrarily-far-future `valid_time` on edges.
- The "valid_time must not precede entity creation" check on
  `update_node_with_valid_time`, `update_edge_with_valid_time`,
  `delete_node_with_valid_time`, and `delete_edge_with_valid_time` now
  compares against the entity's true original creation time instead of its
  most recent version, so backfilling a correction between two existing
  (already backdated) versions no longer fails with a spurious
  `ValidTimeBeforeEntityCreation` error.

## [0.1.1] - 2026-05-12

### Fixed

- MCP server startup now uses `AletheiaDB::open_from_env()`, so stdio MCP
  sessions honor `ALETHEIADB_CONFIG` and `ALETHEIADB_DATA_DIR` instead of
  silently creating a fresh ephemeral database.

### Added

- Initial Python SDK package under `python/`, with PyO3 bindings for graph
  CRUD, traversal, temporal queries, vector search, and Cypher/AQL execution.
- Python wheel CI/release workflow for Linux, macOS, Windows, source
  distributions, and Trusted Publishing to PyPI on `python-v*` tags.

### Changed

- `AletheiaDB::new()` is now explicitly tempdir-backed and ephemeral; durable
  entry points should use `AletheiaDB::open_from_env()` or an explicit unified
  config.
- Updated the Python SDK's PyO3 dependency to `0.24`.
- Excluded `python/**` from the root Rust crate package published to crates.io.

## [0.1.0] - 2026-05-06

### Breaking

- **Experimental "Nova" feature split into category flags**
  ([ADR-0050](docs/adr/0050-experimental-feature-categorization.md)).
  The single `nova = []` flag has been replaced with five category flags:
  - `semantic-search` (graduated to **stable**)
  - `semantic-reasoning`
  - `semantic-temporal`
  - `semantic-diagnostics`
  - `semantic-characterization`

  The `nova` umbrella now enables only the four `semantic-*` cohorts. It **no longer
  enables the semantic-search cohort** — add `"semantic-search"` alongside
  `"nova"` in your `features` list to keep prior behaviour:
  ```toml
  aletheiadb = { version = "0.1", features = ["nova", "semantic-search"] }
  ```

- **Path change for graduated modules**: 14 search-cohort modules moved from
  `aletheiadb::experimental::*` to `aletheiadb::semantic_search::*`. Affected
  modules: `fishing`, `gestalt`, `cartographer`, `highlander`, `janus`,
  `chameleon`, `semantic_navigator`, `concept_algebra`, `serendipity`,
  `voyager`, `spectre`, `telepathy`, `tapestry`, `horizon`. Update imports:
  ```rust
  // Before
  use aletheiadb::experimental::fishing::FishingRod;
  // After
  use aletheiadb::semantic_search::fishing::FishingRod;
  ```

### Stabilized

- **Semantic search cohort graduates from experimental** to stable under the
  new `semantic-search` feature flag. Includes 14 modules covering associative
  retrieval, fuzzy pattern matching, clustering, entity resolution, and
  vector-guided traversal. The remaining "Nova" categories continue under
  `semantic-*` flags.

### Added

- `just check-features` recipe verifies each Nova/semantic-search category
  compiles standalone.

#### Phase 2: Hybrid Logical Clock Integration (2026-01-20)

- **Hybrid Logical Clock Timestamps** ([ADR-0024](docs/adr/0024-hybrid-logical-clock-timestamps.md))
  - Replaced simple `i64` timestamps with `HybridTimestamp` (12-byte structure)
  - Combines physical wallclock time (8 bytes) + logical counter (4 bytes)
  - Enables distributed operation with causal consistency
  - Provides strict total ordering for concurrent transactions
  - Maintains MVCC snapshot isolation guarantees
  - 25 new HLC-specific tests, all 1,327+ tests passing

**Breaking Changes:**
- `Timestamp` type alias now maps to `HybridTimestamp` instead of `i64`
- All timestamp parameters require `.into()` for integer literals
- Binary serialization format changed (8 bytes → 12 bytes)
- Arithmetic on timestamps requires wallclock accessor: `(offset + timestamp.wallclock()).into()`

**Migration Guide:**
```rust
// Before (Phase 1):
let timestamp: i64 = 1000;
let later = timestamp + 100;

// After (Phase 2):
let timestamp: Timestamp = 1000.into();
let later: Timestamp = (100 + timestamp.wallclock()).into();

// Or use the From trait:
use aletheiadb::core::temporal::Timestamp;
let timestamp = Timestamp::from(1000);
```

**Performance Impact:**
- Storage: +50% per timestamp (12 vs 8 bytes)
  - Mitigated by anchor+delta compression
  - Overall database overhead: <2%
- CPU: No measurable impact (comparison remains O(1))
- All performance targets maintained

**References:**
- PR #423: Phase 2 HLC Integration (299→0 compilation errors)
- [Logical Physical Clocks Paper](https://cse.buffalo.edu/tech-reports/2014-04.pdf) (Kulkarni & Demirbas, 2014)
- [CockroachDB HLC Blog Post](https://www.cockroachlabs.com/blog/living-without-atomic-clocks/)

---

#### Index Persistence Layer (2026-01-16)

- **Fast Cold Starts** ([ADR-0023](docs/adr/0023-index-persistence-layer.md))
  - Save indexes to disk for 6-30x faster startup
  - Zstd compression reduces disk usage by 60-75%
  - Memory-mapped loading for multi-GB indexes
  - Parallel loading (graph + temporal + vector)
  - Configurable via `PersistenceConfig`

**Performance:**
- 1M nodes: 30-60s WAL replay → 2-5s index loading
- 10M nodes: 5-10min WAL replay → 20-30s index loading
- Compression: ~65% size reduction with Zstd

---

#### Multi-Property Vector Indexing (2026-01-15)

- **Multiple Vector Properties** ([ADR-0022](docs/adr/0022-multi-property-vector-index.md))
  - Support multiple vector embeddings per database
  - Property-scoped vector indexes
  - Independent HNSW configurations per property
  - Temporal vector indexes with semantic drift tracking

**Use Cases:**
- Different embedding models (text vs image)
- Multi-lingual embeddings
- Domain-specific embeddings (code, documentation, data)

---

#### Hybrid Query System (2026-01-14)

- **Unified Query API** ([ADR-0021](docs/adr/0021-hybrid-query-execution.md))
  - Graph traversal + Vector similarity + Temporal queries
  - Builder pattern API
  - Query planner with cost-based optimization
  - Single query combining all three dimensions

**Example:**
```rust
db.query()
    .as_of(valid_time, tx_time)
    .start(alice_id)
    .traverse("KNOWS")
    .rank_by_similarity(&embedding, 10)
    .execute(&db)?;
```

---

#### Concurrent WAL Architecture (2026-01-10)

- **Striped Lock-Free WAL** ([ADR-0020](docs/adr/0020-concurrent-wal-architecture.md))
  - Lock-free ring buffer with 16 stripes
  - Configurable durability modes (Sync, GroupCommit, Async)
  - Background flush coordinator
  - Zero-allocation hot path

**Performance:**
- Sync: ~1.5ms latency, ~600 ops/sec
- GroupCommit: ~10-50ms latency, ~100K ops/sec
- Async: <100ns latency, ~500K ops/sec

---

#### Temporal Vector Search (2026-01-08)

- **Time-Travel Vector Queries** ([ADR-0017](docs/adr/0017-temporal-vector-strategy.md), [ADR-0018](docs/adr/0018-temporal-vector-historical-integration.md))
  - Snapshot-based temporal indexes
  - Point-in-time vector search
  - Semantic drift tracking
  - Integration with HistoricalStorage

**Use Cases:**
- "What was semantically similar in 2023?"
- "How has document meaning changed over time?"
- "Track knowledge evolution for LLM reasoning"

---

#### Embedding Providers (2026-01-04)

- **Pluggable Embedding Generation** ([ADR-0016](docs/adr/0016-embedding-providers.md))
  - OpenAI provider (text-embedding-3-small/large)
  - HuggingFace provider (local models)
  - Ollama provider (local LLMs)
  - ONNX provider (portable inference)
  - Feature flags for optional dependencies

---

### Changed

- **Storage Refactoring (Breaking Change)**: Removed the `ColdStorage` trait and `FileColdStorage` implementation.
  - `RedbColdStorage` is now the sole concrete implementation for cold storage.
  - `TieredStorage` and `MigrationService` now take `Arc<RedbColdStorage>` instead of `Arc<dyn ColdStorage>` or `Box<dyn ColdStorage>`.
  - Simplifies the storage hierarchy and removes dynamic dispatch overhead.
- Improved test coverage to 86.45% line coverage, 89.10% function coverage
- Enhanced CI/CD with automated benchmarking and coverage reporting
- Updated all documentation to reflect HybridTimestamp migration

### Fixed

- Doctest compilation issues in temporal vector examples
- HybridTimestamp deserialization validation for sentinel values
- Cleanup script and temporary file commits in repository

---

## Project Status

**Version:** 0.1.0
**Rust Version:** 1.92+
**License:** MIT OR Apache-2.0

**Test Coverage:**
- Library tests: 1,327 passing
- Doctests: 62 passing
- Property tests: Included
- Total: 1,400+ tests passing

**Performance** (historical averages across 30–212 CI datapoints):
- Node/edge lookup: 25.7 ns / 25.4 ns ✓ (target <1µs)
- Single-hop traversal: 185.8 ns ✓ (target <1µs)
- 3-hop traversal: 24.0 µs ✓ (target <100µs)
- Time-travel reconstruction: 82.8 ns ✓ (target <10ms)
- k-NN search k=10, 10K vectors: 127.2 µs ✓ (target <10ms)
- Graph + vector hybrid k=10: 22.5 µs ✓ (target <20ms)

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on contributing to AletheiaDB.

## References

- [Architecture Documentation](docs/ARCHITECTURE.md)
- [Architecture Decision Records](docs/adr/)
- [Testing Guide](TESTING.md)
- [Development Workflow](docs/DEVELOPMENT_WORKFLOW.md)
