# GallifreyDB Architecture & Development Guidelines

## ⚠️ CRITICAL: NEVER COMMIT DIRECTLY TO TRUNK ⚠️

**TRUNK IS A PROTECTED BRANCH. YOU MUST ALWAYS USE WORKTREES AND PULL REQUESTS.**

Before making ANY code changes:
1. Check current branch: `git branch --show-current`
2. If on `trunk`, STOP and create a worktree: `just worktree-new feature/your-feature-name`
3. Work in the worktree, commit there, push, and create a PR
4. NEVER use `git commit` when on trunk - there is a pre-commit hook to prevent this

**The ONLY acceptable commits to trunk are automated merges from approved PRs.**

Breaking this rule causes:
- Build failures in CI
- Merge conflicts for other developers
- Formatting inconsistencies
- Wasted time fixing preventable issues

This is enforced by a pre-commit hook that will block direct commits to trunk.

## Project Overview

GallifreyDB is a high-performance bi-temporal graph database written in Rust. It tracks both **valid time** (when facts were true in reality) and **transaction time** (when facts were recorded in the database), while maintaining performance comparable to regular graph databases for current-state queries.

**Primary Use Case - LLM Integration**: Enable reasoning LLMs to query not just current knowledge, but see how that knowledge evolved over time. This allows LLMs to understand temporal context, track when facts changed, reason about causality, and detect contradictions through provenance tracking.

## Architecture Principles

### 1. Performance First

**Current-State Queries Must Be Fast:**
- Current state stored separately from historical data (hybrid storage architecture)
- Zero abstraction overhead for non-temporal queries
- CSR (Compressed Sparse Row) adjacency representation for cache-friendly traversals
- **Target**: <1µs single-hop traversal, <100µs for 3-hop traversal

**Temporal Queries Must Be Efficient:**
- Anchor+delta compression reduces storage 5-6X
- Temporal B-Tree indexes for range queries
- Anchor-based reconstruction skips unnecessary versions
- **Target**: <10ms for point-in-time reconstruction

### 2. Storage Efficiency

**Compression Strategy:**
- Create anchor (full snapshot) every 10 versions (configurable)
- Delta encoding for incremental changes
- Copy-on-write with `Arc<T>` for property deduplication
- String interning for labels and property keys
- **Target**: <2X overhead vs non-temporal storage

**Immutable History:**
- Historical versions are immutable after creation
- Enables aggressive caching and compression
- Safe for concurrent access without locks

### 3. Correctness Guarantees

**Temporal Consistency:**
- Transaction time is monotonically increasing
- Valid time can be retroactive but must be consistent
- No temporal paradoxes (e.g., deleting an entity before it was created)

**ACID Properties:**
- **Atomicity**: WAL ensures atomic commits
- **Consistency**: Invariants checked on write
- **Isolation**: MVCC provides snapshot isolation
- **Durability**: WAL + fsync guarantees

## Design Patterns

### Hybrid Storage Architecture

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

**When to Use Each:**
- **Current**: All non-temporal queries, latest state access
- **Historical**: Time-travel, audit trails, temporal analysis, LLM reasoning

### Version Chain Management

```rust
Node { current_version, first_version }
  → NodeVersion { next_version, is_anchor, data }
  → NodeVersion { next_version, is_anchor, delta }
  → NodeVersion { ... }
```

**Invariants:**
- Versions ordered by transaction time (immutable)
- Anchor exists at start of chain or periodically
- Delta chain never exceeds configured limit

### Temporal Query Processing

**1. Time Point Query (as of timestamp T):**
- Lookup in temporal index: `(EntityId, T) → VersionId`
- Find nearest anchor ≤ T
- Apply deltas forward to T
- Return reconstructed state

**2. Time Range Query (between T1 and T2):**
- Range scan temporal index
- Collect all versions in range
- Reconstruct each version
- Return as stream

**3. Knowledge Evolution Query (for LLMs):**
- Query how entity/relationship changed over time
- Track provenance and sources
- Identify when understanding shifted

## Rust Coding Standards

### Type Safety

