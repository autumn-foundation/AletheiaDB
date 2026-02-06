# ADR 0019: Hybrid Query Planner (VS-060)

**Status**: Accepted
**Date**: 2026-01-09
**Context**: Issue #73 - Phase 4 SUPERRAG: Unified Query Planner for Graph + Vector + Temporal

## Problem Statement

AletheiaDB's three query dimensions (graph traversal, vector similarity, bi-temporal) operate through separate, direct API methods. This prevents:

1. **Hybrid queries**: "Who did Alice know in 2023 that was similar to Bob?" requires manual orchestration
2. **Query optimization**: No cost-based routing between current vs historical storage
3. **Composability**: Cannot chain operations fluently in a unified query language

## Decision

We will implement a **Pull-Based Iterator Query Planner** with cost-based optimization:

- **Query IR**: Intermediate representation capturing all three dimensions
- **Logical Plans**: Tree of logical operations (source-agnostic)
- **Physical Plans**: Concrete operators bound to storage engines
- **Pull-Based Execution**: Lazy iterator model with early termination

### Architecture

```
Query → LogicalPlan → Optimization → PhysicalPlan → Execution
         │                │               │
         │     ┌──────────┴────────┐     │
         │     │ Optimization Rules │     │
         │     │ - PredicatePushdown│     │
         │     │ - LimitPushdown    │     │
         │     │ - VectorReordering │     │
         │     └───────────────────┘     │
         │                               │
    ┌────┴────┐                    ┌────┴────┐
    │LogicalOp│                    │PhysicalOp│
    │ Scan    │                    │ NodeLookup    │
    │ Filter  │    ==========>     │ HnswSearch    │
    │ Traverse│                    │ IndexedTraversal│
    │ VectorRank│                  │ VectorRerank  │
    └─────────┘                    └───────────────┘
```

## Target Query Patterns

```rust
// Graph + Vector: "Who does Alice know that's similar to Bob?"
db.query()
    .start(alice_id)
    .traverse("KNOWS")
    .rank_by_similarity(&bob_embedding, 10)
    .execute()?;

// Temporal + Vector: "What was similar to this concept in 2023?"
db.query()
    .as_of(timestamp_2023, tx_time)
    .find_similar(&concept_embedding, 10)
    .execute()?;

// Full Hybrid: All three dimensions
db.query()
    .as_of(timestamp_2023, tx_time)
    .start(alice_id)
    .traverse("KNOWS")
    .rank_by_similarity(&bob_embedding, 10)
    .execute()?;
```

## Module Structure

```
src/query/
├── mod.rs              # Module exports and documentation
├── ir.rs               # QueryOp, Predicate, TraversalDepth
├── plan.rs             # LogicalPlan, LogicalOp, TemporalContext
├── builder.rs          # QueryBuilder<S> with type-state pattern
├── planner/
│   ├── mod.rs          # QueryPlanner orchestration
│   ├── physical.rs     # PhysicalPlan, PhysicalOp
│   ├── cost.rs         # Cost model with calibrated weights
│   ├── stats.rs        # Statistics collection (lazy, cached)
│   └── rules/
│       ├── mod.rs      # OptimizationRule trait
│       ├── predicate_pushdown.rs
│       └── limit_pushdown.rs
└── executor/
    ├── mod.rs          # QueryExecutor
    ├── iterators.rs    # Pull-based iterator implementations
    └── results.rs      # QueryRow, QueryResults
```

## Key Design Decisions

### 1. Type-State Builder Pattern

The `QueryBuilder<S>` uses phantom types to enforce valid query composition at compile time:

```rust
pub struct QueryBuilder<S: QueryState> {
    ops: Vec<QueryOp>,
    temporal_context: Option<TemporalContext>,
    hints: QueryHints,
    _state: PhantomData<S>,
}
```

States: `Initial` → `HasNodes` → `HasTraversalResults` / `HasVectorResults`

**Benefits**: Invalid queries fail at compile time, not runtime.

### 2. Pull-Based Iterator Model

All operators implement a common iterator interface:

```rust
pub trait ResultIterator: Send {
    fn next(&mut self) -> Option<Result<QueryRow>>;
    fn size_hint(&self) -> (usize, Option<usize>);
}
```

**Benefits**:
- Lazy evaluation (minimal memory usage)
- Early termination (LIMIT propagation)
- Streaming results for large datasets

### 3. Cost-Based Optimization

The planner uses calibrated cost models:

| Operator | CPU Cost | Notes |
|----------|----------|-------|
| NodeLookup | 0.5µs | O(1) DashMap lookup |
| Traversal | 1.0µs/hop | CSR adjacency |
| HnswSearch | 0.3µs/k | Sub-linear k-NN |
| Filter | 0.1µs | Predicate evaluation |
| TemporalReconstruct | 10µs/delta | Anchor+delta |

### 4. Dual API Design

