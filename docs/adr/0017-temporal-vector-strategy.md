# ADR-0017: Temporal Vector Index Strategy

**Status:** Proposed
**Date:** 2026-01-05
**Deciders:** GallifreyDB Core Team
**Categories:** index, vector, temporal

## Context

With Phase 2 complete (HNSW index for current-state vector search), we now need to enable temporal vector queries - the ability to perform semantic similarity searches at any point in time. This is critical for GallifreyDB's SUPERRAG vision: enabling LLMs to reason about how knowledge evolved semantically over time.

### Use Cases

1. **Semantic time-travel**: "What documents were similar to this concept in 2023?"
2. **Knowledge drift tracking**: "How has the meaning of 'AI safety' evolved over the past 5 years?"
3. **Provenance-aware similarity**: "Find similar nodes with high confidence at time T"
4. **Temporal hybrid queries**: "Who did Alice know in 2023 that had similar research interests to Bob?"

### Technical Challenges

1. **HNSW is not temporal**: The HNSW index is a mutable graph structure optimized for current state
2. **Reconstruction cost**: Building an HNSW index at query time would be prohibitively expensive (O(n log n))
3. **Storage efficiency**: We cannot maintain a full HNSW index for every version (memory explosion)
4. **Query latency**: Must maintain <10ms target for temporal vector queries

### Existing Architecture Patterns

GallifreyDB's hybrid storage architecture provides a proven pattern:
- **Current Storage**: Optimized for zero-overhead current-state queries
- **Historical Storage**: Uses anchor+delta compression (every 10 versions)
- **Immutable history**: Historical data never changes, enabling aggressive caching

## Decision

We will implement a **snapshot-based temporal vector index strategy** that mirrors GallifreyDB's existing anchor+delta pattern, adapted for HNSW indexes.

### Architecture: Dual-Path Vector Indexing

```
┌─────────────────────────────────────────────────────────┐
│                 Vector Query Engine                      │
│   Current Search │ Temporal Search │ Range Search       │
└─────────────────────────────────────────────────────────┘
          │                 │                  │
    ┌─────▼─────┐    ┌─────▼─────┐     ┌─────▼─────┐
    │  Current  │    │  Temporal │     │ Historical│
    │  HNSW     │    │  HNSW     │     │  Vectors  │
    │  Index    │    │ Snapshots │     │ (versions)│
    │ (live)    │    │ (anchors) │     │           │
    └───────────┘    └───────────┘     └───────────┘
          │                 │                  │
          └─────────────────┴──────────────────┘
                            │
                    ┌───────▼───────┐
                    │  Persistence  │
                    │  (checkpoints)│
                    └───────────────┘
```

### Data Structures

```rust
/// Temporal vector index manager
pub struct TemporalVectorIndex {
    /// Current (live) HNSW index - always up-to-date
    current: Arc<RwLock<HnswIndex>>,

    /// Historical HNSW snapshots at anchor timestamps
    /// Key: (transaction_time, valid_time) for bi-temporal support
    snapshots: BTreeMap<BiTemporalPoint, Arc<HnswIndex>>,

    /// Configuration
    config: TemporalVectorConfig,

    /// Metadata for snapshot management
    metadata: SnapshotMetadata,
}

/// Configuration for temporal vector indexing
pub struct TemporalVectorConfig {
    /// Snapshot creation strategy
    pub snapshot_strategy: SnapshotStrategy,

    /// Maximum number of snapshots to retain
    /// Default reduced from 100 to 20 (issue #230) to prevent excessive memory usage
    /// Each snapshot creates full HNSW index copy (~200MB for 100K vectors, 384 dims)
    pub max_snapshots: usize,  // Default: 20 (was 100)

    /// Base HNSW configuration
    pub hnsw_config: HnswConfig,
}

/// Snapshot creation strategies
pub enum SnapshotStrategy {
    /// Create snapshot every N write transactions
    /// Mirrors anchor+delta pattern (default: 10)
    TransactionInterval(usize),

    /// Create snapshot at fixed time intervals
    /// Example: daily, hourly snapshots
    TimeInterval(Duration),

    /// Create snapshot when significant changes occur
    /// Example: >10% of vectors changed
    ChangeThreshold(f64),

    /// Hybrid: use whichever trigger fires first
    Hybrid {
        transaction_interval: usize,
        time_interval: Duration,
        change_threshold: f64,
    },
}

/// Metadata for snapshot management
struct SnapshotMetadata {
    /// Last snapshot timestamp
    last_snapshot_time: Timestamp,

    /// Transaction count since last snapshot
    transactions_since_snapshot: usize,

    /// Vectors changed since last snapshot
    vectors_changed_since_snapshot: HashSet<NodeId>,

    /// Total snapshots created (for ID generation)
    total_snapshots: usize,
}

/// Bi-temporal point for precise snapshot identification
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BiTemporalPoint {
    pub transaction_time: Timestamp,
    pub valid_time: Timestamp,
}
```