**Strong Typing for IDs:**
```rust
// GOOD: Distinct types prevent mix-ups
pub struct NodeId(u64);
pub struct EdgeId(u64);
pub struct VersionId(u64);

// BAD: Using raw u64 everywhere
fn get_node(id: u64) -> Node { /* which kind of ID? */ }
```

**ID Validation and Security:**

All ID types validate values on construction to prevent security issues:

```rust
// GOOD: Use validated constructors in public API
pub fn create_node(&self, id: u64) -> Result<NodeId> {
    NodeId::new(id)  // Validates ID is within MAX_VALID_ID
}

// INTERNAL USE ONLY: new_unchecked() bypasses validation
// - MUST remain pub(crate) - never expose in public API
// - Only use when ID is known valid (WAL recovery, trusted storage)
// - Document safety reasoning at call site
impl NodeId {
    pub(crate) const fn new_unchecked(id: u64) -> Self {
        NodeId(id)
    }
}
```

**Critical Security Rule**: The `new_unchecked()` methods MUST remain `pub(crate)`.
Never expose them in:
- Public API functions
- C FFI boundaries
- External plugin systems
- Any untrusted context

IDs exceeding `MAX_VALID_ID` (u64::MAX - 1000) are rejected to prevent:
- Arithmetic overflow in ID operations
- Excessive memory allocation attempts
- Serialization buffer overflow
- DoS attacks via extreme values

**Temporal Types:**
```rust
// GOOD: Explicit temporal semantics
pub struct BiTemporalInterval {
    valid_time: TimeRange,
    transaction_time: TimeRange,
}

// BAD: Using raw tuples or generic ranges
```

### Error Handling

**Use `Result<T, Error>` for Fallible Operations:**
```rust
pub fn get_node(&self, id: NodeId) -> Result<Node, Error> {
    self.nodes.get(&id).ok_or(Error::NodeNotFound(id))
}
```

**Define Specific Error Types:**
```rust
pub enum StorageError {
    NodeNotFound(NodeId),
    EdgeNotFound(EdgeId),
    VersionNotFound(VersionId),
    TemporalConstraintViolation {
        entity_id: String,
        reason: String,
    },
    Io(io::Error),
}

impl From<io::Error> for StorageError {
    fn from(err: io::Error) -> Self {
        StorageError::Io(err)
    }
}
```

**Never Use `.unwrap()` or `.expect()` in Production Code:**
- Only use in tests or when impossible to fail (document why)
- Prefer `?` operator for error propagation
- Handle errors at appropriate levels

### Performance Guidelines

**Minimize Allocations:**
```rust
// GOOD: Reuse buffers
let mut buffer = Vec::with_capacity(100);
for item in items {
    buffer.clear();
    process_into_buffer(item, &mut buffer);
}

// BAD: Allocate per iteration
for item in items {
    let buffer = vec![];  // New allocation each time
    process(item, buffer);
}
```

**Use Zero-Copy Where Possible:**
```rust
// GOOD: Return references
pub fn get_properties(&self) -> &PropertyMap {
    &self.properties
}

// BAD: Clone unnecessarily
pub fn get_properties(&self) -> PropertyMap {
    self.properties.clone()
}
```

**Prefer Iterator Chains:**
```rust
// GOOD: Lazy evaluation
edges.iter()
    .filter(|e| e.label == target_label)
    .map(|e| e.target)
    .collect()

// BAD: Intermediate collections
let filtered: Vec<_> = edges.iter()
    .filter(|e| e.label == target_label)
    .collect();
filtered.iter().map(|e| e.target).collect()
```

### Concurrency

**Use Lock-Free Structures for Hot Paths:**
```rust
// Current indexes use DashMap (concurrent hashmap)
pub struct CurrentIndexes {
    nodes: DashMap<NodeId, Node>,
    edges: DashMap<EdgeId, Edge>,
}
```

**Immutable History Needs No Locks:**
```rust
// Historical versions are immutable after creation
// Safe to read concurrently without locks
pub struct HistoricalStorage {
    versions: Vec<Arc<NodeVersion>>,  // Immutable, shared
}
```

**Avoid `RwLock` and `Mutex` on Hot Paths:**
- Use lock-free data structures (DashMap, atomic types)
- Prefer immutability over locking
- If locking is necessary, hold locks for minimal time

