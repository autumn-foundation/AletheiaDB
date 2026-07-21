# AletheiaDB Guides

The guides index. If you're new, start with the four onboarding guides, then
pick the guides relevant to what you're building. For a task-oriented map of
*all* documentation (not just guides), see the top-level
[Documentation index](../README.md).

## Start Here

| Step | Guide | What You'll Learn |
|------|-------|-------------------|
| 0 | [Why AletheiaDB](why-aletheiadb.md) | The problem it solves and why the three dimensions matter together |
| 1 | [Core Concepts](core-concepts.md) | What bi-temporal means, how nodes/edges/time work |
| 2 | [Installation](installation.md) | Prerequisites, adding to Cargo.toml, building |
| 3 | [Getting Started](getting-started.md) | Create a database, nodes, edges, run your first queries |

Also handy on day one:
- [60-Second Quickstart](quickstart.md) — the fastest path to a first (time-travel) query
- [Docker](docker.md) — run the server without a Rust toolchain

---

## By Feature

### Storage & Persistence
- [Persistence Guide](PERSISTENCE.md) — WAL, index persistence, cold storage: when to use each
- [Tiered Storage Guide](tiered-storage-guide.md) — Hot/warm/cold architecture for unlimited history
- [Index Persistence Guide](index-persistence-guide.md) — Fast cold starts with Zstd-compressed index snapshots
- [Backup & Restore](backup-restore.md) — Portable single-file `.albk` snapshots of full bi-temporal state

### Querying
- [Hybrid Query Guide](hybrid-query-guide.md) — Combine graph traversal + vector similarity + temporal in one query
- [Query Pipeline Guide](query-pipeline-guide.md) — How queries are planned and executed internally
- [Cypher Compatibility Matrix](cypher-compatibility.md) — The supported read-only openCypher subset
- [MCP Query Tool](mcp-query-tool.md) — The MCP tool surface: batches, cursors, token budgets, structured errors, temporal reads
- [MCP Semantic-Search Tools](mcp-semantic-search-tools.md) — Read-only semantic analysis tools over the stable cohort
- [Namespaces Guide](namespaces-guide.md) — Multi-agent data scoping: shared knowledge base with private per-agent scratch
- [Schema Constraints](schema-constraints.md) — Opt-in required/typed property declarations per label or edge type

### Vector Search
- [Vector Search Integration](vector-search-integration.md) — HNSW indexing, k-NN search, embedding properties
- [Vector Search Performance](vector-search-performance.md) — Tuning HNSW parameters, batch indexing, benchmarks
- [Vector Search Troubleshooting](vector-search-troubleshooting.md) — Common issues and solutions
- [Retrieval Evaluation](retrieval-eval.md) — Measuring retrieval quality

### Bi-Temporal & Time-Travel
- [Temporal Joins](temporal-joins.md) — Joining facts across their valid/transaction intervals
- [Named Snapshots](snapshot-pin.md) — Pin a name to a bi-temporal coordinate for reproducible reads

### Provenance, Lineage & Trust
- [Provenance Hash Chain](provenance-hash-chain.md) — Tamper-evident provenance chain and verification
- [Derivation Lineage](derivation-lineage.md) — Version-pinned fact-to-fact derivation closures
- [Trust Propagation](trust-propagation.md) — Computed confidence over lineage with explainability

### React to Change
- [Reacting to Change](reacting-to-change.md) — Changefeed subscriptions, `await_changes` long-poll, and the HTTP SSE stream

### Security & Compliance
- [Security Quickstart](security-quickstart.md) — Authentication, RBAC roles, API-key lifecycle
- [Access Control Matrix](access-control-matrix.md) — Canonical role/operation authorization matrix
- [Encryption](encryption.md) — End-to-end encryption overview (at-rest, KMS/Vault, rotation, hot-live enable, crypto-shred)
- [GDPR Crypto-Shred](crypto-shred.md) — Right-to-erasure over bi-temporal history via per-subject key destruction
- [Audit Export](audit-export.md) — Exporting a tamper-evident audit trail

