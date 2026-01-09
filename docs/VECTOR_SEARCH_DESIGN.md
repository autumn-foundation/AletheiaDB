# Vector Search Integration Design

> **Status**: Phase 3 Complete (VS-047)
> **Created**: 2024-12-30
> **Updated**: 2026-01-08
> **Goal**: Position GallifreyDB as SUPERRAG - Graph + Vector + Bi-temporal
>
> ## Implementation Progress
>
> | Phase | Status | Description |
> |-------|--------|-------------|
> | Phase 1 | ✅ Complete (PR #138) | Vector storage foundation (`PropertyValue::Vector`, similarity functions) |
> | Phase 2 | ✅ Complete (Milestone M2) | HNSW index integration, benchmarks, tests, documentation |
> | Phase 3 | ✅ Complete (Issue #67) | Temporal vector-historical integration, provenance tracking |
> | Phase 4 | 🔲 Planned | Hybrid query engine |
> | Phase 5 | 🔲 Planned | Persistence & performance optimization |

## Executive Summary

Adding vector search to GallifreyDB enables the combination of **graph traversal**, **semantic similarity**, and **bi-temporal tracking**. This enables queries like "What did we know about X that was semantically similar to Y at time T?" - essential for LLM reasoning about knowledge evolution.

## Architecture Integration

### Current Architecture (for reference)

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
└─────────────────┘          └───────────────────┘
```

### Proposed Architecture with Vectors

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
          │                 │                  │
          └─────────────────┴──────────────────┘
                            │
                    ┌───────▼───────┐
                    │  Persistence  │
                    │  (WAL + Snap) │
                    └───────────────┘
```

### Why the Architecture Fits

1. **Arc-based PropertyMap**: Vectors stored as properties won't duplicate across versions if unchanged
2. **Immutable history**: Vector indexes can be built/queried without locks
3. **Dual-path architecture**: "Current vectors" vs "historical vectors" mirrors existing pattern
4. **String interning**: Vector labels/categories can use existing interning infrastructure

## Design Decisions

### Decision 1: Temporal Vector Strategy

Three possible approaches:

| Approach | Description | Complexity | Value |
|----------|-------------|------------|-------|
| **Global vectors** | Single index, latest embeddings only | Low | Basic RAG |
| **Versioned vectors** | Track embedding changes over time | Medium | Knowledge evolution |
| **Temporal reconstruction** | Reconstruct vectors at any point in time | High | Full time-travel |

**Recommendation**: Start with **versioned vectors**. When a node's content changes, its embedding gets a new version. This enables "find similar nodes as of time T" without full reconstruction complexity.

### Decision 2: Vector Storage Format

```rust
// Option A: Dedicated PropertyValue variant (recommended)
pub enum PropertyValue {
    // ... existing variants ...
    Vector(Arc<[f32]>),      // Dense vector, f32 for memory efficiency
    SparseVector(Arc<SparseVec>), // Future: sparse embeddings
}

// Option B: Use existing Bytes/Array
PropertyValue::Bytes(Arc<[u8]>)  // Less type-safe, manual conversion

// Option C: Separate storage
struct VectorStorage {
    embeddings: HashMap<NodeId, Arc<[f32]>>,
}
```

**Recommendation**: Option A - explicit `Vector` variant provides type safety and enables optimized operations.

### Decision 3: Index Library

| Library | Language | Pros | Cons |
|---------|----------|------|------|
| **usearch** | C++/Rust | Fast, filtering support, production-ready | External dependency |
| **hora** | Pure Rust | No FFI, simpler build | Less mature |
| **hnswlib** | C++ | Battle-tested, widely used | C++ bindings complexity |
| **Custom** | Rust | Full control, temporal-aware | Significant effort |

**Recommendation**: **usearch** for initial implementation due to performance and filtering support. Consider custom implementation later for deep temporal integration.

### Decision 4: Query API Design

```rust
// Vector search operations
pub trait VectorOps {
    /// Find k nearest neighbors to embedding
    fn find_similar(&self, embedding: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>>;

    /// Find similar with label filter
    fn find_similar_with_label(
        &self,
        embedding: &[f32],
        k: usize,
        label: &str
    ) -> Result<Vec<(NodeId, f32)>>;

    /// Find similar at specific point in time
    fn find_similar_at_time(
        &self,
        embedding: &[f32],
        k: usize,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>>;
}

// Hybrid queries
pub trait HybridOps: GraphOps + VectorOps + TemporalOps {
    /// Traverse graph, then rank by similarity
    fn traverse_and_rank(
        &self,
        start: NodeId,
        edge_label: &str,
        target_embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>>;

    /// Semantic time-travel
    fn semantic_evolution(
        &self,
        node_id: NodeId,
        time_range: TimeRange,
    ) -> Result<Vec<(Timestamp, Arc<[f32]>)>>;
}
```

## Implementation Plan

### Phase 1: Vector Storage Foundation ✅ COMPLETE
**Implemented in**: PR #138

**Accomplished**:
- ✅ Added `PropertyValue::Vector(Arc<[f32]>)` variant
- ✅ Implemented binary serialization/deserialization for vectors
- ✅ Full vector math module with:
  - `cosine_similarity()`, `cosine_similarity_normalized()`
  - `euclidean_distance()`, `squared_euclidean_distance()`
  - `dot_product()`
  - `normalize()`, `normalize_in_place()`, `magnitude()`
  - `validate_vector()`, `check_dimensions_match()`
- ✅ `PropertyMapBuilder::insert_vector()` API
- ✅ Comprehensive unit tests

**Files modified**:
- `src/core/property.rs` - Added Vector variant
- `src/core/vector.rs` - NEW: Vector utilities module
- `src/core/mod.rs` - Export vector module

### Phase 2: HNSW Index Integration ✅ COMPLETE (Milestone M2)
**Implemented in**: Milestone M2 (PR #169 + M2 completion work)

**Accomplished**:
- ✅ Integrated usearch crate via HnswIndex wrapper
- ✅ Created VectorIndex trait and HnswIndex implementation
- ✅ VectorIndexState integrated into CurrentStorage with parking_lot::RwLock
- ✅ Automatic vector indexing on CRUD operations:
  - `create_node()` - auto-index with rollback on failure
  - `update_node()` - auto-update with rollback on failure
  - `delete_node()` - auto-remove (best-effort)
- ✅ k-NN query methods with label filtering
- ✅ **Index configuration persistence (VS-032)** - V2 checkpoint format with vector index config
- ✅ **HNSW benchmarks (VS-033)** - Comprehensive benchmark suite for all operations
- ✅ **Phase 2 integration tests (VS-034)** - 27 total tests covering all functionality
- ✅ **Documentation (VS-035)** - Integration, performance, and troubleshooting guides

**Files created/modified**:
- `src/index/vector.rs` - VectorIndex trait + HnswIndex implementation
- `src/index/vector/hnsw.rs` - HNSW wrapper with serialization
- `src/storage/current.rs` - VectorIndexState integration + query methods
- `src/storage/persistence.rs` - V2 checkpoint format with vector index config
- `src/db.rs` - Public API exposure
- `src/utils/error.rs` - Added PropertyNotFound error
- `benches/hnsw_index.rs` - NEW: Comprehensive HNSW benchmarks
- `tests/vector_storage.rs` - Extended with Phase 2 integration tests
- `docs/guides/vector-search-integration.md` - NEW: Integration guide
- `docs/guides/vector-search-performance.md` - NEW: Performance tuning guide
- `docs/guides/vector-search-troubleshooting.md` - NEW: Troubleshooting guide

**Implemented API**:
```rust
impl GallifreyDB {
    pub fn enable_vector_index(&self, property_name: &str, config: HnswConfig) -> Result<()>;
    pub fn is_vector_index_enabled(&self) -> bool;
    pub fn find_similar(&self, query_node_id: NodeId, k: usize) -> Result<Vec<(NodeId, f32)>>;
    pub fn find_similar_with_label(&self, query_node_id: NodeId, label: &str, k: usize) -> Result<Vec<(NodeId, f32)>>;
    pub fn find_similar_by_embedding(&self, query_embedding: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>>;
}
```

**Key Design Decisions**:
- Uses `parking_lot::RwLock` for efficient read-heavy access
- Arc-wrapped HnswIndex enables lock-free cloning before expensive operations
- Query node is excluded from results (searches k+1, filters, truncates to k)
- Label filtering uses GLOBAL_INTERNER for efficient string comparison
- Index configuration persisted in V2 checkpoint format for recovery
- Comprehensive benchmarks cover: index creation, single/batch add, k-NN search, parameter tuning
- Integration tests validate: lifecycle, search, updates, errors, concurrency

**Performance Characteristics** (384-dim vectors, M=16):
- Index creation: ~755ns
- Single vector add: ~8-12µs (with existing index)
- k-NN search (k=10, 1k vectors): ~2-4µs
- Memory overhead: ~1KB per vector

### Phase 3: Temporal Vector-Historical Integration ✅ Complete

**Issue**: #67 (VS-047) - Integrate Temporal Vectors with HistoricalStorage
**Completed**: 2026-01-08
**Implementation**: Hybrid Pre-Anchor Hooks + Post-Commit Observers

**Goals Achieved**:
- ✅ Vector snapshot creation synchronized with graph anchors
- ✅ Provenance tracking: `anchor.vector_snapshot_id → temporal_index.snapshot(id)`
- ✅ Strong consistency (snapshot IDs stored atomically)
- ✅ Observer extensibility (metrics, logging, future indexes)
- ✅ Graceful degradation (hook failures don't block anchors)

**Architecture Pattern**:
- **Pre-anchor hooks**: Fire BEFORE anchor storage, return snapshot IDs → strong consistency
- **Post-commit observers**: Fire AFTER storage for metrics/logging → extensibility

**Key Implementation**:
```rust
// GallifreyDB.enable_temporal_vector_index() registers both:
pub fn enable_temporal_vector_index(&self, property_name: &str, config: TemporalVectorConfig) -> Result<()> {
    // 1. Create temporal vector index
    self.current.enable_temporal_vector_index(property_name, config)?;
    let temporal_index = self.current.get_temporal_vector_index().ok_or(...)?;

    // 2. Register pre-anchor hooks (strong consistency)
    // Both node and edge hooks perform the same action, so we create one and clone it
    let hook: PreAnchorHook = {
        let index = Arc::clone(&temporal_index);
        Arc::new(move |_entity_type, _entity_id, timestamp, _properties| {
            index.create_snapshot_for_anchor(timestamp)  // Returns Option<snapshot_id>
        })
    };
    historical.register_pre_node_anchor_hook(Arc::clone(&hook));
    historical.register_pre_edge_anchor_hook(hook);

    // 3. Register observer (extensibility)
    let observer = VectorIndexObserver::new(temporal_index);
    historical.add_observer(Arc::new(observer));

    Ok(())
}
```

**Provenance Tracking**:
- Every graph anchor stores `vector_snapshot_id` atomically
- Enables reconstruction: anchor → snapshot ID → temporal vector state
- 1:1 alignment: anchor interval matches snapshot creation

**Test Coverage**:
- 6 unit tests in `src/storage/historical.rs` (hook behavior)
- 5 integration tests in `tests/temporal_vector_integration.rs`
- All 684 tests pass, clippy clean

**Documentation**:
- ADR-0018: Complete architecture and design decisions
- Updated CLAUDE.md with integration guide
- Module-level documentation in observer.rs

**Files Modified**:
- `src/storage/historical.rs` - PreAnchorHook infrastructure, hook calls in add_*_version()
- `src/db.rs` - Hook and observer registration in enable_temporal_vector_index()
- `src/storage/observer.rs` - Module docs explaining hook vs observer patterns
- `docs/adr/0018-temporal-vector-historical-integration.md` - NEW ADR

### Phase 4: Hybrid Query Engine
**Estimated effort**: 2-3 days

**Goals**:
- Graph + Vector queries
- Vector + Temporal queries
- Full hybrid: Graph + Vector + Temporal
- Query planner for optimal execution

**Example queries**:
```rust
// Graph + Vector: "Who does Alice know that's similar to Bob?"
db.traverse(alice_id, "KNOWS")
  .rank_by_similarity(bob_embedding, 10)

// Vector + Temporal: "What was similar to this concept in 2023?"
db.as_of(timestamp_2023)
  .find_similar(concept_embedding, 10)

// Full hybrid: "Who did Alice know in 2023 that was similar to Bob?"
db.as_of(timestamp_2023)
  .traverse(alice_id, "KNOWS")
  .rank_by_similarity(bob_embedding, 10)
```

### Phase 5: Persistence & Performance
**Estimated effort**: 2-3 days

**Goals**:
- Persist vector indexes to disk
- Incremental index updates (avoid full rebuilds)
- Benchmark suite for vector operations
- Performance optimization

**Targets**:
- Vector search: <10ms for 1M vectors
- Index update: <1ms per vector
- Storage overhead: <20% for index structures

## Module Structure

```
src/
├── core/
│   ├── property.rs      # Add Vector variant
│   └── vector.rs        # NEW: Vector utilities
├── index/
│   ├── vector.rs        # NEW: VectorIndex trait + impl
│   ├── vector/
│   │   ├── hnsw.rs      # NEW: HNSW wrapper
│   │   └── temporal.rs  # NEW: Temporal vector index
│   └── mod.rs           # Export new modules
├── storage/
│   └── current.rs       # Vector query methods
└── query/               # NEW: Query planning
    ├── mod.rs
    ├── planner.rs       # Query optimization
    └── hybrid.rs        # Hybrid query execution
```

## SUPERRAG Query Examples

### Example 1: Knowledge Evolution
```
User: "How has our understanding of 'machine learning' evolved?"

Query:
db.find_nodes_with_label("Concept")
  .filter(|n| n.name == "machine learning")
  .semantic_evolution(TimeRange::all())
  .with_related_concepts(depth: 2)
```

### Example 2: Temporal Semantic Search
```
User: "What did we know about quantum computing in 2020
       that's similar to today's AI safety concerns?"

Query:
let ai_safety_embedding = db.get_embedding("AI safety concerns");
db.as_of(timestamp_2020)
  .find_nodes_with_label("Concept")
  .filter(|n| n.domain == "quantum computing")
  .rank_by_similarity(ai_safety_embedding, 10)
  .with_provenance()
```

### Example 3: Relationship Discovery
```
User: "Who influenced Alice's work that has similar
       research interests to Bob?"

Query:
let bob_interests = db.get_node(bob_id).embedding;
db.traverse(alice_id, "INFLUENCED_BY")
  .rank_by_similarity(bob_interests, 5)
  .include_path()
```

### Example 4: Contradiction Detection
```
User: "Find facts that changed meaning over time"

Query:
db.find_nodes_with_label("Fact")
  .where_semantic_drift_exceeds(threshold: 0.3)
  .between(time_start, time_end)
  .with_version_history()
```

## Performance Considerations

### Memory Budget
- HNSW index: ~1KB per vector (for 384-dim embeddings)
- 1M nodes with embeddings: ~1GB index memory
- Temporal snapshots: multiply by snapshot count

### CPU Considerations
- Index building: O(n log n) for HNSW
- Query: O(log n) average case
- Batch operations preferred for index updates

### Storage Format
```
Vector Index File (.vidx):
┌─────────────────────────────────┐
│ Header (version, dimensions)    │
├─────────────────────────────────┤
│ HNSW Graph Structure            │
├─────────────────────────────────┤
│ Vector Data (memory-mapped)     │
├─────────────────────────────────┤
│ NodeId → Vector Offset Map      │
└─────────────────────────────────┘
```

## Testing Strategy

### Unit Tests
- Vector similarity calculations
- PropertyValue::Vector serialization
- Index add/remove/query operations

### Integration Tests
- End-to-end vector search
- Temporal vector queries
- Hybrid graph+vector queries

### Benchmarks
```rust
// benches/vector_search.rs
fn bench_knn_search(c: &mut Criterion) {
    let db = setup_db_with_vectors(1_000_000);
    let query = random_embedding(384);

    c.bench_function("knn_k10", |b| {
        b.iter(|| db.find_similar(&query, 10))
    });
}

fn bench_temporal_vector_search(c: &mut Criterion) {
    let db = setup_temporal_db_with_vectors();
    let query = random_embedding(384);
    let timestamp = historical_timestamp();

    c.bench_function("temporal_knn_k10", |b| {
        b.iter(|| db.as_of(timestamp).find_similar(&query, 10))
    });
}
```

## Dependencies to Add

```toml
# Cargo.toml additions

[dependencies]
# Vector index (choose one)
usearch = "2"           # Recommended: fast, filtering support
# hora = "0.1"          # Alternative: pure Rust

# Optional: for vector normalization
ndarray = "0.15"        # If we need matrix operations

[dev-dependencies]
rand = "0.8"            # For generating test vectors
```

## Open Questions

1. **Embedding generation**: Should GallifreyDB generate embeddings or expect them as input?
   - Recommendation: Accept embeddings as input; generation is application-specific

2. **Sparse vectors**: Support for sparse embeddings (e.g., BM25, SPLADE)?
   - Recommendation: Add later as `PropertyValue::SparseVector`

3. **Multi-vector nodes**: Can a node have multiple embeddings (e.g., title + content)?
   - Recommendation: Yes, as separate properties with naming convention

4. **Index sharding**: How to handle very large vector collections?
   - Recommendation: Single index for MVP; add sharding in future

5. **Consistency**: How to keep vector index in sync with storage?
   - Recommendation: Synchronous updates for current index; async for temporal snapshots

## Success Criteria

1. **Functional**: All four phases implemented and tested
2. **Performance**:
   - Vector search <10ms for 1M vectors
   - No regression in graph/temporal query performance
3. **Integration**: Seamless API combining all three capabilities
4. **Documentation**: Updated CLAUDE.md with vector guidelines

## References

- [HNSW Paper](https://arxiv.org/abs/1603.09320)
- [usearch Documentation](https://github.com/unum-cloud/usearch)
- [Vector Database Benchmarks](https://ann-benchmarks.com/)
- [Temporal + Vector Research](https://arxiv.org/abs/2304.12212) (AeonG paper)