### Memory Management

**Use `Arc` for Shared Ownership:**
```rust
// Properties shared across versions
pub struct PropertyMap {
    inner: Arc<HashMap<PropertyKey, PropertyValue>>,
}

impl Clone for PropertyMap {
    fn clone(&self) -> Self {
        // Cheap: only increments reference count
        PropertyMap { inner: Arc::clone(&self.inner) }
    }
}
```

**String Interning for Repeated Strings:**
```rust
// Labels and property keys are interned
pub struct StringInterner {
    strings: DashMap<Arc<str>, InternedString>,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct InternedString(u32);  // 4 bytes instead of 24
```

**Profile Before Optimizing:**
- Use `cargo flamegraph` for CPU profiling
- Use `heaptrack` or `valgrind` for memory profiling
- Benchmark before/after optimizations
- Document trade-offs in code comments

## Testing Requirements

### Unit Tests

**Test Each Module in Isolation:**
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_reconstruction() {
        let anchor = create_anchor_version();
        let delta = create_delta_version();
        let reconstructed = reconstruct(&anchor, &delta);
        assert_eq!(reconstructed.properties, expected_properties);
    }

    #[test]
    fn test_temporal_invariants() {
        // Transaction time must be monotonic
        let v1 = create_version(tx_time: 100);
        let v2 = create_version(tx_time: 99);
        assert!(v1.can_follow(&v2).is_err());
    }
}
```

### Integration Tests

**Test End-to-End Workflows:**
```rust
// tests/integration/temporal_queries.rs
#[test]
fn test_time_travel_query() -> Result<()> {
    let db = GallifreyDB::open("test.db")?;

    // Insert data at different times
    let node_id = db.create_node("Person", properties! {
        "name" => "Alice",
        "age" => 30,
    })?;

    // Later, update age
    db.update_node(node_id, properties! {
        "age" => 31,
    })?;

    // Query historical state
    let historical = db.as_of(timestamp_before_update)
        .get_node(node_id)?;
    assert_eq!(historical.get("age"), Some(&Value::Int(30)));

    // Query current state
    let current = db.get_node(node_id)?;
    assert_eq!(current.get("age"), Some(&Value::Int(31)));

    Ok(())
}
```

### Property-Based Tests

**Use `proptest` for Temporal Invariants:**
```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn temporal_consistency(operations: Vec<Operation>) {
        let mut db = GallifreyDB::new();

        for op in operations {
            let _ = db.apply(op);
        }

        // Verify temporal invariants
        assert!(db.verify_transaction_time_monotonic());
        assert!(db.verify_no_temporal_paradoxes());
        assert!(db.verify_version_chain_integrity());
    }

    #[test]
    fn reconstruction_equals_snapshot(
        anchor: NodeVersion,
        deltas: Vec<NodeVersion>
    ) {
        // Reconstructing from anchor+deltas should equal
        // a full snapshot at that point
        let reconstructed = reconstruct(anchor, deltas);
        let snapshot = create_snapshot_at_same_time();
        assert_eq!(reconstructed, snapshot);
    }
}
```

### Performance Benchmarks

**Criterion Benchmarks for Critical Paths:**
```rust
// benches/current_state.rs
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_single_hop_traversal(c: &mut Criterion) {
    let db = setup_test_graph();
    let start_node = db.get_node_by_label("Start").unwrap();

    c.bench_function("single_hop", |b| {
        b.iter(|| {
            db.get_outgoing_edges(black_box(start_node.id))
        });
    });
}

fn bench_time_travel(c: &mut Criterion) {
    let db = setup_temporal_graph();
    let timestamp = get_historical_timestamp();

    c.bench_function("time_travel_reconstruction", |b| {
        b.iter(|| {
            db.as_of(black_box(timestamp)).get_node(black_box(node_id))
        });
    });
}

