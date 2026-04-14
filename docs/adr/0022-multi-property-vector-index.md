# ADR-0022: Multi-Property Vector Index Architecture

**Status:** Accepted
**Date:** 2026-01-14
**Deciders:** @madmax983, Claude
**Categories:** vector-search, api-design, storage

## Context

AletheiaDB's initial vector search implementation (ADR-0011) supported a single vector property per database instance. Real-world applications often require multiple vector properties:

- **Document search**: `title_embedding` + `content_embedding` + `summary_embedding`
- **Multi-modal**: `image_embedding` + `text_embedding` + `audio_embedding`
- **Multi-lingual**: `en_embedding` + `es_embedding` + `fr_embedding`
- **Multi-model**: `openai_embedding` + `cohere_embedding` (different providers)

The existing single-index approach forced users to either:
1. Run multiple database instances (operational overhead)
2. Concatenate vectors (loses semantic separation)
3. Use a single "primary" embedding (loses information)

Additionally, the temporal vector index (ADR-0017, ADR-0018) only supported a single property, limiting semantic evolution tracking to one embedding type.

## Decision

We will implement a multi-property vector index architecture with:

### 1. DashMap-based Concurrent Storage

```rust
struct CurrentStorage {
    // Multi-property HNSW indexes
    vector_indexes: DashMap<String, HnswIndex>,

    // Single temporal index (validated against property name)
    temporal_vector_index_state: RwLock<TemporalVectorIndexState>,
}
```

**Rationale:** DashMap provides lock-free concurrent reads with sharded write locks, matching our existing pattern for node/edge indexes (ADR-0010).

### 2. VectorIndexBuilder Pattern

```rust
db.vector_index("content_embedding")
    .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    .temporal(temporal_config)  // Optional
    .enable()?;
```

**Rationale:** Fluent builder pattern provides:
- Discoverable API through IDE autocomplete
- Compile-time validation of required parameters
- Easy extension for future options (quantization, etc.)

### 3. Property-Specific Query APIs

```rust
// Explicit property queries
db.find_similar_in("content_embedding", &query, 10)?;
db.rank_by_similarity_in("content_embedding", node_ids, &query, 10)?;

// Default "embedding" property for backwards compatibility
db.find_similar(&query, 10)?;  // Uses "embedding" property
```

**Rationale:** Explicit `_in` suffix indicates property-specific operation, while keeping the original API for simple single-property use cases.

### 4. Property Validation for Temporal Queries

Temporal queries validate that the requested property matches the enabled temporal index:

```rust
// Enabled for "content_embedding"
db.find_similar_as_of_in("wrong_property", &q, 10, ts);
// Error: Property 'wrong_property' does not match temporal index property 'content_embedding'
```

**Rationale:** Single temporal index is a current limitation. Clear error messages guide users to the correct property name rather than silently failing.

### 5. QueryBuilder Property Support

```rust
db.query()
    .find_similar_builder(&embedding, 10)
    .property("content_embedding")
    .metric(DistanceMetric::Cosine)
    .finish()
    .execute(&db)?;
```

**Rationale:** Builder pattern allows optional property specification without breaking the fluent API.

## Consequences

### Positive

- **Multi-property support**: Store and query multiple vector properties independently
- **Concurrent access**: DashMap enables parallel queries on different properties
- **Backwards compatible**: Existing single-property code continues to work
- **Discoverable API**: Builder pattern with IDE autocomplete
- **Clear errors**: Property validation provides actionable error messages
- **Unified temporal API**: All temporal methods follow consistent `_in` naming

### Negative

- **Single temporal index**: Only one property can have temporal tracking enabled at a time
- **Memory overhead**: Each property has its own HNSW graph (~1.5x per additional property)
- **API surface increase**: More methods to learn and maintain

### Neutral

- **DashMap dependency**: Already used elsewhere in codebase
- **Property name strings**: Could use type-safe property keys in future

## Alternatives Considered

### Alternative 1: Multi-Temporal Index Support

Support multiple temporal indexes (one per property):

```rust
temporal_indexes: DashMap<String, TemporalVectorIndex>
```

**Not chosen because:**
- Significant complexity increase in anchor/snapshot coordination
- Memory overhead multiplied per temporal property
- Can be added in future iteration if needed