### Experimental — Semantic Temporal (`semantic-temporal` cohort)
- [Belief-Revision Audit](belief-revision.md) — Classify why the database changed its mind about a fact
- [Knowledge Half-Life](knowledge-half-life.md) — Survival analysis over fact volatility and staleness
- [Contradiction Genealogy](contradiction-genealogy.md) — The bi-temporal life of competing claims
- [Counterfactual Replay](counterfactual-replay.md) — Materialize "the world without source X"
- [Temporal Drift Alarms (demo)](drift-alarms-demo.md) — Detecting semantic drift over time

### Scale & Operations
- [Sharding Guide](sharding-guide.md) — Domain-based horizontal scaling with 2PC transactions
- [HTTP State Management](http-state-management.md) — Session and state management for the HTTP API
- [HTTP Query Limits](http-query-limits.md) — Per-query resource limits on the HTTP surface
- [OpenTelemetry Tracing](otel-tracing-guide.md) — Distributed tracing and metrics integration
- [MCP Latency Benchmarks](mcp-latency-benchmarks.md) — Measured MCP tool latencies

### Interoperability & Migration
- [Parquet Import / Export](parquet-import-export.md) — Columnar import/export for DuckDB / pandas / analytics
- [Neo4j Import](neo4j-import.md) — Migrate a Neo4j CSV export with a fidelity report
- [Migrating from XTDB](migrating-from-xtdb.md) — History-preserving concept mapping and query translation
- [Migrating from Datomic](migrating-from-datomic.md) — Datomic's single-axis history mapped onto the bi-temporal model
- [0.1 → 0.2 Migration](migration-0.1-to-0.2.md) — Upgrading an embedded 0.1.x deployment to 0.2.0

---

## By Role

**I'm building an LLM integration (MCP, RAG, Claude):**
→ [Why AletheiaDB](why-aletheiadb.md) → [Getting Started](getting-started.md) → [MCP Query Tool](mcp-query-tool.md) → [Hybrid Query Guide](hybrid-query-guide.md)

**I'm embedding AletheiaDB as a Rust library:**
→ [Installation](installation.md) → [Getting Started](getting-started.md) → [Persistence Guide](PERSISTENCE.md)

**I need time-travel queries:**
→ [Core Concepts](core-concepts.md) → [Getting Started](getting-started.md) → [Temporal Joins](temporal-joins.md) → [Named Snapshots](snapshot-pin.md)

**I need semantic/vector search:**
→ [Getting Started](getting-started.md) → [Vector Search Integration](vector-search-integration.md) → [Hybrid Query Guide](hybrid-query-guide.md)

**I'm scaling to large datasets:**
→ [Sharding Guide](sharding-guide.md) → [Tiered Storage Guide](tiered-storage-guide.md)

**I'm securing a deployment:**
→ [Security Quickstart](security-quickstart.md) → [Access Control Matrix](access-control-matrix.md) → [Encryption](encryption.md)

**I'm migrating from another store:**
→ [Core Concepts](core-concepts.md) → [Migrating from XTDB](migrating-from-xtdb.md) / [Migrating from Datomic](migrating-from-datomic.md) / [Neo4j Import](neo4j-import.md)

---

## Deeper Documentation

These live outside the guides but are referenced throughout:

- [Architecture](../ARCHITECTURE.md) — How the system is designed
- [Configuration Reference](../CONFIGURATION.md) — All configuration options
- [WAL Internals](../WAL.md) — Write-ahead log format and concurrency model
- [Encryption at Rest](../ENCRYPTION.md) — Detailed at-rest encryption reference
- [Vector Search Design](../VECTOR_SEARCH_DESIGN.md) — HNSW design and roadmap (Phases 1–5)
- [Query Language Design](../query-language-design.md) — AQL/Cypher grammar and semantics
- [Embeddings](../EMBEDDINGS.md) — Embedding-generation providers
- [Coding Standards](../CODING_STANDARDS.md) — Rust conventions for contributors
- [ADR Index](../adr/README.md) — All architectural decisions with rationale
- [Architecture Deep-Dives](../architecture/README.md) — Data model, storage, index, transaction, durability, scalability