Both fluent builder and convenience methods:

```rust
// Fluent builder for complex queries
let query = db.query()
    .start(alice)
    .traverse("KNOWS")
    .rank_by_similarity(&embedding, 10)
    .build();

// Convenience method for common patterns
let similar = db.find_similar(node_id, 10)?;
```

### 5. RwLock Strategy

- **CurrentStorage, HnswIndex, Statistics**: Use `parking_lot::RwLock` (performance-critical)
- **HistoricalStorage access**: Use `std::sync::RwLock` (matches `db.rs` type signature)

A future PR should migrate all historical storage access to `parking_lot::RwLock` for consistency.

## Physical Operators

| Operator | Description | Target Latency |
|----------|-------------|----------------|
| `NodeLookup` | O(1) DashMap lookup by ID | <1µs |
| `NodeScan` | Full scan with optional label filter | O(N) |
| `HnswSearch` | k-NN via HNSW index | <10ms (1M vectors) |
| `IndexedTraversal` | CSR adjacency traversal | <1µs/hop |
| `TemporalNodeLookup` | Point-in-time reconstruction | <10ms |
| `TemporalVectorSearch` | k-NN at point-in-time | <15ms |
| `VectorRerank` | Compute similarities, sort | O(n log k) |
| `Filter` | Predicate evaluation | <0.1µs/row |
| `Limit` | Truncate result stream | O(1) |

### Multi-Property Vector Support

Vector operators (`HnswSearch`, `TemporalVectorSearch`, `VectorRerank`) support property-specific queries via `property_key: Option<String>`:

```rust
PhysicalOp::HnswSearch {
    embedding: Arc<[f32]>,
    k: usize,
    label_filter: Option<String>,
    property_key: Option<String>,  // Multi-property support (ADR-0022)
}
```

When `property_key` is `Some`, the executor uses property-specific search methods. When `None`, it falls back to the default "embedding" property for backwards compatibility.

See [ADR-0022](0022-multi-property-vector-index.md) for complete multi-property architecture.

## Consequences

### Positive

- **Unified API**: Single entry point for hybrid queries across all dimensions
- **Optimization opportunities**: Cost-based routing, predicate pushdown, limit propagation
- **Extensibility**: Easy to add new operators and optimization rules
- **LLM-friendly**: Fluent API maps naturally to natural language queries
- **Type safety**: Invalid queries fail at compile time

### Negative

- **Complexity**: Additional abstraction layer between API and storage
- **Learning curve**: Developers must understand logical vs physical plan distinction
- **Potential overhead**: For simple queries, direct API may be faster (mitigated by cost model)

### Neutral

- **Future work**: Full executor implementation with all operators
- **Benchmarks needed**: Verify no regression vs direct API for simple queries

## Alternatives Considered

### Alternative 1: Push-Based Execution

Execute operators eagerly, pushing results downstream.

**Rejected because**:
- Higher memory usage for large intermediate results
- No early termination for LIMIT queries
- Less control over execution order

### Alternative 2: External Query Engine (DataFusion)

Integrate Apache DataFusion for query planning.

**Rejected because**:
- Heavyweight dependency for our use case
- Graph and temporal queries don't map well to relational model
- Would require significant adaptation layer

### Alternative 3: Macro-Based DSL

Use Rust macros for query construction.

**Rejected because**:
- Worse error messages at compile time
- Harder to extend and maintain
- Type-state pattern achieves similar safety with better ergonomics

## Performance Targets

| Query Type | Target | Notes |
|------------|--------|-------|
| Single node lookup | <1µs | Must not regress current O(1) |
| k-NN search (k=10, 1M) | <10ms | HNSW sub-linear |
| 3-hop traversal | <100µs | CSR adjacency |
| Point-in-time | <10ms | Anchor+delta |
| Graph+Vector hybrid | <20ms | traverse + rank |
| Full hybrid (temporal) | <30ms | as_of + traverse + rank |

## Implementation Notes

1. **Statistics collection**: Lazy with caching - collected on first optimized query, cached until invalidated
2. **Optimization rules**: Applied in order; each rule can modify the logical plan tree
3. **Iterator composition**: Physical operators wrap child iterators, forming execution pipeline

## References

- Issue #73: VS-060 Hybrid Query Planner
- [ADR-0001](./0001-hybrid-storage-architecture.md): Hybrid Storage Architecture
- [ADR-0011](./0011-vector-search-integration.md): Vector Search Integration
- [ADR-0018](./0018-temporal-vector-historical-integration.md): Temporal Vector Integration
- [Volcano Query Processing](https://paperhub.s3.amazonaws.com/dace52a42c07f7f8348b08dc2b186061.pdf): Pull-based iterator model
- [Cascades Optimizer](https://15721.courses.cs.cmu.edu/spring2019/papers/22-optimizers/graefe-ieee1995.pdf): Cost-based optimization framework