### Snapshot Creation Algorithm

```rust
impl TemporalVectorIndex {
    /// Called on every vector update/insert/delete
    pub fn on_vector_change(&mut self, node_id: NodeId, timestamp: Timestamp) -> Result<()> {
        // Track change
        self.metadata.vectors_changed_since_snapshot.insert(node_id);

        // Check if snapshot needed
        if self.should_create_snapshot(timestamp)? {
            self.create_snapshot(timestamp)?;
        }

        Ok(())
    }

    fn should_create_snapshot(&self, current_time: Timestamp) -> Result<bool> {
        match &self.config.snapshot_strategy {
            SnapshotStrategy::TransactionInterval(interval) => {
                Ok(self.metadata.transactions_since_snapshot >= *interval)
            }

            SnapshotStrategy::TimeInterval(duration) => {
                let elapsed = current_time.duration_since(self.metadata.last_snapshot_time)?;
                Ok(elapsed >= *duration)
            }

            SnapshotStrategy::ChangeThreshold(threshold) => {
                let total_vectors = self.current.read().len();
                let changed = self.metadata.vectors_changed_since_snapshot.len();
                Ok((changed as f64 / total_vectors as f64) >= *threshold)
            }

            SnapshotStrategy::Hybrid { transaction_interval, time_interval, change_threshold } => {
                let by_txn = self.metadata.transactions_since_snapshot >= *transaction_interval;
                let elapsed = current_time.duration_since(self.metadata.last_snapshot_time)?;
                let by_time = elapsed >= *time_interval;
                let total = self.current.read().len();
                let changed = self.metadata.vectors_changed_since_snapshot.len();
                let by_change = (changed as f64 / total as f64) >= *change_threshold;

                Ok(by_txn || by_time || by_change)
            }
        }
    }

    fn create_snapshot(&mut self, timestamp: Timestamp) -> Result<()> {
        // Clone current HNSW index (cheap due to Arc)
        let snapshot = Arc::new(self.current.read().clone());

        // Store with bi-temporal point
        let point = BiTemporalPoint {
            transaction_time: timestamp,
            valid_time: timestamp,  // Can be overridden for retroactive updates
        };
        self.snapshots.insert(point, snapshot);

        // Update metadata
        self.metadata.last_snapshot_time = timestamp;
        self.metadata.transactions_since_snapshot = 0;
        self.metadata.vectors_changed_since_snapshot.clear();
        self.metadata.total_snapshots += 1;

        // Enforce max snapshots limit
        self.enforce_snapshot_limit()?;

        Ok(())
    }

    fn enforce_snapshot_limit(&mut self) -> Result<()> {
        while self.snapshots.len() > self.config.max_snapshots {
            // Remove oldest snapshot
            if let Some((oldest_key, _)) = self.snapshots.iter().next() {
                let key_to_remove = *oldest_key;
                self.snapshots.remove(&key_to_remove);
            }
        }
        Ok(())
    }
}
```

### Temporal Query Algorithm

