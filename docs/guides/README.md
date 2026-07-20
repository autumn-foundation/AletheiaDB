# AletheiaDB Guides

This is the entry point for AletheiaDB documentation. If you're new, start here.

## Start Here

| Step | Guide | What You'll Learn |
|------|-------|-------------------|
| 0 | [Why AletheiaDB](why-aletheiadb.md) | The problem it solves and why the three dimensions matter together |
| 1 | [Core Concepts](core-concepts.md) | What bi-temporal means, how nodes/edges/time work |
| 2 | [Installation](installation.md) | Prerequisites, adding to Cargo.toml, building |
| 3 | [Getting Started](getting-started.md) | Create a database, nodes, edges, run your first queries |

After those four, pick the guides relevant to what you're building.

---

## By Feature

### Storage & Persistence
- [Persistence Guide](PERSISTENCE.md) — WAL, index persistence, cold storage: when to use each
- [Tiered Storage Guide](tiered-storage-guide.md) — Hot/warm/cold architecture for unlimited history
- [Index Persistence Guide](index-persistence-guide.md) — Fast cold starts with Zstd-compressed index snapshots

### Interoperability
- [Parquet Import / Export](parquet-import-export.md) — Columnar import/export of nodes, edges, and full bi-temporal history for DuckDB / pandas / analytics pipelines

### Querying
- [Hybrid Query Guide](hybrid-query-guide.md) — Combine graph traversal + vector similarity + temporal in one query
- [Query Pipeline Guide](query-pipeline-guide.md) — How queries are planned and executed internally
- [Cypher Compatibility Matrix](cypher-compatibility.md) — The supported read-only openCypher subset and how unsupported constructs are rejected
- [Namespaces Guide](namespaces-guide.md) — Multi-agent data scoping: a shared knowledge base with private per-agent scratch (isolated-by-default, shared-by-explicit-query)

### Vector Search
- [Vector Search Integration](vector-search-integration.md) — HNSW indexing, k-NN search, embedding properties
- [Vector Search Performance](vector-search-performance.md) — Tuning HNSW parameters, batch indexing, benchmarks
- [Vector Search Troubleshooting](vector-search-troubleshooting.md) — Common issues and solutions

### Scale & Operations
- [Sharding Guide](sharding-guide.md) — Domain-based horizontal scaling with 2PC transactions
- [HTTP State Management](http-state-management.md) — Session and state management for the HTTP API

### Durable Execution
- [Workflow Journal](workflow-journal.md) — DBOS-style exactly-once step recording: the `durable-execution` schema convention (`WorkflowRun`/`Step`/`HAS_STEP`) and memoized replay (Phase 3a)

### Migration
- [Migrating from XTDB](migrating-from-xtdb.md) — History-preserving concept mapping, valid-time backfill, and query translation from XTDB
- [Migrating from Datomic](migrating-from-datomic.md) — Datomic's single-axis (tx-time) history mapped onto AletheiaDB's bi-temporal model, with provenance and query translation
- - [Migrating from Neo4j](neo4j-import.md) — Migrate a Neo4j CSV export (typed headers) with a fidelity report

---

## By Role

**I'm building an LLM integration (MCP, RAG, Claude):**
→ [Why AletheiaDB](why-aletheiadb.md) → [Getting Started](getting-started.md) → [Hybrid Query Guide](hybrid-query-guide.md)

**I'm embedding AletheiaDB as a Rust library:**
→ [Installation](installation.md) → [Getting Started](getting-started.md) → [Persistence Guide](PERSISTENCE.md)

**I need time-travel queries:**
→ [Core Concepts](core-concepts.md) → [Getting Started](getting-started.md) → [Tiered Storage Guide](tiered-storage-guide.md)

**I need semantic/vector search:**
→ [Getting Started](getting-started.md) → [Vector Search Integration](vector-search-integration.md) → [Hybrid Query Guide](hybrid-query-guide.md)

**I'm scaling to large datasets:**
→ [Sharding Guide](sharding-guide.md) → [Tiered Storage Guide](tiered-storage-guide.md)

**I'm migrating from XTDB or Datomic:**
→ [Core Concepts](core-concepts.md) → [Migrating from XTDB](migrating-from-xtdb.md) / [Migrating from Datomic](migrating-from-datomic.md)

---

## Deeper Documentation

These live outside the guides but are referenced throughout:

- [Architecture](../ARCHITECTURE.md) — How the system is designed
- [Configuration Reference](../CONFIGURATION.md) — All configuration options
- [WAL Internals](../WAL.md) — Write-ahead log format and concurrency model
- [Vector Search Design](../VECTOR_SEARCH_DESIGN.md) — HNSW design and roadmap (Phases 1–5)
- [Query Language Design](../query-language-design.md) — AQL/Cypher grammar and semantics
- [Coding Standards](../CODING_STANDARDS.md) — Rust conventions for contributors
- [ADR Index](../adr/) — All architectural decisions with rationale
