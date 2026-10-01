# AletheiaDB Documentation

AletheiaDB is a high-performance **bi-temporal graph database** in Rust that
combines graph traversal, vector similarity search, and full temporal history
in one consistent query. Start with the [Architecture overview](ARCHITECTURE.md)
for the system design, or [CLAUDE.md](../CLAUDE.md) for the condensed
feature/agent reference.

This page is the **task-oriented map** of all documentation — *"I want to… →
read this."* For the complete list of user guides grouped by feature, see the
[Guides index](guides/README.md).

---

## Get started / persist data

| I want to… | Read |
|------------|------|
| Understand what problem this solves | [Why AletheiaDB](guides/why-aletheiadb.md) |
| Learn the bi-temporal model | [Core Concepts](guides/core-concepts.md) |
| Install and build | [Installation](guides/installation.md) |
| Run my first queries | [Getting Started](guides/getting-started.md) · [60-Second Quickstart](guides/quickstart.md) |
| Run the server in a container | [Docker](guides/docker.md) |
| Configure the database | [Configuration Reference](CONFIGURATION.md) |
| Make data durable across restarts | [Persistence Guide](guides/PERSISTENCE.md) · [Index Persistence](guides/index-persistence-guide.md) · [WAL Internals](WAL.md) |
| Snapshot / move a whole database | [Backup & Restore](guides/backup-restore.md) |

## Query the graph

| I want to… | Read |
|------------|------|
| Write AQL queries | [Query Language Design](query-language-design.md) |
| Write Cypher queries | [Cypher Compatibility Matrix](guides/cypher-compatibility.md) |
| Understand query planning/execution | [Query Pipeline Guide](guides/query-pipeline-guide.md) |
| Drive the database from an LLM/MCP host | [MCP Query Tool](guides/mcp-query-tool.md) · [MCP Semantic-Search Tools](guides/mcp-semantic-search-tools.md) |
| Scope data per agent | [Namespaces Guide](guides/namespaces-guide.md) |
| Declare required/typed properties | [Schema Constraints](guides/schema-constraints.md) |

## Vectors & hybrid search

| I want to… | Read |
|------------|------|
| Add embeddings and k-NN search | [Vector Search Integration](guides/vector-search-integration.md) |
| Tune HNSW performance | [Vector Search Performance](guides/vector-search-performance.md) |
| Debug vector search | [Vector Search Troubleshooting](guides/vector-search-troubleshooting.md) |
| Combine graph + vector + temporal | [Hybrid Query Guide](guides/hybrid-query-guide.md) |
| Generate embeddings | [Embeddings](EMBEDDINGS.md) |
| Measure retrieval quality | [Retrieval Evaluation](guides/retrieval-eval.md) |
| Understand vector index internals | [Vector Search Design](VECTOR_SEARCH_DESIGN.md) · [Vector Index Format](vector-index-format.md) |

## Bi-temporal & time-travel

| I want to… | Read |
|------------|------|
| Learn valid time vs. transaction time | [Core Concepts](guides/core-concepts.md) |
| Join facts across time | [Temporal Joins](guides/temporal-joins.md) |
| Pin reproducible reads | [Named Snapshots](guides/snapshot-pin.md) |
| Store unlimited history cheaply | [Tiered Storage Guide](guides/tiered-storage-guide.md) |

## Provenance, lineage & trust

| I want to… | Read |
|------------|------|
| Get a tamper-evident provenance trail | [Provenance Hash Chain](guides/provenance-hash-chain.md) |
| Track fact-to-fact derivation | [Derivation Lineage](guides/derivation-lineage.md) |
| Compute confidence over lineage | [Trust Propagation](guides/trust-propagation.md) |
| Audit why a fact changed | [Belief-Revision Audit](guides/belief-revision.md) |

## React to change

| I want to… | Read |
|------------|------|
| Subscribe to committed changes (pull, long-poll, SSE) | [Reacting to Change](guides/reacting-to-change.md) |

## Security & compliance

| I want to… | Read |
|------------|------|
| Set up auth and roles | [Security Quickstart](guides/security-quickstart.md) |
| See the full authorization matrix | [Access Control Matrix](guides/access-control-matrix.md) |
| Understand encryption end-to-end | [Encryption (overview)](guides/encryption.md) · [Encryption at Rest (detail)](ENCRYPTION.md) |
| Satisfy GDPR right-to-erasure | [GDPR Crypto-Shred](guides/crypto-shred.md) |
| Export an audit trail | [Audit Export](guides/audit-export.md) |

## Operate & scale

| I want to… | Read |
|------------|------|
| Scale horizontally | [Sharding Guide](guides/sharding-guide.md) |
| Manage HTTP sessions/state | [HTTP State Management](guides/http-state-management.md) |
| Bound query cost on HTTP | [HTTP Query Limits](guides/http-query-limits.md) |
| Add tracing/metrics | [OpenTelemetry Tracing](guides/otel-tracing-guide.md) · [Observability](OBSERVABILITY.md) |
| Benchmark | [Benchmarking](BENCHMARKING.md) · [MCP Latency Benchmarks](guides/mcp-latency-benchmarks.md) |
| Move data in/out of other tools | [Parquet Import/Export](guides/parquet-import-export.md) · [Neo4j Import](guides/neo4j-import.md) · [Migrating from XTDB](guides/migrating-from-xtdb.md) · [Migrating from Datomic](guides/migrating-from-datomic.md) |
| Upgrade an existing deployment | [0.1 → 0.2 Migration](guides/migration-0.1-to-0.2.md) |

## Experimental — semantic temporal (`semantic-temporal` cohort)

These cover shipped-but-experimental features behind `semantic-*` flags:

- [Belief-Revision Audit](guides/belief-revision.md) — classify why a fact changed
- [Knowledge Half-Life](guides/knowledge-half-life.md) — survival analysis over fact volatility
- [Contradiction Genealogy](guides/contradiction-genealogy.md) — the life of competing claims
- [Counterfactual Replay](guides/counterfactual-replay.md) — "the world without source X"
- [Temporal Drift Alarms (demo)](guides/drift-alarms-demo.md) — detecting semantic drift

## Contribute

| I want to… | Read |
|------------|------|
| Follow the dev workflow | [Development Workflow](DEVELOPMENT_WORKFLOW.md) |
| Match the coding style | [Coding Standards](CODING_STANDARDS.md) |
| Write and run tests | [Testing](../TESTING.md) |
| Validate `unsafe` code | [Miri](MIRI.md) |

---

## Reference sections

- **[Guides](guides/README.md)** — the complete, feature-grouped guide index (single source of truth for every guide)
- **[Architecture](ARCHITECTURE.md)** and **[Architecture deep-dives](architecture/README.md)** — data model, storage, index, transaction, durability, scalability
- **[Architecture Decision Records](adr/README.md)** — the *why* behind each design choice
- **[Design plans](plans/)** — historical implementation plans (many marked executed); a plan captures the design at the time it was written, not necessarily current behavior
- **[Language bindings](../python/README.md)** — Python SDK (PyO3)
