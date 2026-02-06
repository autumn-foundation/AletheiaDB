# Vector Search Integration Design

> **Status**: Phase 4 Complete (VS-072)
> **Created**: 2024-12-30
> **Updated**: 2026-01-14
> **Goal**: Position AletheiaDB as SUPERRAG - Graph + Vector + Bi-temporal
>
> ## Implementation Progress
>
> | Phase | Status | Description |
> |-------|--------|-------------|
> | Phase 1 | ✅ Complete (PR #138) | Vector storage foundation (`PropertyValue::Vector`, similarity functions) |
> | Phase 2 | ✅ Complete (Milestone M2) | HNSW index integration, benchmarks, tests, documentation |
> | Phase 3 | ✅ Complete (Issue #67) | Temporal vector-historical integration, provenance tracking |
> | Phase 4 | ✅ Complete (Issue #85) | Hybrid query engine with unified API |
> | Phase 4.1 | ✅ Complete (Issue #389) | Multi-property vector index support (ADR-0022) |
> | Phase 5 | 🔲 Planned | Persistence & performance optimization |

## Executive Summary

Adding vector search to AletheiaDB enables the combination of **graph traversal**, **semantic similarity**, and **bi-temporal tracking**. This enables queries like "What did we know about X that was semantically similar to Y at time T?" - essential for LLM reasoning about knowledge evolution.

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

### Decision 5: Multi-Property Vector Index Support

**Implemented in**: Issue #389, ADR-0022

Support multiple vector properties per database, each with independent HNSW indexes:

```rust
// Storage: DashMap for concurrent multi-property access
struct CurrentStorage {
    vector_indexes: DashMap<String, VectorIndexEntry>,
    temporal_vector_indexes: DashMap<String, TemporalVectorIndexEntry>,
}

// API: Property-specific methods use `_in` suffix
db.find_similar_in("content_embedding", node_id, 10)?;
db.find_similar_by_embedding_in("title_embedding", &query, 10)?;
db.rank_by_similarity_in("content_embedding", node_ids, &query, 10)?;

// QueryBuilder: Property selection via builder pattern
db.query()
    .find_similar_builder(&embedding, 10)
    .property("content_embedding")
    .finish()
    .execute(&db)?;
```

**Key Decisions**:
- Use `DashMap<String, VectorIndexEntry>` for lock-free concurrent property access
- Property-specific methods use `_in` suffix for explicit property selection
- Default methods (no suffix) use "embedding" property for backwards compatibility
- Physical operators include `property_key: Option<String>` for multi-property execution
- Temporal indexes also support multi-property via same DashMap pattern

See [ADR-0022](docs/adr/0022-multi-property-vector-index.md) for complete architecture details.

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
impl AletheiaDB {
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
// AletheiaDB.enable_temporal_vector_index() registers both:
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

#### Temporal Vector Implementation Details

**Architecture: Dual-Path Design**

The temporal vector index uses a hybrid architecture mirroring AletheiaDB's current/historical storage split:

```
┌─────────────────────────────────────────────────────┐
│           Query Engine (Coordinator)                 │
└─────────────────────────────────────────────────────┘
         │
    ┌────┴────┐
    │          │
Current HNSW  Temporal HNSW Snapshots
(Live index)   (Historical snapshots at configurable intervals)
```

**Core Data Structures**:

1. **TemporalVectorIndex**: Main coordinator containing:
   - `current_index`: Arc<HnswIndex> - Live index for present-time queries
   - `snapshots`: Arc<RwLock<BTreeMap<Timestamp, VectorSnapshot>>> - Historical snapshots
   - `transaction_count`: AtomicUsize - Tracks operations for interval-based snapshots
   - `deleted_nodes`: Arc<DashMap<NodeId, Timestamp>> - Soft deletes (HNSW limitation)

2. **VectorSnapshot** (enum):
   ```rust
   enum VectorSnapshot {
       Full(Arc<HashMap<NodeId, Arc<[f32]>>>),
       Delta {
           base_time: Timestamp,
           added: Arc<HashMap<NodeId, Arc<[f32]>>>,
           removed: Arc<HashSet<NodeId>>,
       },
   }
   ```

**Snapshot Policies**

The `SnapshotStrategy` enum determines when snapshots are created:

| Strategy | Description | Use Case | Trigger Logic |
|----------|-------------|----------|---------------|
| `TransactionInterval(N)` | Every N write operations | Predictable overhead | `transaction_count % N == 0` |
| `TimeInterval(secs)` | Fixed time intervals | Time-based queries | `current_time - last_snapshot >= secs` |
| `ChangeThreshold(pct)` | When X% vectors change | Write-heavy workloads | `changed_vectors / total_vectors >= pct` |
| `Hybrid{tx, time, change}` | Whichever fires first | Balanced approach | Any trigger condition met |

**Default**: `TransactionInterval(10)` - Balances overhead vs temporal granularity.

**Retention Policies**

The `RetentionPolicy` enum controls snapshot pruning:

| Policy | Description | Memory Impact |
|--------|-------------|---------------|
| `KeepAll` | No pruning | Unbounded growth |
| `KeepN(count)` | Keep N most recent | Bounded: N × snapshot_size |
| `KeepDuration(d)` | Time-based retention | Bounded: depends on write rate |

**Default**: `KeepN(100)` - ~100 snapshots for typical workloads.

**Delta Snapshot Optimization**

To reduce memory and creation time, we alternate between Full and Delta snapshots:

- **Full Snapshot**: Complete HNSW index built from all vectors
  - Created every `full_snapshot_interval` snapshots (default: 10)
  - Creation time: O(N log N) for N vectors
  - Memory: ~2.5KB per vector (1.5KB data + ~1KB HNSW structure)

- **Delta Snapshot**: Only changed vectors since last Full snapshot
  - Created between Full snapshots
  - Creation time: O(M log M) for M changed vectors
  - Memory: ~2.5KB per changed vector

**Query Processing**:
- Point-in-time queries reconstruct state by merging delta + base
- Maximum delta chain depth: 10 (enforced via `MAX_DELTA_CHAIN_DEPTH`)
- Deduplication ensures correct results (delta additions override base)

**Example**:
```
Snapshot Timeline:
T0  : Full (10k vectors)       - 25MB
T10 : Delta (+100, -50)        - 375KB
T20 : Delta (+200, -100)       - 750KB
T30 : Full (10,150 vectors)    - 25MB
...

Query at T20: Merge Full@T0 + Delta@T10 + Delta@T20 = 10,150 vectors
```

**Semantic Drift Tracking**

Drift tracking identifies nodes whose embeddings have changed significantly over time.

**Metrics** (`DriftMetric` enum):

| Metric | Formula | Range | Use Case |
|--------|---------|-------|----------|
| `Cosine` | `1.0 - cosine_similarity(v1, v2)` | [0, 2] | Semantic similarity (ignores magnitude) |
| `Euclidean` | `sqrt(Σ(v1[i] - v2[i])²)` | [0, ∞) | Spatial distance (sensitive to magnitude) |
| `Angular` | `arccos(cosine_similarity(v1, v2))` | [0, π] | Geometric angle in radians |

**API Methods**:

1. **Global Drift Detection**: Find all nodes exceeding drift threshold
   ```rust
   pub fn find_semantic_drift(
       &self,
       threshold: f32,
       time_range: TimeRange,
       metric: DriftMetric,
   ) -> Result<Vec<(NodeId, f32)>>
   ```

2. **Per-Node Drift Tracking**: Track drift timeline for specific node
   ```rust
   pub fn track_semantic_drift(
       &self,
       node_id: NodeId,
       reference_embedding: &[f32],
       time_range: TimeRange,
   ) -> Result<Vec<(Timestamp, f32)>>
   ```

**Example Use Cases**:
- **Content Versioning**: Detect when document summaries diverge from original
- **Knowledge Evolution**: Track how concept definitions evolve
- **Anomaly Detection**: Identify sudden semantic shifts
- **LLM Reasoning**: Understand when/why understanding changed

**Performance Characteristics**

| Operation | Complexity | Target | Actual (1M vectors) |
|-----------|------------|--------|---------------------|
| Full snapshot creation | O(N log N) | <1s | ~950ms |
| Delta snapshot creation | O(M log M) | <100ms | ~50ms (M=1000) |
| Point-in-time query | O(log N) | <10ms | ~4-8ms |
| Range query (K snapshots) | O(K × log N) | <100ms | ~40-80ms (K=10) |
| Drift detection | O(S × N) | <50ms | ~30ms (S=5 snapshots) |

**Memory Budget**:
- Small DB (10K vectors, 10 snapshots): ~100MB
- Medium DB (100K vectors, 50 snapshots): ~5GB
- Large DB (1M vectors, 100 snapshots): ~100GB

**Storage Overhead**:
- Per vector: ~2.5KB (1.5KB raw + ~1KB HNSW)
- Per full snapshot: ~2.5GB for 1M vectors
- Per delta snapshot: Proportional to changes

**Temporal Vector API Reference**

```rust
// Configuration
pub struct TemporalVectorConfig {
    pub snapshot_strategy: SnapshotStrategy,
    pub retention_policy: RetentionPolicy,
    pub max_snapshots: usize,
    pub full_snapshot_interval: usize,
    pub hnsw_config: HnswConfig,
}

// Vector Management
impl TemporalVectorIndex {
    pub fn new(config: TemporalVectorConfig) -> Result<Self>;
    pub fn add(&self, node_id: NodeId, vector: &[f32], timestamp: Timestamp) -> Result<()>;
    pub fn remove(&self, node_id: NodeId, timestamp: Timestamp) -> Result<()>;

    // Snapshot Control
    pub fn on_transaction(&self) -> Result<()>;
    pub fn on_transaction_at(&self, timestamp: Timestamp) -> Result<()>;
    pub fn create_snapshot_for_anchor(&self, timestamp: Timestamp) -> Result<Option<usize>>;

    // Temporal Queries
    pub fn find_similar_as_of(
        &self,
        query: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>>;

    pub fn find_similar_in_range(
        &self,
        query: &[f32],
        k: usize,
        time_range: TimeRange,
    ) -> Result<TemporalSearchResults>;

    // Drift Tracking
    pub fn find_semantic_drift(
        &self,
        threshold: f32,
        time_range: TimeRange,
        metric: DriftMetric,
    ) -> Result<Vec<(NodeId, f32)>>;

    pub fn track_semantic_drift(
        &self,
        node_id: NodeId,
        reference_embedding: &[f32],
        time_range: TimeRange,
    ) -> Result<Vec<(Timestamp, f32)>>;

    // Utilities
    pub fn snapshot_count(&self) -> usize;
    pub fn dimensions(&self) -> usize;
    pub fn distance_metric(&self) -> DistanceMetric;
    pub fn get_snapshot_info(&self) -> Result<Vec<SnapshotInfo>>;
}
```

**Usage Examples**

**Example 1: Point-in-Time Semantic Search**
```rust
use aletheiadb::index::vector::temporal::{TemporalVectorIndex, TemporalVectorConfig};
use aletheiadb::index::vector::{HnswConfig, DistanceMetric};

// Configure temporal index
let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
let config = TemporalVectorConfig {
    snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
    retention_policy: RetentionPolicy::KeepN(100),
    max_snapshots: 100,
    full_snapshot_interval: 10,
    hnsw_config: Some(hnsw_config),
};

let index = TemporalVectorIndex::new(config)?;

// Add vectors over time
let doc1_v1 = vec![0.1, 0.2, 0.3, /* ... 381 more */];
index.add(doc1_id, &doc1_v1, timestamp_2023)?;

let doc1_v2 = vec![0.15, 0.25, 0.35, /* ... 381 more */];
index.add(doc1_id, &doc1_v2, timestamp_2024)?;

// Query at specific point in time
let query_embedding = vec![0.12, 0.22, 0.32, /* ... 381 more */];
let results = index.find_similar_as_of(&query_embedding, 10, timestamp_2023)?;
// Returns results using doc1_v1 (state as of 2023)
```

**Example 2: Knowledge Evolution Tracking**
```rust
use aletheiadb::core::temporal::TimeRange;
use aletheiadb::index::vector::temporal::DriftMetric;

// Track how "AI Safety" concept embedding evolved
let reference_embedding = current_ai_safety_embedding;
let time_range = TimeRange::new(timestamp_2020, timestamp_2025);

let drift_timeline = index.track_semantic_drift(
    ai_safety_node_id,
    &reference_embedding,
    time_range,
)?;

for (timestamp, drift_score) in drift_timeline {
    println!("At {}: drift = {:.3}", timestamp, drift_score);
}
// Output:
// At 2020-01-01: drift = 0.000
// At 2021-06-15: drift = 0.124
// At 2022-12-01: drift = 0.456  <- Significant shift!
```

**Example 3: Detect Semantic Drift Across Corpus**
```rust
// Find all documents that changed meaning significantly between 2023-2024
let time_range = TimeRange::new(timestamp_2023_01, timestamp_2024_12);
let drifted_docs = index.find_semantic_drift(
    0.3,  // Threshold: cosine distance > 0.3
    time_range,
    DriftMetric::Cosine,
)?;

for (node_id, drift_score) in drifted_docs {
    println!("Document {} drifted by {:.3}", node_id, drift_score);
}
// Identify contradictions, updates, or evolving understanding
```

**Example 4: Integration with Graph Anchors (VS-047)**
```rust
use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;

let db = AletheiaDB::new();

// Enable temporal vector indexing (registers hooks + observers)
let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);
db.enable_temporal_vector_index("embedding", temporal_config)?;

// Create/update nodes - vector snapshots created automatically
for i in 0..20 {
    db.update_node(doc_id, PropertyMapBuilder::new()
        .insert_vector("embedding", &embeddings[i])
        .build()
    )?;
}

// Graph anchors at v0, v10, v20 each have vector snapshot IDs
// Provenance: anchor.vector_snapshot_id → temporal_index.snapshot(id)
```

**Safety and Correctness Guarantees**

1. **Delta Chain Safety**:
   - `MAX_DELTA_CHAIN_DEPTH = 10` prevents unbounded traversal
   - `MAX_ACCUMULATED_CHANGES = 100,000` forces full snapshot at scale
   - Iterative traversal (no recursion) prevents stack overflow
   - Errors returned instead of silent data loss

2. **Vector Validation**:
   - NaN/Infinity checking via `validate_vector()`
   - Dimension mismatch enforcement
   - k parameter capped at `MAX_K = 10,000` (DoS prevention)

3. **Soft Deletes**:
   - HNSW doesn't support deletion, so deleted IDs tracked separately
   - Prevents ghost results from deleted nodes
   - Cleanup on snapshot creation

4. **Thread Safety**:
   - `Arc<RwLock<_>>` for snapshot map (read-heavy workload)
   - `AtomicUsize` for transaction counter
   - `DashMap` for soft deletes (concurrent access)

### Phase 4: Hybrid Query Engine ✅ Complete

**Issue**: #85 (VS-072) - Phase 4 Documentation
**Completed**: 2026-01-14
**Implementation**: Pull-Based Iterator Query Planner with Cost-Based Optimization

**Goals Achieved**:
- ✅ Graph + Vector queries via `traverse_and_rank()`
- ✅ Vector + Temporal queries via `find_similar_as_of()`
- ✅ Full hybrid: Graph + Vector + Temporal
- ✅ Type-safe fluent query builder API
- ✅ Cost-based query optimization
- ✅ Comprehensive benchmarks across topologies

**Architecture**:

```
┌──────────────────────────────────────────────────────────────┐
│                      Query Builder API                        │
│  QueryBuilder<S> with compile-time state tracking             │
└──────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────┐
│                  Query IR (Intermediate Rep)                  │
│  QueryOp: StartNode | Traverse | VectorSearch | RankBy | ... │
└──────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────┐
│                     Logical Planner                           │
│  - Operation validation       - Tree construction             │
│  - Temporal context binding   - Cardinality estimation        │
└──────────────────────────────────────────────────────────────┘
                              │
              ┌───────────────┴───────────────┐
              │   Optimization Rules          │
              │ - Predicate pushdown          │
              │ - Limit pushdown              │
              │ - Vector operation reordering │
              └───────────────┬───────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────┐
│                    Physical Planner                           │
│  LogicalOp → PhysicalOp (cost-based operator selection)       │
└──────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────┐
│                     Query Executor                            │
│  Pull-based iterators: NodeLookup | HnswSearch | Traverse... │
└──────────────────────────────────────────────────────────────┘
```

#### Three-Layer API Design

The hybrid query API provides three complementary access patterns:

**Layer 1: Direct Functions** (simple patterns)
```rust
use aletheiadb::query::hybrid::{traverse_and_rank, find_similar_as_of};

// Graph + Vector: Find neighbors ranked by similarity
let results = traverse_and_rank(&db, alice_id, "KNOWS", &query_embedding, 10)?;

// Temporal + Vector: Point-in-time semantic search
let results = find_similar_as_of(&db, &query_embedding, 10, timestamp)?;
```

**Layer 2: Fluent Query Builder** (complex compositions)
```rust
use aletheiadb::query::QueryBuilder;
use aletheiadb::query::ir::Predicate;

// Graph + Vector: "Who does Alice know that's similar to Bob?"
let results = db.query()
    .start(alice_id)
    .traverse("KNOWS")
    .rank_by_similarity(&bob_embedding, 10)
    .execute(&db)?;

// Vector + Temporal: "What was similar to this concept in 2023?"
let results = db.query()
    .as_of(valid_time_2023, tx_time_2023)
    .find_similar(&concept_embedding, 10)
    .execute(&db)?;

// Full Hybrid: "Who did Alice know in 2023 that was similar to Bob?"
let results = db.query()
    .as_of(valid_time_2023, tx_time_2023)
    .start(alice_id)
    .traverse("KNOWS")
    .rank_by_similarity(&bob_embedding, 10)
    .filter(Predicate::gt("score", 0.8))
    .limit(5)
    .with_provenance()
    .execute(&db)?;
```

**Layer 3: Database Convenience Methods** (quick access)
```rust
// Direct convenience methods on AletheiaDB
let similar = db.traverse_and_rank(alice_id, "KNOWS", &embedding, 10)?;
let temporal_similar = db.find_similar_at_time(&embedding, 10, valid_time, tx_time)?;
```

#### Query Builder State Machine

The builder uses phantom types for compile-time safety:

```
Initial ──┬─ start(NodeId) ──────────────┬─► HasNodes
          │                              │
          ├─ start_from(Vec<NodeId>) ────┤
          │                              │
          ├─ scan(label) ────────────────┤
          │                              │
          └─ find_similar(&emb, k) ──────┴─► HasVectorResults

HasNodes ──┬─ traverse("LABEL") ─────────┬─► HasTraversalResults
           │                             │
           ├─ traverse_n("LABEL", depth) ┤
           │                             │
           ├─ traverse_in("LABEL") ──────┤
           │                             │
           ├─ traverse_both("LABEL") ────┤
           │                             │
           ├─ rank_by_similarity(&e, k) ─┴─► HasVectorResults
           │
           └─ similar_to(node, k) ───────────► HasVectorResults

HasTraversalResults ─┬─ traverse("LABEL") ───► HasTraversalResults
                     │
                     └─ rank_by_similarity() ─► HasVectorResults

HasVectorResults ────┬─ traverse("LABEL") ───► HasTraversalResults
                     │
                     └─ filter() ────────────► HasVectorResults

Any State: as_of() | between() | limit() | skip() | with_hint() |
           parallel() | with_provenance() | build() | execute()
```

**Invalid queries fail at compile time**:
```rust
// ERROR: Cannot traverse without a node source
let query = QueryBuilder::new().traverse("KNOWS"); // Won't compile

// ERROR: Cannot call rank_by_similarity on Initial state
let query = QueryBuilder::new().rank_by_similarity(&emb, 10); // Won't compile
```

#### Query Operations (IR)

| Operation | Description | State Transition |
|-----------|-------------|------------------|
| `StartNode(id)` | Start from single node | Initial → HasNodes |
| `StartNodes(ids)` | Start from multiple nodes | Initial → HasNodes |
| `ScanNodes { label }` | Scan all nodes (±filter) | Initial → HasNodes |
| `VectorSearch { embedding, k, metric }` | k-NN search | Initial → HasVectorResults |
| `TraverseOut { label, depth }` | Outgoing edges | HasNodes → HasTraversalResults |
| `TraverseIn { label, depth }` | Incoming edges | HasNodes → HasTraversalResults |
| `TraverseBoth { label, depth }` | Both directions | HasNodes → HasTraversalResults |
| `RankBySimilarity { embedding, top_k }` | Rank by similarity | Any → HasVectorResults |
| `SimilarTo { source_node, k, ... }` | Node-based k-NN | HasNodes → HasVectorResults |
| `Filter(predicate)` | Property filter | Same state |
| `FilterLabel(label)` | Label filter | Same state |
| `Limit(n)` | Result limit | Same state |
| `Skip(n)` | Result offset | Same state |

**Traversal Depth Options**:
```rust
TraversalDepth::Exact(1)         // Exactly 1 hop
TraversalDepth::Exact(3)         // Exactly 3 hops
TraversalDepth::Max(5)           // 0..=5 hops
TraversalDepth::Range { min: 2, max: 4 }  // 2..=4 hops
TraversalDepth::Variable         // Unbounded (use with caution)
```

**Predicates for Filtering**:
```rust
Predicate::eq("name", "Alice")     // name == "Alice"
Predicate::ne("status", "deleted") // status != "deleted"
Predicate::gt("age", 18)           // age > 18
Predicate::lt("score", 0.5)        // score < 0.5
Predicate::exists("email")         // has email property
Predicate::contains("bio", "rust") // bio contains "rust"

// Logical combinations
Predicate::eq("a", 1).and(Predicate::gt("b", 2))
Predicate::eq("x", 1).or(Predicate::eq("y", 2))
!Predicate::exists("deleted_at")   // NOT exists
```

#### Physical Operators

| Operator | Description | Complexity | Target Latency |
|----------|-------------|------------|----------------|
| `NodeLookup` | O(1) DashMap lookup | O(1) | <1µs |
| `NodeScan` | Full scan ± label filter | O(N) | Variable |
| `HnswSearch` | k-NN via HNSW index | O(log N) | <10ms (1M vectors) |
| `IndexedTraversal` | CSR adjacency traversal | O(E) | <1µs/hop |
| `TemporalNodeLookup` | Point-in-time reconstruction | O(D) | <10ms |
| `VectorRerank` | Compute similarities, sort | O(N log k) | Variable |
| `Filter` | Predicate evaluation | O(1)/row | <0.1µs |
| `Limit` | Truncate result stream | O(1) | Negligible |

#### Cost Model

The planner uses calibrated cost weights for operator selection:

| Operator | CPU Cost | Memory Cost | Notes |
|----------|----------|-------------|-------|
| NodeLookup | 0.5µs | Minimal | O(1) DashMap |
| Traversal | 1.0µs/hop | Proportional to fan-out | CSR-optimized |
| HnswSearch | 0.3µs × k | Index memory | Sub-linear scaling |
| Filter | 0.1µs/row | None | Predicate eval |
| TemporalReconstruct | 10µs/delta | Delta chain memory | Anchor+delta |
| VectorRerank | 0.5µs × N | O(k) heap | Min-heap top-k |

#### Optimization Rules

**Predicate Pushdown**: Move filters closer to data sources
```
Filter(Scan(Person)) → Scan(Person, filter=predicate)
```

**Limit Pushdown**: Propagate LIMIT through compatible operators
```
Limit(Sort(Scan(...))) → Sort(Scan(..., limit=N))
```

**Vector Operation Reordering**: Execute vector searches early when beneficial
```
Traverse(VectorSearch(...)) → May reorder based on selectivity
```

#### Performance Characteristics

**Benchmark Results** (from `benches/hybrid_query.rs`):

| Operation | Scale | Topology | Latency |
|-----------|-------|----------|---------|
| `traverse_and_rank` k=10 | 1K nodes | Uniform (20 edges/node) | ~15-25µs |
| `traverse_and_rank` k=10 | 10K nodes | Power-law | ~50-100µs |
| `traverse_and_rank` k=10 | 10K nodes | Sparse (2-5 edges) | ~10-20µs |
| `find_similar_as_of` k=10 | 1K nodes | 50 snapshots | ~100-200µs |
| Multi-hop ranked traversal | 1K nodes | Uniform | ~40-80µs |
| Full hybrid (temporal) | 1K nodes | 20 snapshots | ~150-300µs |

**Scaling Characteristics**:
- k-value: Near-linear scaling (min-heap maintains O(N log k))
- Dimensions: Linear scaling with vector dimension
- Temporal depth: Sublinear with anchor caching

**Hybrid vs Sequential Comparison**:
| Approach | Description | Relative Performance |
|----------|-------------|---------------------|
| Hybrid `traverse_and_rank` | Integrated operation | 1.0x (baseline) |
| Sequential traverse then rank | Separate operations | ~1.1-1.3x slower |
| Naive load-all-rank | No heap optimization | ~1.5-2x slower |

#### Query Results

```rust
pub struct QueryRow {
    pub entity: EntityResult,           // Node, Edge, or ID-only
    pub score: Option<f32>,             // Vector similarity score
    pub path: Option<Vec<EntityId>>,    // Traversal path
    pub timestamp: Option<Timestamp>,   // Temporal context
}

pub enum EntityResult {
    Node(Node),       // Full node with properties
    Edge(Edge),       // Full edge data
    NodeId(NodeId),   // ID-only (efficiency)
    EdgeId(EdgeId),   // ID-only
}

// Iterate results
let results = query.execute(&db)?;
for row in results {
    let row = row?;
    println!("Found: {:?}, score: {:?}", row.entity, row.score);
}
```

#### Files Implemented

```
src/query/
├── mod.rs              # Module exports and documentation
├── ir.rs               # QueryOp, Predicate, TraversalDepth
├── plan.rs             # LogicalPlan, LogicalOp, TemporalContext
├── builder.rs          # QueryBuilder<S> with type-state pattern
├── hybrid.rs           # traverse_and_rank, find_similar_as_of
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

**Test Coverage**:
- 38 unit tests in `src/query/hybrid.rs`
- Integration tests in `tests/hybrid_query.rs`
- Comprehensive benchmarks in `benches/hybrid_query.rs`

**Documentation**:
- [ADR-0019: Hybrid Query Planner](docs/adr/0019-hybrid-query-planner.md)
- [ADR-0021: Hybrid Query Execution Engine](docs/adr/0021-hybrid-query-execution.md)
- [Hybrid Query User Guide](docs/guides/hybrid-query-guide.md)

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

1. **Embedding generation**: Should AletheiaDB generate embeddings or expect them as input?
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
