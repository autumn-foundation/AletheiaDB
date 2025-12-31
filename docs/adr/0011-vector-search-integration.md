# ADR-0011: Vector Search Integration (SUPERRAG)

**Status:** Proposed
**Date:** 2024-12-31
**Deciders:** GallifreyDB Core Team
**Categories:** index, vector, future

## Context

GallifreyDB's primary use case is enabling LLMs to reason about knowledge evolution. Modern LLMs heavily rely on vector embeddings for semantic similarity. Adding vector search would enable:

- **Semantic retrieval**: Find nodes by meaning, not just keywords
- **Hybrid queries**: Combine graph traversal with similarity ranking
- **Temporal semantics**: Track how meanings drift over time

This positions GallifreyDB as "SUPERRAG" - combining:
- **Graph**: Relationship traversal
- **Vector**: Semantic similarity
- **Bi-temporal**: Knowledge evolution

## Decision

We will integrate **vector search** capabilities following a phased approach:

### Phase 1: Vector Storage Foundation

Add `PropertyValue::Vector` variant:

```rust
pub enum PropertyValue {
    // ... existing variants ...

    /// Dense float vector for embeddings (e.g., 384, 768, 1536 dimensions)
    Vector(Arc<[f32]>),
}
```

### Phase 2: HNSW Index Integration

Use **usearch** crate for HNSW (Hierarchical Navigable Small World) index:

```rust
pub struct VectorIndex {
    /// HNSW index for fast approximate nearest neighbor search
    index: usearch::Index,

    /// Mapping from internal index ID to NodeId
    id_map: DashMap<u64, NodeId>,

    /// Configuration
    config: VectorIndexConfig,
}

pub struct VectorIndexConfig {
    /// Vector dimensions (e.g., 384 for MiniLM, 1536 for OpenAI)
    pub dimensions: usize,

    /// Distance metric
    pub metric: DistanceMetric,

    /// HNSW parameters
    pub connectivity: usize,  // M parameter
    pub expansion_add: usize,  // efConstruction
    pub expansion_search: usize,  // ef
}

pub enum DistanceMetric {
    Cosine,
    Euclidean,
    InnerProduct,
}
```

### Phase 3: Query API

```rust
pub trait VectorOps {
    /// Find k nearest neighbors
    fn find_similar(&self, embedding: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>>;

    /// Find similar with label filter
    fn find_similar_with_label(
        &self,
        embedding: &[f32],
        k: usize,
        label: &str,
    ) -> Result<Vec<(NodeId, f32)>>;

    /// Find similar at specific point in time
    fn find_similar_as_of(
        &self,
        embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>>;
}
```

### Phase 4: Hybrid Queries

```rust
pub trait HybridOps {
    /// Traverse graph, then rank by similarity
    fn traverse_and_rank(
        &self,
        start: NodeId,
        edge_labels: &[&str],
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>>;

    /// Find similar, then traverse relationships
    fn similar_and_traverse(
        &self,
        embedding: &[f32],
        k: usize,
        edge_labels: &[&str],
    ) -> Result<Vec<(NodeId, Vec<NodeId>)>>;
}
```

### Phase 5: Temporal Vector Snapshots

For time-travel vector queries, maintain periodic HNSW snapshots:

```rust
pub struct TemporalVectorIndex {
    /// Current (live) index
    current: VectorIndex,

    /// Historical snapshots at key timestamps
    snapshots: BTreeMap<Timestamp, VectorIndex>,

    /// Snapshot interval
    snapshot_interval: Duration,
}
```

## Consequences

### Positive

- **SUPERRAG capability**: Unique combination of graph + vector + temporal
- **LLM-native**: Aligns with how modern LLMs work
- **Semantic queries**: Find by meaning, not just structure
- **Knowledge evolution**: Track semantic drift over time

### Negative

- **Memory overhead**: HNSW indexes are memory-intensive (~1KB per vector)
- **Build time**: Index construction is O(n log n)
- **Approximate**: HNSW provides approximate, not exact, nearest neighbors
- **Complexity**: Another index to maintain and synchronize

### Neutral

- External dependency (usearch)
- Well-understood trade-offs from vector database space
- Can be optional feature for users who don't need it

## Alternatives Considered

### Alternative 1: External Vector Database

Use dedicated vector DB (Pinecone, Qdrant, Weaviate).

**Rejected because:**
- Loses temporal integration
- Network overhead
- Separate consistency domain
- Core value prop is unified temporal + graph + vector

### Alternative 2: Pure Rust HNSW (hora)

Use hora crate instead of usearch.

**Considered because:**
- Pure Rust, no FFI
- But: Less mature, fewer features

**Decision**: Start with usearch, can switch later if needed.

### Alternative 3: Flat/Brute-Force Search

No index, compute distances at query time.

**Rejected because:**
- O(n) per query
- Not viable for >10k vectors
- But: Keep as fallback for small datasets

### Alternative 4: Product Quantization

Use PQ for compressed vectors.

**Considered for future:**
- Reduces memory by 4-8x
- Slight accuracy loss
- Good for very large collections

## Implementation Notes

### Memory Budget

| Vectors | HNSW Memory | Raw Vectors (384d) |
|---------|-------------|---------------------|
| 100K | ~100MB | ~150MB |
| 1M | ~1GB | ~1.5GB |
| 10M | ~10GB | ~15GB |

### Temporal Strategy

For time-travel vector queries:
1. **Current queries**: Use live index
2. **Historical queries**: Find nearest snapshot, filter by timestamp
3. **Range queries**: Iterate snapshots in range

### Integration with Existing Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    Query Engine                          │
│   Graph Traversal │ Vector Search │ Temporal Queries    │
└─────────────────────────────────────────────────────────┘
          │                 │                  │
    ┌─────▼─────┐    ┌─────▼─────┐     ┌─────▼─────┐
    │  Current  │    │  Vector   │     │ Historical│
    │  Storage  │    │  Index    │     │  Storage  │
    │ (DashMap) │    │  (HNSW)   │     │ (Anchor+Δ)│
    └───────────┘    └───────────┘     └───────────┘
```

### Example Queries

```rust
// Semantic time-travel
db.as_of(timestamp_2023).find_similar(embedding, k)

// Graph + Vector: traverse then rank
db.traverse(alice_id, "KNOWS").rank_by_similarity(bob_embedding, 10)

// Knowledge evolution: track semantic drift
db.track_semantic_drift(node_id, time_range)
```

## References

- [HNSW Paper](https://arxiv.org/abs/1603.09320)
- [usearch Documentation](https://github.com/unum-cloud/usearch)
- [Vector Database Benchmarks](https://ann-benchmarks.com/)
- [docs/VECTOR_SEARCH_DESIGN.md](../VECTOR_SEARCH_DESIGN.md) - Full design document
- ADR-0001: Hybrid Storage Architecture
- ADR-0008: Property Value Types