criterion_group!(benches, bench_single_hop_traversal, bench_time_travel);
criterion_main!(benches);
```

**Continuous Performance Monitoring:**
- Run benchmarks on each PR
- Fail if performance regresses >10%
- Track latency percentiles (p50, p95, p99)
- Document performance characteristics

### Correctness Tests

**Temporal Invariants Must Hold:**
1. Transaction time is strictly increasing within a version chain
2. Valid time intervals are well-formed (start ≤ end)
3. No entity exists before it was created
4. Version chains are ordered by transaction time
5. Anchor+delta reconstruction matches full snapshot

**ACID Properties Must Hold:**
1. Transactions are atomic (all-or-nothing)
2. Constraints are enforced (consistency)
3. Concurrent transactions don't interfere (isolation)
4. Committed data survives crashes (durability)

## Performance Optimization Guidelines

### Measurement First

**Always Measure Before Optimizing:**
1. Identify bottleneck with profiler (flamegraph)
2. Write benchmark for specific case
3. Implement optimization
4. Measure improvement
5. Document trade-offs

**Avoid Premature Optimization:**
- Write clear code first
- Optimize only proven bottlenecks
- Keep optimized code well-documented

### Hot Path Optimization

**Optimize Current-State Queries First:**
- These represent 90%+ of typical workload
- Must be as fast as non-temporal graph DB
- Use `unsafe` if necessary (with safety proofs and comments)

**Temporal Queries Can Be Slower:**
- Still aim for <10ms for typical cases
- Trade-off: storage space vs reconstruction time
- Anchor interval is tunable parameter

### Memory Hierarchy Awareness

**Design for Cache Efficiency:**
- CSR adjacency list: sequential access pattern
- Group related data (consider struct-of-arrays vs array-of-structs)
- Align hot structures to cache lines (64 bytes)
- Keep hot data compact

**Minimize Memory Footprint:**
- String interning (reduces heap pressure)
- Arc-based sharing (avoid copies)
- Compression for cold data
- Consider memory pool allocation for frequently allocated types

### Async/Await Considerations

**Use Async for I/O, Not CPU:**
```rust
// GOOD: Async for I/O operations
pub async fn flush_wal(&self) -> Result<()> {
    self.wal.sync().await
}

// BAD: Async for pure computation
// (Adds overhead without benefit)
pub async fn compute_graph_stats(&self) -> Stats {
    // CPU-bound work doesn't benefit from async
}
```

## LLM Integration Patterns

### Temporal Query API for LLMs

**Natural Language-Like Queries:**
```rust
// LLM-friendly API design
db.as_of("2024-01-15T10:00:00Z")
    .find_node("Person", "name" == "Alice")
    .get_relationships("KNOWS")

db.between("2024-01-01", "2024-12-31")
    .track_changes(node_id)
    .with_provenance()
```

**Query Patterns LLMs Can Use:**
- "What did we know about X at time T?" → `db.as_of(T).get(X)`
- "How has Y changed?" → `db.history(Y).changes()`
- "When did we first record F?" → `db.first_occurrence(F)`
- "Show changes to E between T1 and T2" → `db.between(T1, T2).track_changes(E)`

### Provenance and Confidence Tracking

**Track Information Sources:**
```rust
pub struct PropertyValue {
    value: Value,
    metadata: Option<Metadata>,
}

