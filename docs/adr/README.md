# Architecture Decision Records (ADRs)

This directory contains Architecture Decision Records (ADRs) for AletheiaDB.

## What is an ADR?

An Architecture Decision Record captures an important architectural decision made along with its context and consequences. ADRs help teams document why certain decisions were made, making it easier for future team members to understand the rationale behind the architecture.

## ADR Format

We use a modified [MADR (Markdown Any Decision Records)](https://adr.github.io/madr/) format:

```markdown
# ADR-XXXX: Title

**Status:** Proposed | Accepted | Deprecated | Superseded by [ADR-YYYY]
**Date:** YYYY-MM-DD
**Deciders:** [list of people involved]
**Categories:** [storage, index, api, transaction, performance, etc.]

## Context

What is the issue that we're seeing that is motivating this decision or change?

## Decision

What is the change that we're proposing and/or doing?

## Consequences

### Positive
- What becomes easier or possible as a result of this change?

### Negative
- What becomes more difficult or requires additional work?

### Neutral
- What side effects or trade-offs should be noted?

## Alternatives Considered

What other options were evaluated?

## References

- Links to related issues, PRs, documentation, or research
```

## ADR Index

### Accepted

| ID | Title | Date | Categories |
|----|-------|------|------------|
| [ADR-0001](0001-hybrid-storage-architecture.md) | Hybrid Storage Architecture | 2024-12-31 | storage, performance |
| [ADR-0002](0002-bitemporal-data-model.md) | Bi-Temporal Data Model | 2024-12-31 | core, temporal |
| [ADR-0003](0003-mvcc-snapshot-isolation.md) | MVCC with Snapshot Isolation | 2024-12-31 | transaction, concurrency |
| [ADR-0004](0004-anchor-delta-compression.md) | Anchor+Delta Compression | 2024-12-31 | storage, performance |
| [ADR-0005](0005-csr-adjacency-format.md) | CSR Adjacency Format | 2024-12-31 | index, performance |
| [ADR-0006](0006-string-interning.md) | String Interning for Labels | 2024-12-31 | core, memory |
| [ADR-0007](0007-wal-durability.md) | Write-Ahead Log for Durability | 2024-12-31 | storage, durability |
| [ADR-0008](0008-property-value-types.md) | Property Value Type System | 2024-12-31 | core, api |
| [ADR-0009](0009-strong-id-types.md) | Strong ID Types | 2024-12-31 | core, type-safety |
| [ADR-0010](0010-dashmap-current-indexes.md) | DashMap for Current Indexes | 2024-12-31 | index, concurrency |
| [ADR-0011](0011-vector-search-integration.md) | Vector Search Integration (SUPERRAG) | 2024-12-31 | index, vector |
| [ADR-0012](0012-configurable-durability-modes.md) | Configurable Durability Modes | 2026-01-01 | storage, durability, performance |
| [ADR-0013](0013-tiered-storage-architecture.md) | Tiered Storage Architecture | 2026-01-22 | storage, scalability, performance |
| [ADR-0014](0014-graph-sharding-strategy.md) | Graph Sharding Strategy | 2026-01-01 | storage, scalability, distributed |
| [ADR-0015](0015-cicd-automation-workflow.md) | CI/CD Automation and Development Workflow | 2026-01-03 | CI/CD, Development Workflow, Automation, AI-Assisted Development |
| [ADR-0020](0020-concurrent-wal-architecture.md) | Concurrent WAL Architecture (Striped Lock-Free Design) | 2026-01-09 | storage, durability, performance, concurrency |
| [ADR-0021](0021-hybrid-query-execution.md) | Hybrid Query Execution Engine (VS-063) | 2026-01-14 | query-engine, performance, architecture |
| [ADR-0022](0022-multi-property-vector-index.md) | Multi-Property Vector Index Architecture | 2026-01-14 | vector-search, api-design, storage |
| [ADR-0023](0023-index-persistence-layer.md) | Index Persistence Layer | 2026-01-15 | storage, persistence, indexes, durability |
| [ADR-0024](0024-hybrid-logical-clock-timestamps.md) | Hybrid Logical Clock Timestamps | 2026-01-20 | core, temporal, distributed |
| [ADR-0025](0025-redb-cold-storage.md) | Redb Cold Storage and LSN-Based WAL Truncation | 2026-01-24 | storage, durability, persistence, architecture |
| [ADR-0026](0026-incremental-csr-adjacency.md) | Incremental CSR Adjacency Index | 2026-01-26 | index, performance, concurrency |
| [ADR-0027](0027-decouple-storage-from-core.md) | Decouple Storage from Core | 2026-01-27 | architecture, storage, core, modularity |
| [ADR-0026](0028-encryption-at-rest.md) | Encryption-at-Rest Architecture | 2026-01-27 | security, storage, durability, encryption |
| [ADR-0029](0029-semantic-clustering.md) | Semantic Clustering Architecture | 2026-05-20 | architecture, experimental, analytics, vector-search |
| [ADR-0030](0030-model-context-protocol.md) | Adopt Model Context Protocol (MCP) | 2026-05-24 | architecture, interface, ai-integration |
| [ADR-0031](0031-custom-honeycomb-client.md) | Internalize Honeycomb Client | 2026-05-24 | engineering, observability, dependency-management |
| [ADR-0032](0032-concept-algebra.md) | Concept Algebra (Semantic Vector Arithmetic) | 2026-05-24 | architecture, experimental, vector-search, semantic-analysis |
| [ADR-0033](0033-temporal-resonance.md) | Temporal Resonance (Echo) | 2026-05-24 | architecture, experimental, temporal, observability |
| [ADR-0034](0034-standardize-redb-cold-storage.md) | Standardize on Redb for Cold Storage | 2026-01-28 | storage, architecture, simplification |
| [ADR-0035](0035-mutation-testing.md) | Mutation Testing with cargo-mutants | 2026-02-07 | Testing, CI/CD, Quality Assurance |
| [ADR-0036](0036-semantic-temperature.md) | Semantic Temperature (Thermos) | 2026-05-25 | experimental, vector-search, temporal |
| [ADR-0037](0037-semantic-spectroscopy.md) | Semantic Spectroscopy (Prism) | 2026-05-25 | experimental, vector-search, explainability |
| [ADR-0038](0038-counterfactual-graph-analysis.md) | Counterfactual Graph Analysis (Hindsight) | 2026-05-25 | experimental, reasoning, simulation |
| [ADR-0039](0039-wormhole-latent-edge-detection.md) | Wormhole (Latent Edge Detection) | 2026-05-25 | experimental, hybrid-search, reasoning |
| [ADR-0044](0044-concrete-storage-coupling.md) | Concrete Storage Coupling | 2026-03-24 | architecture, storage, performance |
| [ADR-0047](0047-highlander-entity-resolution.md) | Highlander (Entity Resolution) | 2026-01-27 | experimental, semantic-analysis, data-quality |
| [ADR-0048](0048-janus-bridge-detection.md) | Janus (Semantic Bridge Detection) | 2026-01-27 | experimental, semantic-analysis, graph-theory |
| [ADR-0049](0049-muse-semantic-ideation.md) | Muse (Semantic Ideation) | 2026-01-27 | experimental, semantic-analysis, ai-reasoning |
| [ADR-0051](0051-context-aware-faceted-search.md) | Context-Aware Faceted Search (Chameleon) | 2024-05-24 | Experimental, Cognitive Architecture, Search |
| [ADR-0053](0053-cognitive-dynamics.md) | Cognitive Dynamics & Probabilistic Reasoning | 2026-06-15 | Experimental, Cognitive Architecture, Reasoning |
| [ADR-0054](0054-advanced-semantic-traversals-and-synthesis.md) | Advanced Semantic Traversals & Synthesis | 2026-06-25 | Experimental, Cognitive Architecture, Semantic Search, Traversal |
| [ADR-0055](0055-semantic-memory-consolidation.md) | Semantic Memory Consolidation (Mnemosyne) | 2024-05-24 | Experimental, Cognitive Architecture |
| [ADR-0056](0056-chimera-entity-synthesis.md) | Chimera Hybrid Entity Synthesis Engine | 2026-06-01 | Experimental, Cognitive Architecture, Data Synthesis |
| [ADR-0057](0057-synergy-engine.md) | Synergy Engine | 2026-06-25 | Experimental, Cognitive Architecture, Data Synthesis |

### Other (Deprecated/Superseded)

| ID | Title | Date | Categories |
|----|-------|------|------------|
| [ADR-0016](0016-embedding-providers.md) | ADR 0016: Plugin-Based Embedding Generation System | Unknown | none |
| [ADR-0017](0017-temporal-vector-strategy.md) | Temporal Vector Index Strategy | 2026-01-05 | index, vector, temporal |
| [ADR-0018](0018-temporal-vector-historical-integration.md) | ADR 0018: Temporal Vector Historical Integration (VS-047) | Unknown | none |
| [ADR-0019](0019-hybrid-query-planner.md) | ADR 0019: Hybrid Query Planner (VS-060) | Unknown | none |
| [ADR-0040](0040-sherlock-temporal-pattern-matching.md) | 40. Sherlock: Temporal Pattern Matching Engine | 2024-05-22 | none |
| [ADR-0041](0041-dreamer-semantic-trajectory.md) | 41. Dreamer: Semantic Trajectory Extrapolation | 2024-05-22 | none |
| [ADR-0042](0042-chronos-temporal-pathfinding.md) | 42. Chronos: Temporal Graph Analysis & Pathfinding | 2024-05-22 | none |
| [ADR-0043](0043-cognitive-architecture.md) | 43. Cognitive Architecture Components | 2024-05-21 | none |
| [ADR-0045](0045-advanced-semantic-analysis.md) | 45. Advanced Semantic Analysis (Physics of Meaning) | 2024-05-22 | none |
| [ADR-0046](0046-semantic-resonance-alignment.md) | 46. Semantic Resonance & Alignment (Telepathy & Metaphor) | 2026-03-24 | none |
| [ADR-0050](0050-mnemosyne-semantic-memory-consolidation.md) | Mnemosyne: Semantic Memory Consolidation | 2024-05-22 | none |
| [ADR-0052](0052-alchemy-semantic-transformation.md) | 52. Alchemy: Semantic Graph Transformation | 2024-05-24 | none |

## Creating a New ADR

1. Copy `0000-template.md` to a new file with the next available number
2. Fill in the template sections
3. Submit a PR for review
4. Once accepted, update the index in this README

## ADR Lifecycle

```
Proposed → Accepted → [Deprecated | Superseded]
```

- **Proposed**: Under discussion, not yet implemented
- **Accepted**: Approved and implemented (or being implemented)
- **Deprecated**: No longer relevant, kept for historical context
- **Superseded**: Replaced by a newer ADR (linked in status)

## Categories

- `core` - Core data structures and primitives
- `storage` - Storage layer decisions
- `index` - Indexing strategies
- `transaction` - Transaction handling
- `concurrency` - Concurrency and thread safety
- `performance` - Performance optimizations
- `api` - Public API design
- `temporal` - Bi-temporal features
- `durability` - ACID durability guarantees
- `memory` - Memory management
- `type-safety` - Type system decisions
- `vector` - Vector search features
- `query-engine` - Query planning and execution
- `embeddings` - Embedding generation and providers
- `devops` - CI/CD and automation
- `scalability` - Horizontal and vertical scaling
- `distributed` - Distributed systems features
- `security` - Security, encryption, and authentication
- `future` - Planned future work
