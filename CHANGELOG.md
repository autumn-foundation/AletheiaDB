# Changelog

All notable changes to AletheiaDB will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-07-18

First crates.io release since 0.1.1. Version 0.2.0
was developed on trunk but never published to crates.io; this release ships
the accumulated work as 0.3.0 (SemVer minor bumps are mandated by the breaking
changes below; skipping 0.2.0 on crates.io is intentional and allowed). MSRV:
Rust 1.92, edition 2024. License: MIT OR Apache-2.0.

### ⚠️ Breaking changes

- `ReadOps::get_outgoing_edges`, `ReadOps::get_incoming_edges`, and
  `ReadOps::get_outgoing_edges_with_label` now return `Result<Vec<EdgeId>>`
  instead of `Vec<EdgeId>` (Issue #359). A node that does not exist (or is not
  visible in the transaction's snapshot) returns `Err(NodeNotFound)`,
  consistent with `get_node`/`get_edge`; an existing node with no matching
  edges returns `Ok(vec![])`, so callers can distinguish "node has no edges"
  from "node doesn't exist". Migration: append `?` (or `.unwrap_or_default()`
  to keep the old silent-empty behavior) at call sites. The non-transactional
  `AletheiaDB` convenience methods of the same name are unchanged.
- The public error enums are **not** `#[non_exhaustive]` and gained variants,
  so any exhaustive `match` over them now needs a wildcard arm. Notably
  `TransactionError::ValidationFailed` (the #3416 concurrent-orphan-edge
  commit abort), new `ConstraintError` variants (#3378 schema constraints),
  and `Error::Namespace`/`Provenance`/`Lineage`/`Constraint`/`Backup`/
  `FailedPrecondition` (#3349/#3371/#3378/#3351) were added to the top-level
  `Error` and its siblings (`StorageError`, `TransactionError`,
  `ConstraintError`).
- `PersistenceConfig` gained a public field `max_interned_strings: usize`
  (Issue #3716). Struct-literal constructors that name every field break;
  use `..Default::default()`.
- `PersistenceConfig::default()` no longer enables index persistence
  (`enabled: false`, Issue #3388) — a behavioral break. A config that never
  touches `PersistenceConfig` no longer writes index snapshots into a
  cwd-relative `./data`. Opt in explicitly with `enabled: true` + an explicit
  `data_dir`, or use the canonical durable entry points `AletheiaDB::open(path)`
  / `durable_config_for_data_dir(path)` (unaffected; WAL durability is
  independent).
- MCP error responses are now a structured object
  `{"error": {"code", "message", "retriable", "details"?}}` instead of
  `{"error": "<string>"}` (Issue #3234) — a wire break for MCP consumers that
  read `error` as a string. The prior free text is preserved verbatim at
  `error.message`.
- The HTTP error envelope was unified to the same nested `{"error": {...}}`
  shape and the legacy flat body (`{"success": false, "error": "<msg>", ...}`)
  was **removed** (Issue #3234) — a wire break for HTTP clients. HTTP and MCP
  error bodies are now byte-shape-identical; success responses
  (`{"success": true, "data": ...}`) are unchanged.

### On-disk format

- Backup artifact `.albk` bumped to **v7** (folds the crypto-shred keyring +
  subject-designation registry — #3712/#3715 — alongside the #3218
  unique-constraint registry and #3378 schema constraints). The reader still
  decodes v1–v6, so a new binary reads old backups; a v7 backup is **not**
  readable by a ≤0.1.1 binary (forward-incompatible).
- Encryption-at-rest on-disk **state v2** plus keyring / crypto-shred
  designation registry (Issue #3616/#3359) — new persisted structures with no
  0.1.1 equivalent, present only when the `encryption` feature is in use.
- WAL v5 (plaintext) / v6 (encrypted), index-persistence manifest v3, and
  cold-storage record tag v3 were bumped **backward-compatibly** to carry
  `provenance.principal` (Issue #3350). Older artifacts still load, with
  `principal: None`.

### Added

#### Encryption suite

- Durable encryption-state authority establishing the on-disk source of truth
  for the database's encryption posture (Issue #3616, PR 1 of 4).
- WAL runtime-installable keyring: the write-ahead log can transition from
  plaintext to encrypted while running (Issue #3616 PR2), with keyring
  provisioning at `open()` (Issue #488/#3653).
- `enable_encryption(&mut self, KeyProviderConfig) -> Result<EnableReport>`
  performs an in-place plaintext→encrypted migration, and
  `disable_encryption(&mut self) -> Result<DisableReport>` performs the
  encrypted→plaintext reverse (Issue #3616 PR3/PR4).
- Cold-tier (redb) key rotation, completing full-MEK all-layer key rotation
  across every storage layer (Issue #3617 PR3 of 3).
- New feature flags: `encryption`, `encryption-aws-kms`, `encryption-vault`.

#### GDPR crypto-shred (Issue #3359)

- Subject-key axis foundation for per-subject cryptographic erasure.
- Seal-at-write / unseal-at-read property-path integration, with a
  fail-closed erase-vs-seal race hardening and a public erased accessor.
- Provenance-chain erasure stability (a shredded subject leaves the
  tamper-evident chain verifiable).
- CLI support, plus MCP admin tools `designate_subject` and
  `erase_subject` (tool registry 61→63), with a 1000-target DoS cap on
  designation.
- The keyring + designation registry are folded into `.albk` backups (format
  v7).

#### Namespaces (Issue #3349)

- Core registry model with reserved-key ride-along and elision.
- Storage/query threading: a membership index, namespace-scoped reads, and a
  traversal boundary that respects namespace membership.
- MCP/HTTP namespace parameters and per-namespace counts, plus the
  `create_namespace` / `list_namespaces` / `describe_namespace` MCP tools
  (registry 58→61).
- `ChangeFilter.namespace` for namespace-scoped changefeed subscriptions.

#### Changefeed (Issues #3375, #3216, #3652, #3673, #3678)

- `AletheiaDB::subscribe_changes` in-process subscription primitive with a
  bounded buffer, best-effort at-least-once delivery, and lossless resume via
  `list_changes` (Issue #3375).
- `await_changes` MCP long-poll tool plus the HTTP SSE `GET /changes/stream`
  route (Issue #3652).
- Event-driven await: no worker pinned during the block, prompt slot release
  (Issue #3673).
- Per-principal subscription quota (Issue #3678).
- Filter + limit pushdown into the `list_changes` hot/cold scans (Issue #3216).

#### Query languages (Issues #3622, #558, #557, #548)

- Edge-property `WHERE` + `ORDER BY` predicates for both AQL and Cypher
  (Issue #3622), with consolidated edge-predicate helpers and a `Cow` sort
  path.
- Cypher aggregation — `count`/`sum`/`avg`/`min`/`max`/`collect` (each with
  optional `DISTINCT`) with openCypher implicit grouping (Issue #558).
- Cypher `OPTIONAL MATCH` left-outer patterns (Issue #557).
- Cypher variable-depth traversal `-[:REL*min..max]->` (Issue #548).

#### MCP / HTTP surface (Issues #3234, #3368, #3561, #3629, #3353, #3360)

- The MCP tool registry now exposes **63 tools**.
- Structured error codes with a `retriable` flag and per-code `details`
  metadata (Issue #3234).
- Token-budget-aware responses: `max_response_tokens` / `max_response_bytes` /
  `priority_properties` on the budgetable read tools, degrading along a
  disclosed ladder with fetch handles (Issue #3353).
- Cursor continuation for large scans: snapshot-anchored, duplicate-free,
  gap-free paging on the bounded read tools (Issue #3360).
- Per-query resource limits (wall-clock timeout + result-byte cap) extended to
  the read tools, including a default-off memory-budget dimension (Issue #3368).
- Inbound HTTP and MCP-over-HTTP concurrency budgets and body cap, rate-limit
  mounting, and timeout→429 mapping (Issue #3561).
- Constraint / precondition / conflict classification on the legacy JSON-RPC
  write path (Issue #3629/#3234).

#### Bi-temporal, provenance & lineage

- Valid-time writes on the convenience API and the MCP create/update/delete
  node/edge tools via an optional `valid_time` (Issue #3221).
- Valid-time retraction: `retract_node` / `retract_node_detach` /
  `retract_edge` close an entity's valid-time interval without deleting its
  history (Issue #3230).
- Queryable bi-temporal `temporal_extent` reporting the dataset's
  earliest/latest valid-time and transaction-time coordinates (Issue #3238).
- Derivation lineage: version-pinned upstream/downstream fact-to-fact closures
  (`create_*_with_lineage`, `upstream_lineage`/`downstream_lineage`, MCP
  `lineage_upstream`/`lineage_downstream`) (Issue #3371).
- Named snapshots for reproducible reads: pin a name to a bi-temporal
  coordinate whose handle returns identical results regardless of later writes
  (Issue #3370).
- Provenance-weighted retrieval fusion (Rust API + core) (Issue #3372).
- Belief-revision audit — when and why the database changed its mind
  (Issue #3362).
- Tamper-evident provenance hash chain with `aletheia verify` and the
  `verify_chain` / `export_chain_head` MCP tools (Issue #3351).
- Schema constraints — opt-in per-label/per-edge-type property types and
  required keys, enforced at the pre-apply commit hook (Issue #3378).

#### Batching & atomicity

- Atomic multi-write batches with local refs via MCP (Issue #3231): the new
  `apply_batch` tool accepts an **ordered** array of write operations
  (`create_node`, `create_edge`, `update_node`, `update_edge`, `delete_node`,
  `delete_edge`, each supporting the #3221 optional `valid_time`) that commit
  **all-or-nothing** in a single `WriteTransaction` (one WAL batch append,
  one GroupCommit fsync). A `create_node` may carry a `ref` alias; later edge
  operations may reference batch-created nodes as `"$alias"` or positionally
  as `"$<index>"` — forward/unknown/duplicate refs are rejected statically
  with a precise `details.failed_op_index` before any transaction opens. Any
  failure (validation, constraint violation, #3209 detach refusal — enforced
  against committed **and** batch-created edges via a batch-local adjacency
  ledger) rolls the whole batch back: zero writes become visible. On success
  the response returns per-operation results in input order (entity ids,
  version ids for creates/updates) plus a `ref_map` of every alias to its
  committed real id. Batch size is capped (default 1000, tunable via
  `AletheiaMcpServer::with_max_batch_operations`; the limit is echoed on
  rejection per #3226). See
  [docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#atomic-multi-write-batches-apply_batch).

#### Authentication & RBAC

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
  revocation is immediate. Auth failures are a uniform `UNAUTHENTICATED`;
  role denials are `PERMISSION_DENIED` — both additive to the #3234
  error-code enum. Authenticated writes stamp the verified principal's name
  into version provenance (`provenance.principal`) on the structured
  create/update node/edge paths of both surfaces. See
  [docs/guides/security-quickstart.md](docs/guides/security-quickstart.md).

#### Other

- Configurable string-interner cap `max_interned_strings` on
  `PersistenceConfig`, plus elimination of the background-persist infinite
  retry loop (Issue #3716).
- The #3218 unique-constraint registry is now included in `.albk` backups
  (Issue #3663).

### Changed

- Vector index loading at startup is now parallel with per-index error
  isolation (Issue #451): with index persistence enabled, all per-property
  HNSW vector indexes load concurrently (one rayon task per property) and a
  corrupted or unreadable vector index is skipped with a warning instead of
  aborting the loading of every remaining index. A skipped index is recovered
  with `AletheiaDB::rebuild_vector_index(property, config)`. See
  [docs/guides/index-persistence-guide.md](docs/guides/index-persistence-guide.md#vector-index-persistence).
- Bulk MCP read responses now evaluate `is_current` against a single
  per-request timestamp (Issue #3391): the wallclock is captured once per
  tool call and every entity's `temporal.is_current` in that response
  (`list_nodes`, `traverse`, `get_outgoing_edges`/`get_incoming_edges`,
  `find_similar`, `find_nodes_at_time`, `hybrid_query`, ...) is judged against
  the same instant, instead of one clock read per serialized entity.
- Removed the legacy single-property temporal vector index state (Issue #450):
  the internal `TemporalVectorIndexState` (which mirrored only the most
  recently enabled temporal index) is gone, and the multi-property
  `temporal_vector_indexes` DashMap (Issue #389) is now the single source of
  truth. No public types were removed, but the property-less temporal APIs
  (`find_similar_as_of` and siblings) now deterministically query the
  **alphabetically first** temporal-indexed property instead of the most
  recently enabled one. Migration: name the property explicitly, e.g.
  `db.find_similar_as_of_in("content_embedding", &query, 10, ts)?`.
- Several breaking behavioral changes are cross-referenced under
  **Breaking changes** above (`PersistenceConfig::default()` no longer enables
  index persistence, #3388; `ReadOps` edge getters now return `Result`, #359).

### Fixed

- Multi-property temporal vector indexes now all receive write-path updates
  (Issue #450): with two or more temporal vector indexes enabled, node
  creates/updates index vectors into **every** matching property index,
  deletes remove the node from every index, and post-commit snapshot
  notifications reach every index. Previously only the most recently enabled
  temporal index was maintained.
- WAL: the flush coordinator no longer appends to an existing segment file
  whose header format version differs from the version the writer emits
  (Issue #3423). Replay derives the parse version solely from the segment
  header, so such an append produced a mixed-version segment whose newer
  entries failed CRC/parsing on recovery. The writer now rolls forward to the
  next segment id on a mismatched (or unreadable) header.
- `create_edge_with_valid_time` now enforces the same "not more than one year
  in the future" cap as every other `*_with_valid_time` operation; it
  previously accepted an arbitrarily-far-future `valid_time` on edges.
- The "valid_time must not precede entity creation" check on
  `update_node_with_valid_time`, `update_edge_with_valid_time`,
  `delete_node_with_valid_time`, and `delete_edge_with_valid_time` now
  compares against the entity's true original creation time instead of its
  most recent version, so backfilling a correction between two existing
  (already backdated) versions no longer fails with a spurious
  `ValidTimeBeforeEntityCreation` error.
- Backup restore no longer calls the process-global `GLOBAL_INTERNER.clear()`
  (Issue #3713), which could corrupt string labels in a concurrently-open
  database sharing the process.

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
