# ADR-0021: Hybrid Query Execution Engine (VS-063)

**Status:** Accepted
**Date:** 2026-01-14
**Context:** Issue #85 - Phase 4 Hybrid Query Documentation
**Categories:** query-engine, performance, architecture

## Context

ADR-0019 established the hybrid query planner architecture. This ADR documents the execution engine implementation that brings that architecture to life, focusing on:

1. **Pull-based iterator execution**: How queries are executed lazily
2. **Direct hybrid functions**: Optimized paths for common patterns
3. **Min-heap top-k algorithm**: Efficient ranking strategy
4. **Error handling and graceful degradation**: Production robustness

The query execution layer sits between the physical plan and storage engines, translating operator trees into concrete database operations while maintaining performance targets.

## Decision

### 1. Pull-Based Iterator Model

We implement a pull-based (Volcano-style) execution model where each physical operator implements `ResultIterator`:

```rust
pub trait ResultIterator: Send {
    fn next(&mut self) -> Option<Result<QueryRow>>;
    fn size_hint(&self) -> (usize, Option<usize>);
}
```

**Rationale:**
- **Lazy evaluation**: Only materialize results as needed
- **Early termination**: LIMIT queries stop producing after N results
- **Memory efficiency**: Streaming large result sets without full materialization
- **Composability**: Operators chain naturally via iterator wrapping

### 2. Direct Hybrid Functions

For common query patterns, we provide direct functions that bypass the full planner pipeline:

```rust
// traverse_and_rank: Graph traversal + vector ranking
pub fn traverse_and_rank(
    db: &AletheiaDB,
    start: NodeId,
    edge_label: &str,
    target_embedding: &[f32],
    k: usize,
) -> Result<Vec<(NodeId, f32)>>;

// find_similar_as_of: Temporal vector search
pub fn find_similar_as_of(
    db: &AletheiaDB,
    embedding: &[f32],
    k: usize,
    timestamp: Timestamp,
) -> Result<Vec<(NodeId, f32)>>;
```

**Rationale:**
- **Reduced overhead**: Skip planning for simple patterns
- **Optimized code paths**: Hand-tuned algorithms
- **API ergonomics**: Simple patterns shouldn't require builder complexity
- **Benchmark baseline**: Direct comparison with builder-based queries

### 3. Min-Heap Top-K Algorithm

For `traverse_and_rank` and `RankBySimilarity`, we use a min-heap algorithm:

```rust
struct ScoredCandidate {
    node_id: NodeId,
    similarity: f32,
}

// Min-heap: lowest similarity at top (for easy eviction)
// Reverse Ord implementation: other.similarity.cmp(&self.similarity)

fn rank_top_k(candidates: impl Iterator<Item = ScoredCandidate>, k: usize)
    -> Vec<(NodeId, f32)>
{
    let mut heap = BinaryHeap::with_capacity(k);

    for candidate in candidates {
        if heap.len() < k {
            heap.push(candidate);
        } else if candidate.similarity > heap.peek().unwrap().similarity {
            heap.pop();  // Remove lowest
            heap.push(candidate);
        }
    }

    // Sort descending by similarity
    let mut results: Vec<_> = heap.into_iter().collect();
    results.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap());
    results
}
```

**Complexity Analysis:**
- **Time**: O(N log k) where N = candidates, k = result size
- **Space**: O(k) - only top-k candidates in memory
- **Comparison**: vs O(N log N) for full sort-then-truncate

**Rationale:**
- Optimal for k << N (common case: k=10, N=1000+)
- Streaming-friendly: can process infinite candidate streams
- Memory-bounded: heap size capped at k

### 4. Graceful Degradation Strategy

The execution engine handles edge cases without failing entire queries:

| Scenario | Behavior |
|----------|----------|
| Node without embedding | Skip silently (log warning if enabled) |
| Dimension mismatch | Skip node with warning |
| Missing vector index | Return error immediately |
| Node not found during traversal | Skip (edge points to deleted node) |
| Invalid embedding (NaN/Inf) | Return validation error |

**Rationale:**
- **Partial results > no results**: Users prefer some results over query failure
- **Observability**: Warnings logged for debugging
- **Fail-fast for setup errors**: Missing index is config error, not runtime issue

### 5. Cycle Detection in Traversal

Graph traversal tracks visited nodes to prevent infinite loops:

```rust
let mut visited = HashSet::with_capacity(edge_ids.len().min(k * 2));

for edge_id in edge_ids {
    let edge = db.get_edge(edge_id)?;
    let target_id = edge.target;

    if visited.contains(&target_id) {
        continue;  // Skip already-visited
    }
    visited.insert(target_id);

    // Process target_id...
}
```