```rust
impl TemporalVectorIndex {
    /// Find k-NN at specific point in time
    pub fn find_similar_as_of(
        &self,
        query_embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        // 1. Find nearest snapshot <= timestamp
        let snapshot = self.find_nearest_snapshot(timestamp)?;

        // 2. Perform k-NN search on snapshot
        let results = snapshot.search(query_embedding, k)?;

        // 3. Filter results by temporal validity
        // (nodes must have existed at the query time)
        self.filter_by_temporal_validity(results, timestamp)
    }

    /// Find k-NN across time range
    pub fn find_similar_in_range(
        &self,
        query_embedding: &[f32],
        k: usize,
        time_range: TimeRange,
    ) -> Result<Vec<(Timestamp, Vec<(NodeId, f32)>)>> {
        // Find all snapshots in range
        let snapshots = self.find_snapshots_in_range(time_range)?;

        // Query each snapshot
        let mut results = Vec::new();
        for (timestamp, snapshot) in snapshots {
            let snapshot_results = snapshot.search(query_embedding, k)?;
            let filtered = self.filter_by_temporal_validity(snapshot_results, timestamp)?;
            results.push((timestamp, filtered));
        }

        Ok(results)
    }

    /// Track semantic drift: how similarity changed over time
    pub fn track_semantic_drift(
        &self,
        node_id: NodeId,
        reference_embedding: &[f32],
        time_range: TimeRange,
    ) -> Result<Vec<(Timestamp, f32)>> {
        // Get node's embedding at each snapshot
        let mut drift_timeline = Vec::new();

        for (timestamp, snapshot) in self.find_snapshots_in_range(time_range)? {
            if let Some(node_embedding) = self.get_node_embedding_at(node_id, timestamp)? {
                let similarity = cosine_similarity(&node_embedding, reference_embedding)?;
                drift_timeline.push((timestamp, similarity));
            }
        }

        Ok(drift_timeline)
    }

    fn find_nearest_snapshot(&self, timestamp: Timestamp) -> Result<Arc<HnswIndex>> {
        // Binary search in BTreeMap for nearest snapshot <= timestamp
        let point = BiTemporalPoint {
            transaction_time: timestamp,
            valid_time: timestamp,
        };

        if let Some((_, snapshot)) = self.snapshots.range(..=point).next_back() {
            Ok(Arc::clone(snapshot))
        } else {
            // No snapshot before timestamp - must be very early query
            // Fall back to reconstructing from historical vectors
            self.reconstruct_index_at(timestamp)
        }
    }

    fn find_snapshots_in_range(
        &self,
        time_range: TimeRange,
    ) -> Result<Vec<(Timestamp, Arc<HnswIndex>)>> {
        let start = BiTemporalPoint {
            transaction_time: time_range.start,
            valid_time: time_range.start,
        };
        let end = BiTemporalPoint {
            transaction_time: time_range.end,
            valid_time: time_range.end,
        };

        Ok(self.snapshots
            .range(start..=end)
            .map(|(point, snapshot)| (point.transaction_time, Arc::clone(snapshot)))
            .collect())
    }
}
```

### Storage Format

```
Temporal Vector Index Directory:
data/
└── vector_index/
    ├── current.hnsw              # Live index (mutable)
    ├── snapshots/
    │   ├── snapshot_000000.hnsw  # Snapshot at T0
    │   ├── snapshot_000000.meta  # Metadata (timestamp, config)
    │   ├── snapshot_000001.hnsw  # Snapshot at T1
    │   ├── snapshot_000001.meta
    │   └── ...
    ├── metadata.json             # Snapshot index, configuration
    └── vectors/                  # Historical vector data
        ├── node_000001_versions  # Version chain for node 1's vectors
        ├── node_000002_versions
        └── ...

Snapshot Metadata File (.meta):
{
  "snapshot_id": 0,
  "transaction_time": "2024-01-15T10:00:00Z",
  "valid_time": "2024-01-15T10:00:00Z",
  "vector_count": 100000,
  "dimensions": 384,
  "hnsw_config": {
    "m": 16,
    "ef_construction": 128,
    "ef_search": 64,
    "metric": "cosine"
  },
  "size_bytes": 104857600,
  "created_at": "2024-01-15T10:00:05Z"
}
```

### Integration with Existing WAL

Vector index snapshots will be coordinated with existing checkpoint mechanism:

```rust
// When creating a database checkpoint
pub fn create_checkpoint(&self) -> Result<()> {
    // 1. Checkpoint current storage (existing)
    self.current_storage.checkpoint()?;

    // 2. Checkpoint historical storage (existing)
    self.historical_storage.checkpoint()?;

    // 3. Create vector index snapshot (NEW)
    if self.vector_index.is_enabled() {
        self.vector_index.create_snapshot(self.current_timestamp())?;
    }

    // 4. Flush WAL
    self.wal.flush()?;

    Ok(())
}
```

## Consequences

### Positive