### Alternative 2: Property Enum Instead of Strings

```rust
enum VectorProperty {
    ContentEmbedding,
    TitleEmbedding,
    Custom(String),
}
```

**Not chosen because:**
- Less flexible for dynamic property names
- Breaks serialization compatibility
- String keys are standard in property graphs

### Alternative 3: Single Combined Index with Property Tags

Store all vectors in one HNSW with property metadata:

```rust
struct TaggedVector {
    property: String,
    vector: Arc<[f32]>,
}
```

**Not chosen because:**
- Different properties may have different dimensions
- Cross-property similarity rarely meaningful
- Harder to tune parameters per property

## Implementation Notes

### Storage Layout

```
CurrentStorage
├── vector_indexes: DashMap<String, VectorIndexEntry>
│   ├── "embedding" → VectorIndexEntry { index: HnswIndex, config: HnswConfig(384, Cosine) }
│   ├── "content_embedding" → VectorIndexEntry { index: HnswIndex, config: HnswConfig(768, Cosine) }
│   └── "image_embedding" → VectorIndexEntry { index: HnswIndex, config: HnswConfig(512, Euclidean) }
│
├── temporal_vector_indexes: DashMap<String, TemporalVectorIndexEntry>
│   ├── "embedding" → TemporalVectorIndexEntry { index: TemporalVectorIndex, config: TemporalVectorConfig }
│   └── ... (one entry per enabled temporal property)
│
└── temporal_vector_index_state: RwLock<TemporalVectorIndexState>  // For backwards compatibility
    ├── index: Option<Arc<TemporalVectorIndex>>  // Points to first enabled temporal index
    └── property_name: Option<String>
```

**Note:** The `temporal_vector_index_state` maintains backwards compatibility with the original single-property API while `temporal_vector_indexes` DashMap provides true multi-property temporal support.

### API Method Naming Convention

| Pattern | Meaning | Example |
|---------|---------|---------|
| `find_similar()` | Default "embedding" property | `db.find_similar(&q, 10)` |
| `find_similar_in()` | Explicit property | `db.find_similar_in("prop", &q, 10)` |
| `find_similar_as_of()` | Temporal, default property | `db.find_similar_as_of(&q, 10, ts)` |
| `find_similar_as_of_in()` | Temporal, explicit property | `db.find_similar_as_of_in("prop", &q, 10, ts)` |

### Temporal Property-Specific Methods

All temporal methods require property validation:

- `find_similar_as_of_in(property, embedding, k, timestamp)`
- `track_drift_in(property, node_id, reference, time_range)`
- `semantic_evolution_in(property, node_id, time_range)`
- `find_drift_in(property, threshold, time_range, metric)`

### Query Engine Integration

The QueryBuilder and executor pipeline support multi-property through `property_key` fields:

**Physical Operators:**
```rust
// PhysicalOp variants with property_key
HnswSearch {
    embedding: Arc<[f32]>,
    k: usize,
    label_filter: Option<String>,
    property_key: Option<String>,  // New: specifies which property to search
}

TemporalVectorSearch {
    embedding: Arc<[f32]>,
    k: usize,
    timestamp: Timestamp,
    property_key: Option<String>,  // New: specifies temporal property
}

VectorRerank {
    input: Box<PhysicalOp>,
    embedding: Arc<[f32]>,
    k: usize,
    property_key: Option<String>,  // New: specifies reranking property
}
```

**Executor Behavior:**
- When `property_key` is `Some(prop)`, uses property-specific search methods
- When `property_key` is `None`, falls back to default "embedding" property
- Dimension validation occurs at search time against the specific property's config

## References

- [GitHub Issue #389](https://github.com/madmax983/AletheiaDB/issues/389) - Original feature request
- [PR #404](https://github.com/madmax983/AletheiaDB/pull/404) - Implementation PR
- [ADR-0010](0010-dashmap-current-indexes.md) - DashMap for concurrent indexes
- [ADR-0011](0011-vector-search-integration.md) - Original vector search design
- [ADR-0017](0017-temporal-vector-strategy.md) - Temporal vector strategy
- [ADR-0018](0018-temporal-vector-historical-integration.md) - Historical integration