pub struct Metadata {
    source: String,        // "user_input", "inference", "external_api"
    confidence: f64,       // 0.0 to 1.0
    created_at: Timestamp,
    created_by: String,
}
```

**Enable LLM Reasoning About Certainty:**
- Track how confidence evolved over time
- Identify conflicting sources
- Support "why do we believe X?" queries

### Integration Methods

**1. Direct Rust API** (for embedded use)
**2. MCP Server** (for Claude integration)
**3. REST/GraphQL API** (for general LLM tool use)
**4. Natural Query Language** (intuitive for LLMs to generate)

## Development Workflow

### IMPORTANT: Worktree-First Development

**When starting ANY implementation task, Claude instances MUST:**

1. **Create a worktree first** before making any code changes:
   ```bash
   just worktree-new feature/descriptive-name   # For new features
   just worktree-new fix/descriptive-name       # For bug fixes
   ```

2. **Navigate to the worktree** and work there:
   ```bash
   cd agents/feature-descriptive-name
   ```

3. **After completing work**, commit, create PR, and clean up:
   ```bash
   git add . && git commit -m "feat: description"
   just worktree-pr "PR Title" "Description"
   # After merge: just worktree-remove feature/descriptive-name
   ```

This enables multiple Claude instances to work in parallel without conflicts. Each instance gets an isolated copy of the codebase.

**Skip worktree creation only if:**
- You're already in a worktree (check with `git worktree list`)
- The task is read-only (exploration, answering questions)
- The user explicitly asks you to work in the main repo

See `WORKTREE_WORKFLOW.md` for complete documentation.

### Feature Development

1. **Design First**: Document design in issue/PR description
2. **API Before Implementation**: Define public API surface
3. **Test-Driven**: Write tests before implementation
4. **Benchmark**: Add benchmarks for performance-critical code
5. **Document**: Update CLAUDE.md if architecture changes

### Code Review Checklist

- [ ] Temporal invariants preserved
- [ ] No performance regression on benchmarks
- [ ] Error handling is comprehensive (no unwrap/expect)
- [ ] Tests cover edge cases
- [ ] Documentation updated
- [ ] No unsafe without safety comments
- [ ] Strong typing used (no raw primitives for IDs)

### Performance Testing

**Required Benchmarks:**
1. Current-state single-hop traversal (<1µs)
2. Current-state 3-hop traversal (<100µs)
3. Time-travel reconstruction (<10ms)
4. Batch insertion throughput (>100k edges/sec)
5. Storage overhead (<2X vs non-temporal)

## Unsafe Rust Guidelines

**When Unsafe Is Acceptable:**
- Performance-critical hot paths with proven bottlenecks
- Zero-copy optimizations
- FFI boundaries
- Interacting with hardware or memory-mapped files

**Requirements for Unsafe Code:**
```rust
// ALWAYS document safety invariants
// GOOD:
unsafe {
    // SAFETY: We know the slice has at least `len` elements because
    // we just checked `slice.len() >= len` above. The pointer is valid
    // because it comes from a Vec allocation.
    std::slice::from_raw_parts(ptr, len)
}

// BAD:
unsafe {
    std::slice::from_raw_parts(ptr, len)  // No explanation!
}
```

## Testing and Profiling Tools

### Coverage Requirements

GallifreyDB enforces strict code coverage thresholds:
- **Minimum 85% line coverage** (current: 86.45%)
- **Minimum 88% function coverage** (current: 89.10%)
- **Minimum 88% region coverage** (current: 88.91%)

See `TESTING.md` for detailed instructions on running coverage reports.

**Quick commands:**
```bash
# Check coverage meets thresholds
just coverage-check

# Generate HTML coverage report
just coverage

# Run all quality checks
just check-all
```

### Profiling with Tracy

Use Tracy profiler for detailed performance analysis:

1. Download Tracy from [releases](https://github.com/wolfpld/tracy/releases)
2. Start Tracy profiler GUI
3. Build with profiling: `cargo build --release --features tracy`
4. Run profiled build: `just profile-tracy`

**Instrumenting code:**
```rust
#[cfg(feature = "tracy")]
use tracy_client::span;

pub fn hot_path_function() {
    #[cfg(feature = "tracy")]
    let _span = span!("hot_path_function");

    // Function body
}
```

### Benchmarking

Use Criterion for performance benchmarks:

```bash
# Run all benchmarks
just bench

# Establish baseline
cargo bench -- --save-baseline main

# Compare against baseline
cargo bench -- --baseline main
```

Add benchmarks in `benches/` directory following existing patterns.

### Development Tools

All common tasks are available via `just`:
- `just test` - Run tests
- `just coverage` - Generate coverage report
- `just lint` - Run clippy
- `just fmt` - Format code
- `just pre-commit` - Quick pre-commit checks
- `just check-all` - Full quality check

See `justfile` for complete list of commands.

## WAL Format and Migration

### WAL Versioning

The Write-Ahead Log (WAL) uses a versioned binary format to enable future evolution:

```
Segment Header (5 bytes):
[magic: 4 bytes "GWAL"][version: 1 byte]