1. **Fast temporal queries**: O(log k) HNSW search on snapshots vs O(n log n) index rebuild
2. **Bounded memory**: Max snapshots limit prevents unbounded growth
3. **Mirrors existing architecture**: Snapshot strategy parallels anchor+delta pattern
4. **Configurable trade-offs**: Users can tune snapshot frequency vs storage
5. **Arc-based sharing**: HNSW index cloning is cheap (reference counting)
6. **Immutable snapshots**: Historical indexes never change, enabling aggressive caching
7. **Supports all temporal query types**: Point-in-time, range, drift tracking

### Negative

1. **Storage overhead**: Each snapshot is a full HNSW index (~1KB per vector)
   - 100 snapshots × 1M vectors = ~100GB snapshot storage
2. **Snapshot staleness**: Queries between snapshots use older snapshot (bounded inaccuracy)
3. **Snapshot creation cost**: O(n) to clone HNSW index (but amortized over many queries)
4. **Memory pressure**: Multiple HNSW indexes in memory simultaneously
5. **Complexity**: Additional index management layer

### Neutral

1. **Trade-off tunability**: More snapshots = better accuracy but more storage
2. **Snapshot pruning**: LRU or time-based pruning strategies available
3. **Compression opportunities**: Older snapshots could be compressed
4. **Delta encoding potential**: Future optimization for incremental snapshots

## Alternatives Considered

### Alternative 1: Rebuild Index at Query Time

**Description**: Reconstruct HNSW index from historical vector data when queried.

**Pros**:
- Zero snapshot storage overhead
- Perfect accuracy (no staleness)
- Simple implementation

**Cons**:
- O(n log n) construction cost per query (prohibitive)
- 1M vectors × 10ms per insert = ~3 hours to build
- Violates <10ms query latency target

**Rejected because**: Query latency is unacceptable for production use.

### Alternative 2: Delta-Based Incremental Snapshots

**Description**: Store only changes between snapshots, reconstruct by applying deltas.

**Pros**:
- Lower storage than full snapshots
- More granular temporal resolution
- Bounded reconstruction cost

**Cons**:
- Complex delta computation for HNSW graphs
- HNSW graph structure changes make deltas expensive
- Reconstruction requires graph surgery
- Higher complexity than full snapshots

**Deferred because**: Good future optimization but adds significant complexity. Full snapshots are simpler for MVP.

### Alternative 3: Versioned Vector Storage Only (No Index Snapshots)

**Description**: Version vectors using anchor+delta, rebuild index on demand.

**Pros**:
- Minimal storage (only vector data)
- Reuses existing anchor+delta infrastructure
- Simple to implement

**Cons**:
- Still requires O(n log n) index rebuild per query
- No better than Alternative 1 for query performance
- Defeats the purpose of indexing

**Rejected because**: Same query latency problem as Alternative 1.

### Alternative 4: Hybrid - Snapshots + Delta Reconstruction

**Description**: Use snapshots as anchors, apply vector deltas for queries between snapshots.

**Pros**:
- Reduces snapshot staleness
- Bounded reconstruction cost
- Balances storage vs accuracy

**Cons**:
- Complex implementation
- HNSW updates are expensive (not designed for incremental)
- May violate query latency target for large deltas

**Deferred for Phase 4**: Interesting optimization but adds complexity. Consider after validating snapshot approach.

## Implementation Notes

### Recommended Default Configuration