**Rationale:**
- **Self-loops**: Node can appear as its own neighbor
- **Cycles**: A→B→C→A shouldn't revisit A
- **Efficiency**: HashSet gives O(1) lookup

### 6. Query Result Structure

Results provide rich metadata for provenance tracking:

```rust
pub struct QueryRow {
    pub entity: EntityResult,
    pub score: Option<f32>,           // Vector similarity
    pub path: Option<Vec<EntityId>>,  // Traversal path
    pub timestamp: Option<Timestamp>, // Temporal context
}

pub enum EntityResult {
    Node(Node),       // Full node with properties
    Edge(Edge),       // Full edge data
    NodeId(NodeId),   // Lightweight ID-only
    EdgeId(EdgeId),
}
```

**Rationale:**
- **Flexibility**: Full nodes when needed, IDs for efficiency
- **Provenance**: Path tracking for audit trails
- **LLM integration**: Rich context for reasoning

## Consequences

### Positive

- **Performance**: Min-heap achieves O(N log k) vs O(N log N)
- **Memory efficiency**: Streaming execution, bounded heap
- **Robustness**: Graceful degradation keeps queries running
- **Ergonomics**: Direct functions for common patterns
- **Debuggability**: Rich result metadata for troubleshooting

### Negative

- **Code duplication**: Direct functions duplicate some builder logic
- **Complexity**: Two code paths (builder vs direct) to maintain
- **Error granularity**: Silent skips may hide data quality issues

### Neutral

- **Iterator overhead**: Pull model has per-call overhead (mitigated by batching)
- **Result materialization**: Final Vec allocation for results

## Alternatives Considered

### Alternative 1: Push-Based Execution

Execute operators eagerly, pushing results downstream.

**Rejected because:**
- Higher memory usage for intermediate results
- No early termination for LIMIT
- Less control over execution order
- Harder to implement backpressure

### Alternative 2: Full Sort for Top-K

Collect all candidates, sort by similarity, truncate to k.

**Rejected because:**
- O(N log N) vs O(N log k) complexity
- Requires materializing all candidates
- No streaming capability
- Wasteful for k << N (99% of use cases)

### Alternative 3: Strict Error Mode

Fail entire query if any node has issues (missing embedding, dimension mismatch).

**Rejected because:**
- Single bad node would fail entire query
- Data quality issues shouldn't prevent useful results
- Users can't always control all nodes in traversal
- Warning logs provide debugging without failure

### Alternative 4: Separate ID-Only and Full-Node APIs

Different methods for `get_node_ids()` vs `get_nodes()`.

**Rejected because:**
- Increases API surface
- Users must choose upfront without knowing downstream needs
- `EntityResult` enum provides flexibility at runtime

## Implementation Notes

### Performance Optimizations

1. **Pre-allocated collections**: `HashSet::with_capacity()` and `BinaryHeap::with_capacity()`
2. **Early exit**: Return immediately when start node doesn't exist
3. **Edge ID batching**: Get all outgoing edges in single call
4. **Property caching**: `Arc<[f32]>` embeddings share memory across versions

### Thread Safety

- `traverse_and_rank` takes `&AletheiaDB` (immutable borrow)
- No internal mutation; safe for concurrent queries
- Results are owned (`Vec<(NodeId, f32)>`), not borrowed

### Error Handling

```rust
// Validation errors propagate immediately
validate_vector(target_embedding)?;  // Early exit on NaN/Inf

// Storage errors propagate
let _start_node = db.get_node(start)?;  // Fail if start doesn't exist

// Runtime issues are skipped with warning
match cosine_similarity(target_embedding, embedding) {
    Ok(similarity) => { /* use it */ }
    Err(_e) => {
        #[cfg(feature = "observability")]
        tracing::warn!(...);
        continue;  // Skip this candidate
    }
}
```

## References

- [ADR-0019: Hybrid Query Planner](./0019-hybrid-query-planner.md) - Query planning architecture
- [ADR-0011: Vector Search Integration](./0011-vector-search-integration.md) - HNSW index design
- [ADR-0018: Temporal Vector Integration](./0018-temporal-vector-historical-integration.md) - Snapshot architecture
- [Issue #85](https://github.com/madmax983/AletheiaDB/issues/85) - VS-072 Phase 4 documentation
- [Volcano Query Processing Paper](https://paperhub.s3.amazonaws.com/dace52a42c07f7f8348b08dc2b186061.pdf) - Pull-based execution model
- [benches/hybrid_query.rs](../../benches/hybrid_query.rs) - Performance benchmarks