Entry Format:
[LSN: 8 bytes][timestamp: 8 bytes][checksum: 4 bytes][op_type: 1 byte][operation data...]
```

**Current Version: 2**
- Full serialization of properties (PropertyMap)
- Full serialization of bi-temporal intervals (32 bytes each)
- Labels serialized for all operation types

**Legacy Version: 1** (no header)
- Properties were not serialized (data loss on recovery)
- Temporal intervals were not serialized (reconstructed from timestamp)
- Update operations did not serialize labels

### Backward Compatibility

The WAL reader automatically detects the format version:
- **V2+ segments**: Identified by "GWAL" magic bytes at start
- **V1 segments**: No header, recognized by absence of magic bytes

When reading V1 segments:
- Properties default to `PropertyMap::new()` (empty)
- Temporal intervals default to `BiTemporalInterval::current(timestamp)`
- Update labels default to empty string

### Migration Tool

To migrate WAL segments to the current format:

```rust
use gallifreydb::storage::wal::{detect_wal_version, migrate_wal_segment, migrate_wal_directory};

// Check a single segment
let info = detect_wal_version(Path::new("data/wal/000001.log"))?;
println!("Version: {}, needs migration: {}", info.version, info.needs_migration);

// Migrate a single segment (creates .bak backup)
let entries_migrated = migrate_wal_segment(Path::new("data/wal/000001.log"))?;

// Migrate all segments in a directory
let results = migrate_wal_directory(Path::new("data/wal/"))?;
for (path, count) in results {
    println!("Migrated {}: {} entries", path.display(), count);
}
```

### Migration Process

1. **Backup**: Original segment is renamed to `.log.bak`
2. **Parse**: Entries are read using version-aware parsing
3. **Rewrite**: Entries are written in V2 format with proper header
4. **Verify**: New segment can be read back successfully

**Important**: Migration of V1 segments results in data loss for properties and temporal intervals that were never serialized. The migrated entries will have placeholder values.

### Adding New WAL Versions

When adding new serialization features:

1. Increment `WAL_VERSION` constant
2. Update `serialize_entry()` to write new format
3. Update `read_segment()` with version-aware parsing:
   ```rust
   let (data, len) = if version >= NEW_VERSION {
       // Deserialize new format
   } else {
       // Use placeholder for older versions
   };
   ```
4. Update `parse_wal_entries_versioned()` for migration support
5. Add tests for new format and backward compatibility

## Vector Storage & Indexing (Phases 1-2)

GallifreyDB supports storing dense vector embeddings as first-class property values with integrated HNSW indexing for fast k-NN search. This enables semantic search, similarity matching, and RAG (Retrieval-Augmented Generation) workflows while preserving full bi-temporal versioning.

### Storing Vector Properties

Use `PropertyMapBuilder::insert_vector()` to attach embeddings to nodes and edges:

```rust
use gallifreydb::{GallifreyDB, PropertyMapBuilder};

let db = GallifreyDB::new();

// Store an embedding on a node
let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
let node_id = db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert("title", "Introduction to Rust")
        .insert_vector("embedding", &embedding)
        .build(),
)?;

// Store embeddings on edges (relationship semantics)
let edge_id = db.create_edge(
    source_id,
    target_id,
    "SIMILAR_TO",
    PropertyMapBuilder::new()
        .insert_vector("relationship_embedding", &rel_embedding)
        .build(),
)?;
```

### Retrieving Vector Properties

```rust
let node = db.get_node(node_id)?;

// Get vector as slice
if let Some(embedding) = node.get_property("embedding").and_then(|v| v.as_vector()) {
    println!("Embedding dimensions: {}", embedding.len());
}

// Check property type
if let Some(prop) = node.get_property("embedding") {
    match prop.type_name() {
        "vector" => println!("It's a vector!"),
        _ => println!("Not a vector"),
    }
}
```

### Similarity Functions

The `gallifreydb::core::vector` module provides optimized similarity functions:

```rust
use gallifreydb::core::vector::{
    cosine_similarity,
    cosine_similarity_normalized,
    euclidean_distance,
    squared_euclidean_distance,
    dot_product,
};

let a = vec![1.0f32, 0.0, 0.0];
let b = vec![0.0f32, 1.0, 0.0];

