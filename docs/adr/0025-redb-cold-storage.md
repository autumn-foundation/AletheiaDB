# ADR-0025: Redb Cold Storage and LSN-Based WAL Truncation

**Status:** Accepted
**Date:** 2026-01-24
**Deciders:** AletheiaDB Core Team
**Categories:** storage, durability, persistence, architecture
**Supersedes:** ADR-0013 (partial - cold storage implementation details)

## Context

AletheiaDB's tiered storage architecture (ADR-0013) requires a cold storage backend for persisting historical versions that have been evicted from the warm tier. The original implementation used RocksDB, which has several challenges:

**RocksDB Challenges:**
1. **FFI Complexity**: RocksDB is a C++ library requiring FFI bindings, adding build complexity
2. **Cross-Platform Issues**: Build difficulties on Windows and ARM architectures
3. **Large Dependency**: Significant binary size and compile time overhead
4. **Configuration Complexity**: LSM-tree tuning requires expertise for optimal performance
5. **No Unified LSN Tracking**: WAL truncation and cold storage were not coordinated

**Current Architecture Gap:**

```
Write → WAL (durable) → Hot (current state)
  ↓
Update → WAL → Hot updated, old version → Warm (LRU cache)
  ↓
Eviction → Batch flush to RocksDB → WAL grows unbounded (no truncation)
```

The WAL cannot be safely truncated because there's no coordination between what has been durably persisted to cold storage and what the WAL contains. This leads to:
- Unbounded WAL growth
- Slow recovery (full WAL replay)
- No clear data lifecycle

## Decision

We will replace RocksDB with **Redb** and implement **LSN-based WAL truncation** to create a unified persistence architecture.

### Architecture Overview

```
Write → WAL (durable) → Hot (current state)
  ↓
Update → WAL → Hot updated, old version → Warm (LRU cache)
  ↓
Eviction → Batch flush to Redb → Record flushed_lsn → Truncate WAL
```

**Key Invariant:** `WAL_truncation_lsn <= Redb_flushed_lsn` (always)

This invariant ensures that any operation truncated from the WAL has been durably persisted to Redb, enabling crash recovery.

### Why Redb

1. **Pure Rust**: No FFI, native cross-platform support
2. **Simple API**: ACID transactions with minimal configuration
3. **Embedded**: Single-file database, easy deployment
4. **Crash-Safe**: Copy-on-write B-trees with checksums
5. **Good Performance**: Comparable read performance to RocksDB for our access patterns
6. **Small Footprint**: Much smaller binary size and compile time

### Redb Storage Schema

```rust
// Table definitions
const NODE_VERSIONS: TableDefinition<u64, &[u8]> = TableDefinition::new("node_versions");
const EDGE_VERSIONS: TableDefinition<u64, &[u8]> = TableDefinition::new("edge_versions");
const METADATA: TableDefinition<&str, &[u8]> = TableDefinition::new("metadata");

// Metadata keys
const FLUSHED_LSN_KEY: &str = "flushed_lsn";
```

### LSN-Based WAL Truncation Flow

```
1. Warm cache evicts batch of versions
   - Versions tagged with their originating LSN
   - Batch collected: [(version, lsn), ...]

2. Atomic write to Redb
   - Begin transaction
   - Write all versions to node_versions/edge_versions tables
   - Update flushed_lsn = max(current_flushed_lsn, batch_max_lsn)
   - Commit transaction (atomic)

3. On successful commit: truncate WAL
   - flush_coordinator.truncate_to_lsn(flushed_lsn)
   - Remove WAL segments where max_lsn < flushed_lsn

4. On failure: no truncation
   - WAL remains intact
   - Next eviction batch will retry
```

### Recovery Flow

```
1. Open Redb database
   - Read flushed_lsn from metadata table
   - If no flushed_lsn: cold storage empty, full WAL replay

2. Initialize hot tier (empty)

3. Replay WAL from flushed_lsn + 1
   - Skip entries with LSN <= flushed_lsn (already in Redb)
   - Apply remaining entries to rebuild hot tier

4. Ready for queries
   - Hot tier has current state
   - Cold tier (Redb) has historical versions
   - Warm cache starts empty, fills on demand
```

### Extended ColdStorage Trait

```rust
pub trait ColdStorage: Send + Sync {
    // Existing methods unchanged...

    /// Get the highest LSN that has been durably flushed to cold storage.
    /// Returns None if no data has been flushed yet.
    fn get_flushed_lsn(&self) -> Result<Option<LSN>> {
        Ok(None) // Default: no LSN tracking
    }

    /// Store a batch of versions with LSN tracking.
    /// The flushed_lsn is updated atomically with the batch write.
    fn store_batch_with_lsn(
        &self,
        nodes: &[NodeVersion],
        edges: &[EdgeVersion],
        lsn: LSN,
    ) -> Result<()> {
        // Default: delegate to existing batch methods, ignore LSN
        self.store_node_versions_batch(nodes)?;
        self.store_edge_versions_batch(edges)?;
        Ok(())
    }
}
```