**Updated 2024**: Default `max_snapshots` reduced from 100 to 20 due to memory concerns (see issue #230).

```rust
pub fn default_temporal_vector_config() -> TemporalVectorConfig {
    TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(10),  // Mirror anchor interval
        max_snapshots: 20,  // Reduced from 100 to prevent OOM (issue #230)
        hnsw_config: HnswConfig::default(),
    }
}
```

**Note**: Each snapshot creates a full copy of the HNSW index. For 100K vectors at 384 dimensions, each snapshot uses ~200MB (150MB vectors + 50-100MB HNSW graph). With 20 snapshots, total memory = ~4GB. The old default of 100 snapshots used ~20GB, causing OOM issues.

### Memory Budget Analysis

**Current Implementation** (Full Snapshot - Phase 3.1):

| Scenario | Vectors | Dims | Snapshots | Memory/Snapshot | Total Memory | Recommendation |
|----------|---------|------|-----------|-----------------|--------------|----------------|
| Small DB | 10K | 384 | 20 | ~20MB | ~400MB | Default (20) |
| Medium DB | 100K | 384 | 20 | ~200MB | ~4GB | Default (20) |
| Large DB | 100K | 384 | 50 | ~200MB | ~10GB | Increase if RAM available |
| Large DB | 1M | 384 | 10 | ~2GB | ~20GB | Reduce snapshots |

**Future with Anchor+Delta** (Phase 3.2 - Planned):
- Memory reduction: ~9X (20GB → ~2.2GB for 100 snapshots)
- Enables higher `max_snapshots` without memory penalty

**Mitigation strategies**:
1. **Lazy loading**: Load snapshots on-demand, evict LRU
2. **Compression**: Compress older snapshots (usearch supports serialization)
3. **Snapshot pruning**: Keep recent + strategically important snapshots
4. **Tiered storage**: Move old snapshots to disk/S3

### Storage Budget Analysis

Assuming 384-dim vectors, M=16 HNSW:

| Component | Storage per Vector |
|-----------|-------------------|
| Raw vector | 384 × 4 bytes = 1.5KB |
| HNSW index | ~1KB |
| **Total per snapshot** | **~2.5KB** |

**For 1M vectors, 100 snapshots**:
- Raw vectors: 1.5GB
- HNSW snapshots: 100 × 1GB = 100GB
- **Total: ~101.5GB**

**Comparison to anchor+delta (property data)**:
- Properties: ~5-7x compression
- Vectors: No delta compression (full snapshots)
- Trade-off: Query speed vs storage efficiency

### Performance Targets

| Operation | Target | Notes |
|-----------|--------|-------|
| Snapshot creation | <1s for 1M vectors | Amortized over 10 transactions |
| Temporal k-NN query | <10ms | Same as current-state query |
| Range query | <10ms per snapshot | Linear in snapshot count |
| Drift tracking | <100ms for 10 snapshots | Includes vector reconstruction |

### Snapshot Lifecycle

```
Transaction → Vector Change → Check Strategy → Create Snapshot?
                                    ↓
                              YES: Clone HNSW
                                    ↓
                              Persist to disk
                                    ↓
                              Update metadata
                                    ↓
                              Enforce max limit
                                    ↓
                              Prune if needed
```

## API Design

### Public API

```rust
impl GallifreyDB {
    /// Enable temporal vector indexing
    pub fn enable_temporal_vector_index(
        &self,
        property_name: &str,
        config: TemporalVectorConfig,
    ) -> Result<()>;

    /// Find k-NN at specific point in time
    pub fn find_similar_as_of(
        &self,
        query_embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>>;

    /// Find k-NN for a node at specific point in time
    pub fn find_similar_node_as_of(
        &self,
        query_node_id: NodeId,
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>>;

    /// Track how similar a node was to reference over time
    pub fn track_semantic_drift(
        &self,
        node_id: NodeId,
        reference_embedding: &[f32],
        time_range: TimeRange,
    ) -> Result<Vec<(Timestamp, f32)>>;

    /// Find similar nodes across time range (one result set per snapshot)
    pub fn find_similar_in_range(
        &self,
        query_embedding: &[f32],
        k: usize,
        time_range: TimeRange,
    ) -> Result<Vec<(Timestamp, Vec<(NodeId, f32)>)>>;

    /// Manual snapshot creation (for critical timestamps)
    pub fn create_vector_snapshot(&self) -> Result<()>;

    /// Get snapshot metadata (for monitoring)
    pub fn get_snapshot_info(&self) -> Result<Vec<SnapshotInfo>>;
}

/// Snapshot information for monitoring
pub struct SnapshotInfo {
    pub snapshot_id: usize,
    pub timestamp: Timestamp,
    pub vector_count: usize,
    pub size_bytes: usize,
    pub age: Duration,
}
```

### Example Usage

```rust
// Enable temporal vector indexing
let config = TemporalVectorConfig {
    snapshot_strategy: SnapshotStrategy::Hybrid {
        transaction_interval: 10,
        time_interval: Duration::from_secs(3600),  // Hourly
        change_threshold: 0.1,  // 10% changed
    },
    max_snapshots: 20,  // Conservative default (was 100, see issue #230)
    hnsw_config: HnswConfig::new(384, DistanceMetric::Cosine),
};
db.enable_temporal_vector_index("embedding", config)?;

// Query: "What was similar to this in 2023?"
let timestamp_2023 = Timestamp::from_str("2023-12-31T23:59:59Z")?;
let similar_in_2023 = db.find_similar_as_of(&query_embedding, 10, timestamp_2023)?;

// Query: "How has 'AI safety' meaning drifted?"
let ai_safety_embedding = get_current_embedding("AI safety");
let drift = db.track_semantic_drift(
    concept_node_id,
    &ai_safety_embedding,
    TimeRange::new(
        Timestamp::from_str("2020-01-01T00:00:00Z")?,
        Timestamp::from_str("2025-01-01T00:00:00Z")?,
    ),
)?;

for (timestamp, similarity) in drift {
    println!("{}: similarity = {:.2}", timestamp, similarity);
}

// Query: "Find similar across entire history"
let history = db.find_similar_in_range(
    &query_embedding,
    10,
    TimeRange::all(),
)?;

for (timestamp, results) in history {
    println!("At {}: found {} similar nodes", timestamp, results.len());
}
```

## Migration Path

### Phase 3 Implementation Plan

1. **Core snapshot manager** (2 days)
   - `TemporalVectorIndex` struct
   - Snapshot creation/retrieval logic
   - Metadata management

2. **Snapshot strategies** (1 day)
   - Implement all 4 strategies
   - Configuration parsing
   - Default configuration

3. **Temporal query API** (2 days)
   - `find_similar_as_of()`
   - `find_similar_in_range()`
   - `track_semantic_drift()`

4. **Persistence integration** (1 day)
   - Snapshot serialization
   - Checkpoint coordination
   - Recovery logic

5. **Testing & benchmarking** (2 days)
   - Unit tests for snapshot logic
   - Integration tests for temporal queries
   - Performance benchmarks

**Total estimate**: 8 days (aligned with "effort:medium" label)

### Backward Compatibility

- Phase 2 current-state queries remain unchanged
- Temporal indexing is opt-in via `enable_temporal_vector_index()`
- Databases without temporal indexing continue to work
- Can enable temporal indexing on existing database (creates first snapshot)

**Configuration Default Change (2024)**:
- Default `max_snapshots` reduced from 100 to 20 (issue #230)
- **Existing databases**: Configurations are stored per-database. Existing databases retain their configured `max_snapshots` value (likely 100 if created before this change)
- **New databases**: Will use new default of 20 snapshots
- **Migration**: Users with existing databases can manually reduce `max_snapshots` if experiencing memory pressure, or keep existing value if RAM is sufficient
- **Rationale**: Each snapshot creates full HNSW index copy (~200MB for 100K vectors, 384 dims). Old default of 100 snapshots = ~20GB, causing OOM issues. New default of 20 = ~4GB, suitable for most deployments.

### Future Enhancements (Phase 4+)

1. **Delta-based snapshots**: Reduce storage overhead
2. **Snapshot compression**: Compress older snapshots
3. **Lazy loading**: Load snapshots on-demand
4. **Distributed snapshots**: Shard snapshots across machines
5. **Hybrid queries**: Combine graph + temporal + vector in single query

## Success Criteria

1. **Functional**:
   - ✅ Temporal vector queries work correctly
   - ✅ Snapshots created according to strategy
   - ✅ Snapshot limit enforced
   - ✅ Recovery from persistence works

2. **Performance**:
   - ✅ Temporal k-NN query <10ms (same as current)
   - ✅ Snapshot creation <1s for 1M vectors
   - ✅ No regression in current-state query performance

3. **Storage**:
   - ✅ Storage overhead documented and acceptable
   - ✅ Snapshot pruning prevents unbounded growth

4. **Integration**:
   - ✅ Seamless integration with existing checkpoint mechanism
   - ✅ Clear API following GallifreyDB conventions
   - ✅ Comprehensive documentation and examples

## References

- [HNSW Paper](https://arxiv.org/abs/1603.09320) - Original HNSW algorithm
- [usearch Documentation](https://github.com/unum-cloud/usearch) - HNSW implementation
- [Temporal + Vector Research](https://arxiv.org/abs/2304.12212) - AeonG paper
- [Vector Database Benchmarks](https://ann-benchmarks.com/) - Performance baselines
- ADR-0001: Hybrid Storage Architecture - Foundation pattern
- ADR-0004: Anchor+Delta Compression - Inspiration for snapshot intervals
- ADR-0011: Vector Search Integration - Phases 1-2 implementation
- docs/VECTOR_SEARCH_DESIGN.md - Overall vector search design