// Cosine similarity: measures angle between vectors (-1 to 1)
// Best for: semantic similarity, document matching
let cos_sim = cosine_similarity(&a, &b)?;  // Returns 0.0 (orthogonal)

// Use normalized variant when vectors are pre-normalized (faster)
let cos_sim_fast = cosine_similarity_normalized(&a, &b)?;

// Euclidean distance: measures straight-line distance
// Best for: spatial data, clustering
let distance = euclidean_distance(&a, &b)?;

// Squared Euclidean (avoids sqrt, faster for comparisons)
let sq_distance = squared_euclidean_distance(&a, &b)?;

// Dot product: raw inner product
// Best for: when magnitude matters, MaxSim operations
let dot = dot_product(&a, &b)?;
```

**Choosing the Right Metric:**
| Metric | Use Case | Range | Notes |
|--------|----------|-------|-------|
| `cosine_similarity` | Semantic similarity | [-1, 1] | Ignores magnitude |
| `euclidean_distance` | Spatial clustering | [0, ∞) | Sensitive to magnitude |
| `dot_product` | MaxSim, ColBERT | (-∞, ∞) | Preserves magnitude |

### Normalization Functions

```rust
use gallifreydb::core::vector::{
    normalize,
    normalize_in_place,
    magnitude,
    is_normalized,
};

let v = vec![3.0f32, 4.0];

// Get magnitude (L2 norm)
let mag = magnitude(&v);  // Returns 5.0

// Create normalized copy (unit vector)
let unit = normalize(&v);  // Returns [0.6, 0.8]

// Normalize in place (mutates vector)
let mut v_mut = v.clone();
normalize_in_place(&mut v_mut);

// Check if already normalized
assert!(is_normalized(&unit, 1e-6));
```

### Validation Functions

```rust
use gallifreydb::core::vector::{
    validate_vector,
    check_dimensions_match,
    validate_vector_with_bounds,
};

// Validate vector contains no NaN/Infinity
validate_vector(&embedding)?;

// Check two vectors have same dimensions
check_dimensions_match(&a, &b)?;

// Validate with custom dimension limit
validate_vector_with_bounds(&embedding, 4096)?;
```

### Common Embedding Dimensions

GallifreyDB supports any dimension up to 100,000. Common sizes:

| Model | Dimensions |
|-------|------------|
| all-MiniLM-L6-v2 | 384 |
| all-mpnet-base-v2 | 768 |
| text-embedding-ada-002 | 1536 |
| text-embedding-3-large | 3072 |

### Temporal Vector Versioning

Vector properties are fully versioned like any other property:

```rust
// Create node with initial embedding
let node_id = db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert_vector("embedding", &v1_embedding)
        .build(),
)?;

// Update embedding (creates new version)
let mut tx = db.write_transaction()?;
tx.update_node(
    node_id,
    PropertyMapBuilder::new()
        .insert_vector("embedding", &v2_embedding)
        .build(),
)?;
tx.commit()?;

// Query historical embeddings via time-travel (Phase 2+)
// let old_node = db.as_of(timestamp).get_node(node_id)?;
```

### Performance Considerations

- **Storage**: Vectors are stored as contiguous `Arc<[f32]>` with efficient cloning
- **Serialization**: Binary format with 4-byte dimension prefix + raw f32 data
- **Memory**: ~4 bytes per dimension + small overhead
- **Similarity ops**: O(n) where n = dimensions; consider pre-normalization for cosine

### Error Handling

Vector operations return `Result<T, Error>` with specific error types:

```rust
use gallifreydb::utils::VectorError;

match cosine_similarity(&a, &b) {
    Ok(sim) => println!("Similarity: {}", sim),
    Err(Error::Vector(VectorError::DimensionMismatch { expected, actual })) => {
        eprintln!("Dimension mismatch: expected {}, got {}", expected, actual);
    }
    Err(Error::Vector(VectorError::InvalidVector(reason))) => {
        eprintln!("Invalid vector: {}", reason);
    }
    Err(e) => eprintln!("Other error: {}", e),
}
```

### Vector Index (k-NN Search)

Enable HNSW-based k-nearest-neighbor search on vector properties:

```rust
use gallifreydb::{GallifreyDB, PropertyMapBuilder};
use gallifreydb::index::vector::{HnswConfig, DistanceMetric};