### WAL Segment LSN Tracking

Each WAL segment header now includes:

```rust
struct SegmentHeader {
    magic: [u8; 4],      // "GWAL"
    version: u8,         // Format version
    min_lsn: u64,        // Minimum LSN in this segment
    max_lsn: u64,        // Maximum LSN in this segment
}
```

This enables efficient truncation:
```rust
impl FlushCoordinator {
    /// Truncate all WAL segments where max_lsn < threshold_lsn.
    /// The active segment is never truncated.
    pub fn truncate_to_lsn(&self, threshold_lsn: LSN) -> Result<u64> {
        let mut removed = 0;
        for segment in self.completed_segments() {
            if segment.max_lsn < threshold_lsn {
                segment.delete()?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}
```

## Consequences

### Positive

1. **Pure Rust Stack**: No FFI dependencies, simpler builds, better cross-platform support
2. **Bounded WAL**: WAL size bounded by migration frequency, not history depth
3. **Faster Recovery**: Replay from checkpoint LSN, not full history
4. **Clear Data Lifecycle**: Write → WAL → Hot → Warm → Cold → WAL truncated
5. **Simpler Configuration**: Redb requires minimal tuning vs RocksDB's many knobs
6. **Crash Safety**: Atomic flushed_lsn update ensures safe truncation
7. **Smaller Binaries**: Redb adds ~500KB vs RocksDB's ~10MB

### Negative

1. **Migration Effort**: Existing RocksDB users need to migrate data
2. **Write Performance**: Redb B-trees may be slower than RocksDB's LSM for pure writes
3. **Limited Ecosystem**: Fewer tools and community resources than RocksDB
4. **No Built-in Compression**: Must handle compression ourselves (already do with Zstd)

### Neutral

1. **Read Performance**: Similar for our point-lookup access patterns
2. **Memory Usage**: Both use mmap-based approaches
3. **Concurrent Access**: Both support concurrent readers, single writer

## Alternatives Considered

### Alternative 1: Keep RocksDB with LSN Tracking

Add LSN tracking to the existing RocksDB implementation.

**Rejected because:**
- Still has FFI complexity and build issues
- Doesn't address the fundamental maintenance burden
- Would require significant work anyway for LSN coordination

### Alternative 2: SQLite

Use SQLite as the cold storage backend.

**Rejected because:**
- SQL interface adds unnecessary overhead for key-value access
- Blob handling less efficient than native key-value stores
- Write amplification from B-tree structure

### Alternative 3: Custom File Format

Implement our own append-only file format.

**Rejected because:**
- Significant engineering effort for crash safety
- Would need to implement compaction, checksums, etc.
- Redb already solves these problems well

### Alternative 4: LMDB

Use LMDB (Lightning Memory-Mapped Database).

**Rejected because:**
- C library requiring FFI (same issue as RocksDB)
- Single-writer limitation more restrictive
- Less active development than Redb

## Implementation Notes

### Phase 1: Infrastructure
- Add `redb` dependency to Cargo.toml
- Implement `RedbColdStorage` with `ColdStorage` trait
- Add LSN tracking methods to `ColdStorage` trait

### Phase 2: WAL Integration
- Add min/max LSN to segment headers
- Implement `truncate_to_lsn()` in FlushCoordinator
- Update segment reader to parse LSN headers

### Phase 3: Migration Flow
- Update MigrationService to use `store_batch_with_lsn()`
- Wire eviction → flush → truncation pipeline
- Add metrics for LSN tracking and truncation

### Phase 4: Recovery
- Update CheckpointManager to read flushed_lsn
- Modify WAL replay to start from flushed_lsn + 1
- Add validation for LSN consistency

### Phase 5: Cleanup
- Remove RocksDB dependency and feature flag
- Update documentation
- Migration guide for existing users
- Removed `FileColdStorage` as it was redundant and inferior to `RedbColdStorage` (Completed 2026-01-24)

### Configuration

```rust
pub struct RedbConfig {
    /// Path to the database file
    pub path: PathBuf,
    /// Compression algorithm for values (applied before Redb storage)
    pub compression: CompressionAlgorithm,
    /// Enable checksums (in addition to Redb's built-in checksums)
    pub enable_checksums: bool,
    /// Cache size for Redb (0 = default)
    pub cache_size_bytes: usize,
}

impl Default for RedbConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("data/cold.redb"),
            compression: CompressionAlgorithm::Zstd,
            enable_checksums: true,
            cache_size_bytes: 0, // Use Redb default
        }
    }
}
```

## References

- **ADR-0007**: Write-Ahead Log for Durability
- **ADR-0012**: Configurable Durability Modes
- **ADR-0013**: Tiered Storage Architecture
- **ADR-0020**: Concurrent WAL Architecture
- [Redb Documentation](https://docs.rs/redb/)
- [Redb Design](https://www.redb.org/design.html)
- [RocksDB vs Redb Benchmarks](https://github.com/cberner/redb/blob/master/BENCHMARKS.md)
