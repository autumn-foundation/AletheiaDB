# Changelog

All notable changes to AletheiaDB will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Authentication and role-based access control on both server surfaces
  (Issue #3350): the HTTP server (`aletheia-server`) and the MCP server
  (`aletheia-mcp`) require an API key by default and refuse to start with
  zero credentials; anonymous operation is an explicit, loudly-warned
  opt-in (`ALETHEIADB_AUTH_MODE=anonymous`, fail-closed on invalid values).
  Four roles (`admin`/`writer`/`reader`/`metrics`) gate every HTTP route
  and MCP tool via classifications kept in lockstep with
  `docs/guides/access-control-matrix.md` by CI conformance tests. Key
  lifecycle is served by the HTTP `POST/GET /admin/keys` and
  `POST /admin/keys/revoke` endpoints over a persisted, hashed key store
  (`{data_dir}/auth/keys.json`, SHA-256 digests only, `0600`, atomic
  writes with directory fsync); credentials are re-verified per call so
  revocation is immediate. Auth failures are a uniform `UNAUTHENTICATED`
  (never distinguishing missing/unknown/revoked); role denials are
  `PERMISSION_DENIED` — both additive to the #3234 error-code enum.
  Authenticated writes stamp the verified principal's name into version
  provenance (`provenance.principal`) on the structured create/update
  node/edge paths of both surfaces (deletes/retracts and HTTP AQL-statement
  writes do not stamp a principal yet — known follow-up). Persistence
  format versions bump **backward-compatibly** to carry the new provenance
  field: WAL v5 (plaintext) / v6 (encrypted), index-persistence manifest
  v3, backup artifact v3, cold-storage record tag v3 — all older artifacts
  still load, with `principal: None`. The autumn-web framework's
  sensitive actuator endpoints (`/actuator/env`, `/actuator/configprops`,
  unauthenticated `PUT /actuator/loggers/{name}`, `/actuator/tasks`,
  `/actuator/jobs`, `/actuator/prometheus`) are force-disabled in every
  profile via a hardened config loader, since framework routes bypass the
  API-key layer; the remaining health/metadata framework routes are
  documented in `docs/guides/security-quickstart.md`.

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

- Bulk MCP read responses now evaluate `is_current` against a single
  per-request timestamp (Issue #3391): the wallclock is captured once per
  tool call and every entity's `temporal.is_current` in that response
  (`list_nodes`, `traverse`, `get_outgoing_edges`/`get_incoming_edges`,
  `find_similar`, `find_nodes_at_time`, `hybrid_query`, ...) is judged
  against the same instant, instead of one clock read per serialized
  entity. See
  [docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#temporal-bounds-on-read-responses).
- Removed the legacy single-property temporal vector index state (Issue
  #450): the internal `TemporalVectorIndexState` (which mirrored only the
  most recently enabled temporal index) is gone, and the multi-property
  `temporal_vector_indexes` DashMap introduced by Issue #389 is now the
  single source of truth. No public types were removed, but one behavior
  changed when **multiple** temporal vector indexes are enabled: the
  property-less temporal APIs — `AletheiaDB::find_similar_as_of`
  (deprecated), `similarity_search(...).at_time(...)`,
  `GraphView::find_similar_as_of`, `query::hybrid::find_similar_as_of`, and
  `CurrentStorage::find_similar_as_of` / `find_similar_in_range` — now
  deterministically query the **alphabetically first** temporal-indexed
  property (mirroring the non-temporal default-property rule) instead of
  the most recently enabled one. Migration: name the property explicitly.

  ```rust
  // Before (ambiguous with several temporal indexes -- used the
  // index that happened to be enabled last):
  let results = db.find_similar_as_of(&query, 10, ts)?;

  // After (explicit property -- recommended):
  let results = db.find_similar_as_of_in("content_embedding", &query, 10, ts)?;
  ```

- Vector index loading at startup is now parallel with per-index error
  isolation (Issue #451): with index persistence enabled, all per-property
  HNSW vector indexes are loaded concurrently (one rayon task per property)
  and a corrupted or unreadable vector index (bad `meta.idx`,
  `mappings.idx`, `current.usearch`, or `current.usearch.mappings`; unknown
  metric; out-of-range mapping key; even a panic inside one load task) is
  skipped with a warning instead of aborting the loading of every remaining
  vector index. Startup logs a loaded/skipped summary when any index is
  skipped and reports the actually restored vector count per index. A
  skipped index is recovered with the new
  `AletheiaDB::rebuild_vector_index(property, config)`, which re-enables the
  index and backfills it from the vector properties of current nodes —
  merely re-enabling via `enable_vector_index` creates an empty index that
  the next persistence cycle writes over the on-disk files, losing the
  vectors. See
  [docs/guides/index-persistence-guide.md](docs/guides/index-persistence-guide.md#vector-index-persistence).

- `PersistenceConfig::default()` no longer enables index persistence
  (Issue #3388). The old default (`enabled: true` with the cwd-relative
  `data_dir: "data"`) made every database built from a default or builder
  config silently write index snapshots into `./data` on shutdown and load
  whatever `./data` happened to contain on startup, so unrelated instances
  sharing a working directory could observe each other's data and a stale
  `./data` could short-circuit WAL replay (this caused a real CI flake).
  Index persistence is now opt-in: set `enabled: true` together with an
  explicit `data_dir`, or use the canonical durable entry points
  `AletheiaDB::open(path)` / `durable_config_for_data_dir(path)`, which are
  unaffected. **Breaking for callers that relied on the implicit default:**
  a config that never touches `PersistenceConfig` no longer persists indexes
  (the WAL still provides durability when configured). TOML configs must now
  set `enabled = true` under `[persistence]`; a `[persistence]` section that
  omits `enabled` (even one that sets `data_dir` or `load_on_startup`) is
  treated as disabled.

- **BREAKING**: `ReadOps::get_outgoing_edges`, `ReadOps::get_incoming_edges`,
  and `ReadOps::get_outgoing_edges_with_label` now return
  `Result<Vec<EdgeId>>` instead of `Vec<EdgeId>` (Issue #359). A node that
  does not exist (or is not visible in the transaction's snapshot) returns
  `Err(NodeNotFound)`, consistent with `get_node`/`get_edge`; an existing
  node with no (matching) edges returns `Ok(vec![])`, so callers can finally
  distinguish "node has no edges" from "node doesn't exist". Within a write
  transaction the existence check is buffer-aware: a node created in the
  transaction exists, a node deleted in it does not. The non-transactional
  `AletheiaDB::get_outgoing_edges`/`get_incoming_edges`/
  `get_outgoing_edges_with_label` convenience methods are unchanged.
  Migration: append `?` (or `.unwrap_or_default()` to keep
  the old silent-empty behavior) at call sites. The `ReadOps` trait methods
  also gained comprehensive rustdoc with runnable examples covering the
  empty-vs-missing contract (Issue #358).
  Two edge-case behavior changes ride along: `retract_node_detach` on a node
  previously removed via the plain (non-cascade) `delete_node` no longer
  co-retracts that node's orphaned edges (`edges_retracted: 0`) — consistent
  with the documented "retracting a deleted node is a no-op" contract; and
  `delete_node_cascade` on a node already deleted in the same transaction
  now fails fast with `NodeNotFound` at edge enumeration (same final outcome
  as before, earlier failure point).

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

- Multi-property temporal vector indexes now all receive write-path updates
  (Issue #450): with two or more temporal vector indexes enabled, node
  creates/updates now index vectors into **every** matching property index,
  deletes remove the node from every index, and post-commit snapshot
  notifications reach every index. Previously only the most recently enabled
  temporal index was maintained, silently leaving earlier-enabled temporal
  indexes empty for point-in-time queries.
- WAL: the flush coordinator no longer appends to an existing segment file
  whose header format version differs from the version the writer emits
  (Issue #3423). Replay derives the parse version solely from the segment
  header, so such an append produced a mixed-version segment whose newer
  entries failed CRC/parsing on recovery. The writer now reads the header
  of any existing non-empty segment it is about to reuse and rolls forward
  to the next segment id on a mismatched (or unreadable) header; a failed
  WAL-directory scan during startup id-recovery now warns instead of
  silently under-reporting the next segment id.
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