let db = GallifreyDB::new();

// Enable vector indexing on a specific property
let config = HnswConfig::new(384, DistanceMetric::Cosine)  // 384 dimensions
    .with_capacity(10000);  // Expected number of vectors
db.enable_vector_index("embedding", config)?;

// Create nodes with embeddings - automatically indexed!
let doc1 = db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert("title", "Introduction to Rust")
        .insert_vector("embedding", &embedding1)
        .build(),
)?;

let doc2 = db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert("title", "Advanced Rust Patterns")
        .insert_vector("embedding", &embedding2)
        .build(),
)?;

// Find similar nodes
let similar = db.find_similar(doc1, 10)?;  // Returns Vec<(NodeId, f32)>
for (node_id, similarity) in similar {
    println!("Node {:?} has similarity {}", node_id, similarity);
}

// Find similar with label filter
let similar_docs = db.find_similar_with_label(doc1, "Document", 5)?;
```

**Auto-Indexing Behavior:**
- `create_node()`: Automatically indexes vectors, rolls back node on failure
- `update_node()`: Updates index entry, rolls back update on failure
- `delete_node()`: Removes from index (best-effort, no rollback needed)

**Supported Distance Metrics:**
| Metric | Use Case |
|--------|----------|
| `DistanceMetric::Cosine` | Semantic similarity (default) |
| `DistanceMetric::Euclidean` | Spatial data, clustering |
| `DistanceMetric::DotProduct` | MaxSim, ColBERT-style queries |

**Configuration Options:**
```rust
let config = HnswConfig::new(dimensions, metric)
    .with_capacity(expected_count)     // Pre-allocate index capacity
    .with_connectivity(16)             // HNSW M parameter (default: 16)
    .with_expansion_add(128)           // efConstruction (default: 128)
    .with_expansion_search(64);        // ef search parameter (default: 64)
```

**Comprehensive Documentation:**

For detailed information on vector search integration, see:
- **[Integration Guide](docs/guides/vector-search-integration.md)** - Complete integration examples and API reference
- **[Performance Guide](docs/guides/vector-search-performance.md)** - Tuning parameters and optimization strategies
- **[Troubleshooting Guide](docs/guides/vector-search-troubleshooting.md)** - Common issues and solutions
- **[Design Document](docs/VECTOR_SEARCH_DESIGN.md)** - Architecture and roadmap

## Future Considerations

### Vector Search (SUPERRAG) - Remaining Phases

**Status**: Phases 1-2 complete, Phases 3-5 pending

Phases 1-2 provide vector storage and HNSW k-NN search. Remaining phases will add:

- **Phase 3**: Temporal vector queries (semantic time-travel)
- **Phase 4**: Hybrid graph+vector queries
- **Phase 5**: Advanced features (streaming, incremental updates)

See **[docs/VECTOR_SEARCH_DESIGN.md](docs/VECTOR_SEARCH_DESIGN.md)** for the complete design.

**Key query patterns Phase 3+ will enable:**
```rust
// Semantic time-travel (Phase 3)
db.as_of(timestamp_2023).find_similar(embedding, k)

// Graph + Vector: traverse then rank (Phase 4)
db.traverse(alice_id, "KNOWS").rank_by_similarity(bob_embedding, 10)

// Knowledge evolution (Phase 4)
db.track_semantic_drift(node_id, time_range)
```

### Scalability
- Sharding for horizontal scale
- Distributed transaction coordination
- Replication for high availability

### Query Language
- Cypher-like temporal extensions
- SQL:2011 temporal syntax
- Time-aware pattern matching

### Advanced Features
- Temporal graph algorithms (shortest path over time)
- Streaming temporal queries
- Incremental materialized views
- LLM-assisted query generation

## References

- [AeonG: Efficient Temporal Graph Database](https://arxiv.org/abs/2304.12212)
- [XTDB Bi-temporality](https://v1-docs.xtdb.com/concepts/bitemporality/)
- [Temporal Database Concepts](https://en.wikipedia.org/wiki/Temporal_database)
- [Rust Performance Book](https://nnethercote.github.io/perf-book/)
