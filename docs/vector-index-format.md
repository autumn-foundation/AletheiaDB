# Vector Index Persistence Format Specification (.vidx)

**Version:** 1.0
**Status:** Specification
**Created:** 2026-01-20
**Issue:** [#86](https://github.com/madmax983/AletheiaDB/issues/86) (VS-080)
**Related:** [Phase 5: Persistence & Performance](VECTOR_SEARCH_DESIGN.md#phase-5-persistence--performance)

## Executive Summary

This document specifies the `.vidx` file format for persisting vector indexes in AletheiaDB. The format enables fast cold starts by storing HNSW indexes and their metadata to disk, eliminating the need for full WAL replay on database restart.

**Key Design Goals:**
- **Fast Loading**: Memory-mapped access for multi-GB indexes
- **Data Integrity**: CRC32 checksums and atomic writes
- **Compatibility**: Version-aware format supporting forward/backward compatibility
- **Security**: Built-in DoS protection via size limits
- **Efficiency**: Separate metadata and data for selective loading

## 1. File Extension

Vector indexes use the **`.vidx`** extension with the following naming convention:

```
<property_name>.vidx         # Current index (e.g., embedding.vidx)
<property_name>.meta         # Metadata file (e.g., embedding.meta)
<property_name>.mappings     # ID mappings file (e.g., embedding.mappings)
snapshot_<id>.usearch        # Temporal snapshot (e.g., snapshot_42.usearch)
snapshot_<id>.meta           # Snapshot metadata (e.g., snapshot_42.meta)
```

### Directory Structure

```
data/my-database/
└── indexes/
    ├── manifest.idx                    # Root manifest
    ├── strings/
    │   └── interner.idx                # String interner
    └── vector/
        ├── embedding/                  # Property: "embedding"
        │   ├── meta.idx                # Metadata (REQUIRED)
        │   ├── mappings.idx            # ID mappings (REQUIRED)
        │   ├── current.usearch         # Current HNSW index (REQUIRED)
        │   └── snapshots/              # Temporal snapshots (OPTIONAL)
        │       ├── snapshot_0.usearch
        │       ├── snapshot_0.meta
        │       ├── snapshot_10.usearch
        │       └── snapshot_10.meta
        └── title_embedding/            # Property: "title_embedding"
            ├── meta.idx
            ├── mappings.idx
            └── current.usearch
```

**Multi-Property Support**: Each vector property gets its own directory under `vector/`, enabling independent HNSW indexes per property (see [ADR-0022](adr/0022-multi-property-vector-index.md)).

## 2. File Format Overview

The vector index persistence format consists of **three separate files** per index:

| File | Purpose | Format | Size |
|------|---------|--------|------|
| `<property>.meta` | Metadata and configuration | Bitcode + CRC32 | <10 KB |
| `<property>.mappings` | NodeId ↔ usearch key mappings | Bitcode + CRC32 | ~24 bytes per vector |
| `<property>.usearch` | HNSW graph structure + vectors | usearch native | ~2.5 KB per vector |

**Rationale for Separation:**
1. **Selective Loading**: Load metadata without loading entire index
2. **Format Flexibility**: usearch native format for HNSW, bitcode for metadata
3. **Incremental Updates**: Update mappings without rebuilding HNSW
4. **Memory-Mapping**: Large `.usearch` files can be mmap'd independently

## 3. Metadata File Format (`.meta`)

### 3.1 File Structure

```
┌─────────────────────────────────────────────────────────┐
│                    Metadata File                        │
├─────────────────────────────────────────────────────────┤
│ Bitcode-Encoded VectorIndexMeta                         │
│ ┌─────────────────────────────────────────────────────┐ │
│ │ magic: [u8; 4]               # "GVEC"               │ │
│ │ version: u16                 # Format version (1)   │ │
│ │ property_name: String        # e.g., "embedding"    │ │
│ │ dimensions: u32              # Vector dimensions    │ │
│ │ metric: u8                   # Distance metric      │ │
│ │ hnsw_config: HnswConfig      # HNSW parameters      │ │
│ │ vector_count: u64            # Number of vectors    │ │
│ │ created_at: i64              # Unix timestamp       │ │
│ │ last_modified: i64           # Unix timestamp       │ │
│ └─────────────────────────────────────────────────────┘ │
├─────────────────────────────────────────────────────────┤
│ CRC32 Checksum: [u8; 4]                                 │
└─────────────────────────────────────────────────────────┘
```

### 3.2 Field Specifications

#### Magic Bytes
- **Value**: `[0x47, 0x56, 0x45, 0x43]` (ASCII "GVEC")
- **Purpose**: File type identification
- **Validation**: MUST match exactly, reject if mismatch

#### Version
- **Type**: `u16`
- **Current**: `1`
- **Purpose**: Format versioning for backward/forward compatibility
- **Validation**:
  - Reject if `version > MAX_SUPPORTED_VERSION`
  - Warn if `version < CURRENT_VERSION`

#### Property Name
- **Type**: UTF-8 string
- **Max Length**: 255 bytes
- **Purpose**: Identifies which property this index covers
- **Example**: `"embedding"`, `"title_embedding"`, `"content_vector"`
- **Validation**:
  - MUST be valid UTF-8
  - MUST NOT be empty
  - SHOULD match property name in manifest

#### Dimensions
- **Type**: `u32`
- **Range**: `[1, 100_000]`
- **Purpose**: Vector dimensionality
- **Validation**:
  - MUST be > 0
  - MUST be ≤ 100,000 (DoS protection)
  - MUST match vectors in usearch index

#### Distance Metric
- **Type**: `u8` (enum)
- **Values**:
  - `0` = Cosine similarity
  - `1` = Euclidean distance (L2)
  - `2` = Dot product (inner product)
  - `3` = Haversine (geospatial)
  - `4` = Hamming (binary)
  - `5` = Tanimoto (chemical fingerprints)
- **Purpose**: Determines similarity calculation
- **Validation**: MUST be valid enum value

#### HNSW Configuration

```rust
pub struct PersistedHnswConfig {
    pub m: u16,                  // Max connections per node (8-64)
    pub ef_construction: u16,    // Build-time expansion (100-500)
    pub ef_search: u16,          // Query-time expansion (10-500)
}
```

- **m**: Bidirectional connections per node
  - Default: 16
  - Range: [8, 64]
  - Memory: ~m × 8 bytes per vector

- **ef_construction**: Build-time candidate list size
  - Default: 128
  - Range: [100, 500]
  - Trade-off: Higher = better quality, slower build

- **ef_search**: Query-time candidate list size
  - Default: 64
  - Range: [10, 500]
  - Trade-off: Higher = better recall, slower queries
  - Can be adjusted at runtime

**Validation**:
- MUST be within specified ranges
- SHOULD warn if unusual values detected

#### Vector Count
- **Type**: `u64`
- **Purpose**: Number of vectors in the index
- **Validation**: SHOULD match actual count in usearch index

#### Timestamps
- **Type**: `i64` (Unix timestamp in seconds)
- **Fields**: `created_at`, `last_modified`
- **Purpose**: Audit trail and staleness detection
- **Validation**: SHOULD be reasonable (not year 1970 or 3000)

### 3.3 CRC32 Checksum
- **Position**: Last 4 bytes of file
- **Algorithm**: CRC32 (IEEE 802.3 polynomial)
- **Input**: All bytes before checksum
- **Byte Order**: Little-endian
- **Purpose**: Detect corruption from bit rot, disk errors, crashes

**Validation Process**:
```rust
// Read file
let bytes = fs::read(path)?;
let (data, checksum_bytes) = bytes.split_at(bytes.len() - 4);

// Compute expected checksum
let mut hasher = crc32fast::Hasher::new();
hasher.update(data);
let computed = hasher.finalize();

// Validate
let stored = u32::from_le_bytes(checksum_bytes);
if computed != stored {
    return Err(CorruptedIndex);
}
```

## 4. ID Mappings File Format (`.mappings`)

### 4.1 Purpose

Maps AletheiaDB `NodeId` ↔ usearch internal keys. Required because:
- usearch uses contiguous integer keys (0, 1, 2, ...)
- AletheiaDB uses 64-bit `NodeId` (can be sparse)
- Enables node lookup without scanning entire index

### 4.2 File Structure

```
┌─────────────────────────────────────────────────────────┐
│                   Mappings File                         │
├─────────────────────────────────────────────────────────┤
│ Bitcode-Encoded VectorMappingsData                      │
│ ┌─────────────────────────────────────────────────────┐ │
│ │ version: u16                 # Format version (1)   │ │
│ │ count: u64                   # Number of mappings   │ │
│ │ mappings: Vec<VectorMapping>                        │ │
│ │   ┌───────────────────────────────────────────────┐ │ │
│ │   │ node_id: u64          # AletheiaDB NodeId    │ │ │
│ │   │ usearch_key: u64      # usearch internal key  │ │ │
│ │   └───────────────────────────────────────────────┘ │ │
│ │ deleted_ids: Vec<u64>        # Soft-deleted nodes  │ │
│ └─────────────────────────────────────────────────────┘ │
├─────────────────────────────────────────────────────────┤
│ CRC32 Checksum: [u8; 4]                                 │
└─────────────────────────────────────────────────────────┘
```

### 4.3 Field Specifications

#### Version
- **Type**: `u16`
- **Current**: `1`
- **Purpose**: Mapping format versioning

#### Count
- **Type**: `u64`
- **Purpose**: Number of active mappings
- **Validation**: MUST equal `mappings.len()`

#### Mappings
- **Type**: `Vec<VectorMapping>`
- **Order**: Sorted by `node_id` for binary search
- **Size**: ~24 bytes per mapping (2 × u64 + overhead)

**Mapping Entry**:
```rust
struct VectorMapping {
    node_id: u64,       // AletheiaDB node ID
    usearch_key: u64,   // usearch internal key
}
```

**Properties**:
- `node_id` is unique across all mappings
- `usearch_key` is unique across all mappings
- Both MUST be < `u64::MAX - 1000` (DoS protection)

#### Deleted IDs
- **Type**: `Vec<u64>`
- **Purpose**: Track soft-deleted nodes (HNSW limitation workaround)
- **Reason**: usearch doesn't support true deletion, so we track deleted IDs and filter them from search results
- **Cleanup**: Deleted IDs are removed when snapshot is created

### 4.4 Usage Patterns

**NodeId → usearch key** (for index operations):
```rust
// Binary search in sorted mappings
let usearch_key = mappings
    .binary_search_by_key(&node_id, |m| m.node_id)
    .map(|idx| mappings[idx].usearch_key)?;
```

**usearch key → NodeId** (for search results):
```rust
// Linear scan or secondary index
let node_id = mappings
    .iter()
    .find(|m| m.usearch_key == usearch_key)
    .map(|m| m.node_id)?;
```

**Optimization**: For large indexes (>100K vectors), consider:
- In-memory hash map for reverse lookup
- Secondary index file for bidirectional mapping

## 5. HNSW Index File Format (`.usearch`)

### 5.1 Native usearch Format

The HNSW graph structure and vector data are stored in **usearch's native binary format**. We do NOT define a custom format for this component.

**Rationale**:
- usearch format is battle-tested and optimized
- Native format enables memory-mapped loading
- Avoids reimplementing complex HNSW serialization
- Future-proof: benefits from usearch improvements

### 5.2 File Structure (usearch native)

```
┌─────────────────────────────────────────────────────────┐
│                 usearch Index File                      │
├─────────────────────────────────────────────────────────┤
│ Header (usearch-specific)                               │
│ - Magic bytes                                           │
│ - Version                                               │
│ - Dimensions                                            │
│ - Metric kind                                           │
│ - Scalar kind (quantization)                            │
│ - M, ef_construction                                    │
│ - Entry point ID                                        │
│ - Max level                                             │
│ - Node count                                            │
├─────────────────────────────────────────────────────────┤
│ HNSW Graph Layers                                       │
│ Layer 0 (densest):                                      │
│   Node 0: [neighbor_ids..., distances...]              │
│   Node 1: [neighbor_ids..., distances...]              │
│   ...                                                   │
│ Layer 1:                                                │
│   Node 0: [neighbor_ids..., distances...]              │
│   ...                                                   │
│ Layer N (sparsest, entry point)                        │
├─────────────────────────────────────────────────────────┤
│ Vector Data                                             │
│ Vector 0: [f32 × dimensions] or [f16 × dim] or [i8]    │
│ Vector 1: [f32 × dimensions] or [f16 × dim] or [i8]    │
│ ...                                                     │
│ Vector N: [f32 × dimensions] or [f16 × dim] or [i8]    │
└─────────────────────────────────────────────────────────┘
```

**Key Properties**:
- **Contiguous layout**: Enables efficient memory-mapping
- **Platform-independent**: Little-endian byte order
- **Quantization support**: F32 (4 bytes), F16 (2 bytes), I8 (1 byte)
- **Self-describing**: Header contains all necessary metadata

### 5.3 Memory Considerations

**Storage Size Estimation**:
```
Index Size ≈ N × (dimensions × scalar_size + M × 12 + overhead)

Where:
- N = number of vectors
- dimensions = vector dimensions
- scalar_size = 4 (F32), 2 (F16), or 1 (I8)
- M = connections per node
- 12 = approximate bytes per connection (ID + distance)
- overhead = ~200 bytes per vector (graph structure)
```

**Example** (1M vectors, 384 dimensions, M=16, F32):
```
Size ≈ 1M × (384 × 4 + 16 × 12 + 200)
    ≈ 1M × (1536 + 192 + 200)
    ≈ 1M × 1928 bytes
    ≈ 1.84 GB
```

**Memory-Mapped Loading**:
```rust
// For indexes > 1GB, use memory-mapping
let index = Index::new(&IndexOptions {
    dimensions,
    metric,
    quantization,
    ..Default::default()
})?;

// Load with mmap (read-only)
index.load_mmap(path)?;  // Does not load entire file into RAM
```

**Benefits**:
- Serves multi-GB indexes without consuming RAM
- OS-level page cache management
- Fast startup (no deserialization)

**Limitations**:
- Read-only (cannot modify mmap'd index)
- OS-dependent behavior
- Requires file system support

## 6. Temporal Snapshot Format

For temporal vector indexes, snapshots are stored in a `snapshots/` subdirectory.

### 6.1 Snapshot Files

Each snapshot consists of:
- `snapshot_<id>.usearch` - HNSW index at snapshot time
- `snapshot_<id>.meta` - Snapshot metadata

### 6.2 Snapshot Metadata Format

```
┌─────────────────────────────────────────────────────────┐
│                 Snapshot Metadata                       │
├─────────────────────────────────────────────────────────┤
│ Bitcode-Encoded VectorSnapshotMeta                      │
│ ┌─────────────────────────────────────────────────────┐ │
│ │ snapshot_id: u64             # Unique snapshot ID   │ │
│ │ snapshot_type: SnapshotType  # Full or Delta        │ │
│ │ timestamp: i64               # Creation time        │ │
│ │ vector_count: u64            # Vectors in snapshot  │ │
│ │ config: HnswConfig           # HNSW parameters      │ │
│ │ base_snapshot_id: Option<u64># For delta snapshots  │ │
│ └─────────────────────────────────────────────────────┘ │
├─────────────────────────────────────────────────────────┤
│ CRC32 Checksum: [u8; 4]                                 │
└─────────────────────────────────────────────────────────┘
```

### 6.3 Snapshot Types

**Full Snapshot**:
- Complete HNSW index
- Self-contained (no dependencies)
- Created every N snapshots (default: 10)
- Larger size but faster to load

**Delta Snapshot**:
- Only changed vectors since last full snapshot
- References `base_snapshot_id`
- Smaller size but requires reconstruction
- Maximum chain depth: 10 (enforced)

**Snapshot Strategy**:
```
Timeline:
T0  : Full (10k vectors)       - 25MB
T10 : Delta (+100, -50)        - 375KB  (base: T0)
T20 : Delta (+200, -100)       - 750KB  (base: T0)
T30 : Full (10,150 vectors)    - 25MB
T40 : Delta (+50, -25)         - 250KB  (base: T30)
...

Query at T20: Load Full@T0 + apply Delta@T10 + Delta@T20
```

**Reconstruction**:
1. Load base full snapshot
2. Apply delta changes in chronological order
3. Rebuild HNSW index with combined vectors
4. Cache reconstructed snapshot

## 7. Backward/Forward Compatibility

### 7.1 Versioning Strategy

**Version Field** (`u16`):
- Current version: `1`
- Range: [1, 65535]
- Bump on breaking changes

**Compatibility Rules**:

| Scenario | Action |
|----------|--------|
| `file_version == current_version` | Load normally |
| `file_version < current_version` | Load with migration |
| `file_version > current_version` | Reject with error |

### 7.2 Migration Path

**V1 → V2 Example**:
```rust
match loaded_meta.version {
    1 => {
        // Migrate V1 to V2
        let v2_meta = VectorIndexMeta {
            // Copy V1 fields
            ..loaded_meta
            // Add V2-specific fields with defaults
            new_field: default_value,
        };
        v2_meta
    }
    2 => loaded_meta,  // Current version
    _ => return Err(UnsupportedVersion),
}
```

### 7.3 Feature Flags

For optional features, use feature flags in metadata:

```rust
pub struct VectorIndexMeta {
    // ... existing fields ...
    pub features: u64,  // Bitfield for optional features
}

// Feature bits
const FEATURE_QUANTIZATION: u64 = 1 << 0;
const FEATURE_CUSTOM_METRIC: u64 = 1 << 1;
const FEATURE_COMPRESSION: u64 = 1 << 2;
```

**Loading with Features**:
```rust
if meta.features & FEATURE_QUANTIZATION != 0 {
    // Handle quantized index
}

if meta.features & UNSUPPORTED_FEATURES != 0 {
    warn!("Index uses unsupported features, may degrade gracefully");
}
```

### 7.4 Deprecation Policy

When deprecating format versions:
1. **Version N**: Current version, fully supported
2. **Version N-1**: Previous version, load with migration
3. **Version N-2**: Deprecated, warn user to upgrade
4. **Version N-3**: Unsupported, reject with upgrade instructions

**Minimum Support Window**: 2 major versions

## 8. Atomic Write Protocol

To prevent corruption during save operations, use **write-temp-then-rename**:

### 8.1 Save Protocol

```rust
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    // 1. Create temp file in same directory (same filesystem)
    let temp_path = path.with_extension(".tmp");

    // 2. Write data to temp file
    let mut file = File::create(&temp_path)?;
    file.write_all(data)?;
    file.sync_all()?;  // Ensure data hits disk

    // 3. Atomically rename temp to target
    fs::rename(&temp_path, path)?;  // Atomic on POSIX

    Ok(())
}
```

**Properties**:
- **Atomic**: Rename is atomic on POSIX systems
- **No Corruption**: If crash occurs during write, old file remains intact
- **Idempotent**: Can retry safely

### 8.2 Load Protocol

```rust
pub fn load_with_fallback(path: &Path) -> Result<IndexManifest> {
    match load_manifest(path) {
        Ok(manifest) => Ok(manifest),
        Err(e) if e.is_corrupted() => {
            // Try loading from backup
            let backup_path = path.with_extension(".backup");
            if backup_path.exists() {
                load_manifest(&backup_path)
            } else {
                Err(e)
            }
        }
        Err(e) => Err(e),
    }
}
```

### 8.3 Backup Strategy

**Periodic Backups**:
```rust
// Before overwriting, copy to backup
if path.exists() {
    let backup = path.with_extension(".backup");
    fs::copy(path, backup)?;
}

// Then write new version
atomic_write(path, new_data)?;
```

**Retention**:
- Keep last 2 backups
- Rotate on each save
- Clean up old backups

## 9. Security Considerations

### 9.1 DoS Protection

**Size Limits** (reject if exceeded):

| Field | Limit | Reason |
|-------|-------|--------|
| dimensions | 100,000 | Prevent memory exhaustion |
| vector_count | u64::MAX - 1000 | Arithmetic overflow protection |
| property_name | 255 bytes | Path traversal prevention |
| mappings.len() | 1B | Realistic upper bound |
| k (search) | 10,000 | Prevent excessive allocations |

**Validation**:
```rust
if meta.dimensions > MAX_DIMENSIONS {
    return Err(SecurityError::DimensionsTooLarge);
}

if meta.vector_count > MAX_VECTOR_COUNT {
    return Err(SecurityError::VectorCountExceedsLimit);
}
```

### 9.2 Path Traversal Prevention

**Property Names**:
- MUST NOT contain `/`, `\`, `..`, or null bytes
- MUST be valid UTF-8
- MUST match regex: `^[a-zA-Z0-9_-]+$`

**Validation**:
```rust
fn validate_property_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 255 {
        return Err(InvalidPropertyName);
    }

    if name.contains(['/', '\\', '\0', '.']) {
        return Err(PathTraversalAttempt);
    }

    Ok(())
}
```

### 9.3 Memory Exhaustion

**Pre-Allocation Limits**:
```rust
// Before allocating large buffers
let estimated_size = meta.vector_count as usize * meta.dimensions as usize * 4;
if estimated_size > MAX_INDEX_SIZE {
    return Err(IndexTooLarge);
}
```

**Streaming for Large Files**:
```rust
// Instead of loading entire file
let mut reader = BufReader::new(File::open(path)?);
for chunk in reader.chunks(CHUNK_SIZE) {
    process_chunk(chunk?)?;
}
```

## 10. Performance Considerations

### 10.1 Loading Performance

**Optimization Techniques**:

| Technique | Speedup | Use Case |
|-----------|---------|----------|
| Memory-mapped loading | 10-100x | Read-only, >1GB indexes |
| Parallel loading | 3-5x | Multiple properties |
| Metadata-only load | 1000x | Check version/config |
| Incremental loading | 2-3x | Partial index reconstruction |

**Example** (1M vectors, 384 dim, F32):

| Method | Time | Memory |
|--------|------|--------|
| Full deserialization | ~5s | 1.84 GB |
| Memory-mapped | ~50ms | <100 MB |
| Metadata-only | <1ms | <10 KB |

### 10.2 Saving Performance

**Delta Encoding** (incremental saves):
```rust
// Save only changes since last checkpoint
let delta = compute_delta(old_index, new_index);
save_delta(&delta, path)?;  // 60-75% size reduction
```

**Compression**:
```rust
// Zstd compression (level 3)
let compressed = zstd::encode_all(data, 3)?;  // ~40% size reduction
save_with_crc(&compressed, path)?;
```

**Background Saves**:
```rust
// Non-blocking save
tokio::spawn(async move {
    index.save(path).await?;
});
```

### 10.3 Disk Space Optimization

**Quantization**:
- F32 → F16: 50% size reduction, minimal accuracy loss
- F32 → I8: 75% size reduction, moderate accuracy loss

**Snapshot Pruning**:
```rust
// Retention policy: Keep last N snapshots
if snapshot_count > MAX_SNAPSHOTS {
    delete_oldest_snapshot()?;
}
```

**Compression Ratios** (typical):
- Metadata: 60-75% (zstd level 3)
- Mappings: 20-40% (low entropy)
- HNSW index: 10-20% (high entropy)

## 11. Error Handling

### 11.1 Error Types

```rust
pub enum IndexPersistenceError {
    /// File not found
    NotFound { path: PathBuf },

    /// Corrupted index (CRC32 mismatch)
    Corrupted { path: PathBuf, source: String },

    /// Invalid magic bytes
    InvalidMagic { path: PathBuf, expected: [u8; 4], got: [u8; 4] },

    /// Unsupported version
    UnsupportedVersion { found: u16, supported: u16 },

    /// Size limit exceeded
    SizeLimitExceeded { field: String, value: u64, limit: u64 },

    /// IO error
    Io(std::io::Error),

    /// Encoding/decoding error
    Codec(bitcode::Error),
}
```

### 11.2 Recovery Strategies

| Error | Recovery |
|-------|----------|
| `NotFound` | Rebuild from WAL or create empty index |
| `Corrupted` | Load from backup or rebuild |
| `InvalidMagic` | Reject, wrong file type |
| `UnsupportedVersion` | Migrate or reject |
| `SizeLimitExceeded` | Reject, potential attack |
| `Io` | Retry with backoff |
| `Codec` | Reject, malformed data |

### 11.3 Validation Checklist

Before loading an index, validate:

- [ ] File exists and is readable
- [ ] File size is reasonable (>min, <max)
- [ ] CRC32 checksum matches
- [ ] Magic bytes are correct
- [ ] Version is supported
- [ ] Dimensions are within limits
- [ ] Vector count is within limits
- [ ] Property name is valid (no path traversal)
- [ ] Timestamps are reasonable
- [ ] Mappings file exists and matches
- [ ] usearch file exists and matches dimensions

## 12. Implementation Checklist

### Phase 5 Implementation Tasks

- [ ] **Format Structs** (DONE - already in `formats.rs`)
  - [x] `VectorIndexMeta`
  - [x] `VectorMappingsData`
  - [x] `VectorSnapshotMeta`
  - [x] `PersistedHnswConfig`

- [ ] **Save/Load Functions** (DONE - already in `vector.rs`)
  - [x] `save_vector_meta()`
  - [x] `load_vector_meta()`
  - [x] `save_vector_mappings()`
  - [x] `load_vector_mappings()`
  - [x] `save_snapshot_meta()`
  - [x] `load_snapshot_meta()`

- [ ] **HnswIndex Integration**
  - [ ] `HnswIndex::save()` - Save to `.usearch` file
  - [ ] `HnswIndex::load()` - Load from `.usearch` file
  - [ ] `HnswIndex::load_mmap()` - Memory-mapped loading
  - [ ] `HnswIndex::save_with_mappings()` - Save index + mappings atomically

- [ ] **TemporalVectorIndex Integration**
  - [ ] `create_snapshot()` - Create temporal snapshot
  - [ ] `load_snapshot()` - Load snapshot by ID
  - [ ] `prune_snapshots()` - Apply retention policy
  - [ ] `reconstruct_at_time()` - Reconstruct from full + deltas

- [ ] **Persistence Policies**
  - [ ] Trigger on mutation threshold
  - [ ] Trigger on time interval
  - [ ] Background save task
  - [ ] Shutdown hook

- [ ] **Validation**
  - [ ] Size limit checks
  - [ ] Path traversal prevention
  - [ ] Version compatibility checks
  - [ ] CRC32 validation

- [ ] **Error Handling**
  - [ ] Graceful degradation
  - [ ] Backup/restore on corruption
  - [ ] Retry logic for IO errors
  - [ ] User-friendly error messages

- [ ] **Testing**
  - [ ] Round-trip tests (save/load)
  - [ ] Corruption detection tests
  - [ ] Version migration tests
  - [ ] Large index tests (>1GB)
  - [ ] Concurrent access tests
  - [ ] Performance benchmarks

- [ ] **Documentation**
  - [x] This specification document
  - [ ] API documentation (rustdoc)
  - [ ] User guide updates
  - [ ] Migration guide (when V2 released)

## 13. Examples

### 13.1 Save Index

```rust
use aletheiadb::storage::index_persistence::vector::*;
use aletheiadb::index::vector::HnswIndex;

// Create index
let index = HnswIndex::new(384, DistanceMetric::Cosine)?;
// ... add vectors ...

// Prepare metadata
let meta = new_vector_meta("embedding", 384, 0, config.into());

// Prepare mappings
let mappings = index.export_mappings()?;

// Save (atomic)
save_vector_meta(&meta, "indexes/vector/embedding/meta.idx")?;
save_vector_mappings(&mappings, "indexes/vector/embedding/mappings.idx")?;
index.save("indexes/vector/embedding/current.usearch")?;

println!("Saved index with {} vectors", meta.vector_count);
```

### 13.2 Load Index

```rust
// Load metadata first (fast)
let meta = load_vector_meta("indexes/vector/embedding/meta.idx")?;

// Check if we can handle this version
if meta.version > CURRENT_VERSION {
    return Err(UnsupportedVersion);
}

// Load mappings
let mappings = load_vector_mappings("indexes/vector/embedding/mappings.idx")?;

// Load HNSW index
let index = if meta.vector_count > 1_000_000 {
    // Large index: use memory-mapping
    HnswIndex::load_mmap("indexes/vector/embedding/current.usearch")?
} else {
    // Small index: load into RAM
    HnswIndex::load("indexes/vector/embedding/current.usearch")?
};

// Import mappings
index.import_mappings(mappings)?;

println!("Loaded index with {} vectors", meta.vector_count);
```

### 13.3 Temporal Snapshot

```rust
// Create snapshot
let snapshot_id = temporal_index.create_snapshot_for_anchor(timestamp)?;
let snapshot_meta = VectorSnapshotMeta {
    snapshot_id: snapshot_id.unwrap(),
    snapshot_type: PersistedSnapshotType::Full,
    timestamp,
    vector_count: temporal_index.current_index.size(),
    config: temporal_index.config.hnsw_config.clone().into(),
    base_snapshot_id: None,
};

// Save snapshot
let snapshot_path = format!("indexes/vector/embedding/snapshots/snapshot_{}.usearch", snapshot_id.unwrap());
temporal_index.current_index.save(&snapshot_path)?;
save_snapshot_meta(&snapshot_meta, &format!("{}.meta", snapshot_path))?;

// Load snapshot (later)
let loaded_meta = load_snapshot_meta(&format!("{}.meta", snapshot_path))?;
let snapshot = HnswIndex::load(&snapshot_path)?;
```

## 14. References

- [Phase 5: Persistence & Performance](VECTOR_SEARCH_DESIGN.md#phase-5-persistence--performance)
- [Index Persistence Guide](guides/index-persistence-guide.md)
- [ADR-0023: Index Persistence Layer](adr/0023-index-persistence-layer.md)
- [ADR-0022: Multi-Property Vector Index](adr/0022-multi-property-vector-index.md)
- [usearch Documentation](https://github.com/unum-cloud/usearch)
- [Bitcode Serialization](https://github.com/SoftbearStudios/bitcode)

## 15. Appendix: Magic Bytes Registry

| File Type | Magic | ASCII | Hex |
|-----------|-------|-------|-----|
| Manifest | GIDX | "GIDX" | 0x47 0x49 0x44 0x58 |
| String Interner | GSTR | "GSTR" | 0x47 0x53 0x54 0x52 |
| Graph Index | GGRP | "GGRP" | 0x47 0x47 0x52 0x50 |
| Temporal Index | GTMP | "GTMP" | 0x47 0x54 0x4D 0x50 |
| Vector Meta | GVEC | "GVEC" | 0x47 0x56 0x45 0x43 |
| Mappings | GMAP | "GMAP" | 0x47 0x4D 0x41 0x50 |

**Pattern**: All AletheiaDB index files start with "G" (0x47)

## 16. Appendix: Size Reference Table

**Index Size Estimation** (per 1M vectors):

| Dimensions | Quantization | M | Index Size | Mappings | Metadata | Total |
|------------|--------------|---|------------|----------|----------|-------|
| 128 | F32 | 16 | 600 MB | 24 MB | <1 KB | ~624 MB |
| 384 | F32 | 16 | 1.8 GB | 24 MB | <1 KB | ~1.82 GB |
| 768 | F32 | 16 | 3.5 GB | 24 MB | <1 KB | ~3.52 GB |
| 384 | F16 | 16 | 950 MB | 24 MB | <1 KB | ~974 MB |
| 384 | I8 | 16 | 550 MB | 24 MB | <1 KB | ~574 MB |
| 384 | F32 | 32 | 2.0 GB | 24 MB | <1 KB | ~2.02 GB |

**Loading Time Estimation** (SSD):

| Method | 100K vectors | 1M vectors | 10M vectors |
|--------|--------------|------------|-------------|
| Deserialization | ~500ms | ~5s | ~50s |
| Memory-mapped | ~50ms | ~50ms | ~100ms |
| Metadata-only | <1ms | <1ms | <1ms |

## 17. Changelog

| Version | Date | Changes |
|---------|------|---------|
| 1.0 | 2026-01-20 | Initial specification for VS-080 |

---

**Document Status**: ✅ Complete
**Implementation Status**: 🔧 Partial (formats defined, save/load functions exist)
**Next Steps**: Integrate with HnswIndex and TemporalVectorIndex
