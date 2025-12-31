# Architecture Decision Records (ADRs)

This directory contains Architecture Decision Records (ADRs) for GallifreyDB.

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

### Proposed

| ID | Title | Date | Categories |
|----|-------|------|------------|
| [ADR-0011](0011-vector-search-integration.md) | Vector Search Integration (SUPERRAG) | 2024-12-31 | index, vector, future |

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
- `future` - Planned future work
