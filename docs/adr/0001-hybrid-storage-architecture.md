# ADR-0001: Hybrid Storage Architecture

**Status:** Accepted
**Date:** 2024-12-31
**Deciders:** GallifreyDB Core Team
**Categories:** storage, performance

## Context

GallifreyDB is a bi-temporal graph database designed to track both valid time (when facts were true in reality) and transaction time (when facts were recorded). The primary use case is enabling LLMs to query not just current knowledge, but to see how knowledge evolved over time.

The fundamental tension is between:
1. **Current-state query performance**: 90%+ of queries target current data
2. **Temporal query capability**: Must support efficient time-travel queries
3. **Storage efficiency**: Cannot afford unbounded storage growth

Traditional approaches either:
- Store everything temporally (slow current-state queries due to version filtering)
- Store only current state (no temporal capability)
- Use views/snapshots (complex consistency, high storage)

We need current-state queries to be as fast as non-temporal graph databases while maintaining efficient temporal query capability.

## Decision

We will implement a **hybrid storage architecture** with separate storage paths for current state and historical data:

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
│  (Fast Path)    │          │  (Temporal Path)  │
│                 │          │                   │
│ - Live graph    │          │ - Version chains  │
│ - Hot indexes   │          │ - Anchor+delta    │
│ - No temporal   │          │ - Compressed      │
│   overhead      │          │                   │
└─────────────────┘          └───────────────────┘
```

### Key Design Principles

1. **Current Storage** optimizes for read performance:
   - No version filtering on reads
   - Lock-free data structures (DashMap)
   - Direct access to latest state

2. **Historical Storage** optimizes for storage efficiency:
   - Anchor+delta compression (see ADR-0004)
   - Immutable after creation
   - Aggressive caching possible

3. **Query Router** directs queries to appropriate storage:
   - Non-temporal queries → Current Storage
   - Time-travel queries → Historical Storage
   - Hybrid queries → Both (e.g., compare current to historical)

4. **Synchronization** maintains consistency:
   - Writes update both storages atomically
   - Current storage always reflects latest committed state
   - Historical storage appends new versions

## Consequences

### Positive

- **Zero overhead for current-state queries**: No version filtering, no temporal predicate evaluation
- **Performance target achievable**: <1µs single-hop traversal possible with dedicated current-state path
- **Clear separation of concerns**: Each storage layer can be optimized independently
- **Enables aggressive compression**: Historical data can be compressed without affecting current-state performance
- **Simpler current-state indexes**: No need to index temporal metadata for current queries

### Negative

- **Increased complexity**: Two storage systems to maintain
- **Write amplification**: Updates must be reflected in both storages
- **Memory overhead**: Current state duplicates some historical data
- **Consistency challenge**: Must ensure both storages remain in sync

### Neutral

- Code organization naturally follows the dual-storage pattern
- API design must consider both query paths
- Testing must cover both paths and their interaction

## Alternatives Considered

### Alternative 1: Unified Temporal Storage

Store all data with full temporal metadata, filter to current state at query time.

**Rejected because:**
- Every current-state query incurs temporal filtering overhead
- Index structures must include temporal dimensions
- Cannot achieve <1µs single-hop traversal target
- This is the traditional temporal database approach that sacrifices current-state performance

### Alternative 2: Materialized Views

Maintain a "current view" as a materialized view over temporal storage.

**Rejected because:**
- View maintenance adds write latency
- Complex invalidation logic needed
- Still need full temporal storage underneath
- Hybrid approach gives us the view benefits without the complexity

### Alternative 3: Snapshot-based Approach

Store full snapshots at regular intervals, delta from most recent snapshot for current state.

**Rejected because:**
- High storage overhead for full snapshots
- Current state reads may need snapshot + deltas
- Less efficient than dedicated current-state storage

## Implementation Notes

- `CurrentStorage` uses `DashMap<NodeId, Node>` for lock-free concurrent access
- `HistoricalStorage` uses version chains with anchor+delta compression
- Transaction commit atomically updates both storages
- WAL ensures durability across both storages

## References

- [AeonG: Efficient Temporal Graph Database](https://arxiv.org/abs/2304.12212)
- [XTDB Bi-temporality](https://v1-docs.xtdb.com/concepts/bitemporality/)
- ADR-0004: Anchor+Delta Compression
- ADR-0010: DashMap for Current Indexes
