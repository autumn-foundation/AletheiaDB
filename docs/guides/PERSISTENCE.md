# Comprehensive Persistence Guide

**Last Updated:** 2026-02-08
**Status:** Stable
**Related:** [ADR-0023](../adr/0023-index-persistence-layer.md), [Tiered Storage Guide](tiered-storage-guide.md)

## Overview

AletheiaDB provides **three complementary persistence systems** for different use cases:

| System | Purpose | What It Stores | Performance Impact |
|--------|---------|----------------|-------------------|
| **WAL** | Transaction durability | All mutations (creates, updates, deletes) | Minimal (async mode) |
| **Index Persistence** | Fast cold starts | Current state indexes | 6-30x faster startup |
| **Cold Storage (Redb)** | Unlimited history | Historical versions (bi-temporal) | Enables unlimited depth |

**⚠️ Common Mistake:** Trying to call `AletheiaDB::open()`. That API does not exist. For restart, use `with_unified_config()` with persistence enabled.

## Current Reality (Important)

As of 2026-02, persistence and recovery behave as follows:

- `StringInterner` is persisted to `indexes/strings/interner.idx` and restored on startup.
- Recovery is driven by `storage::checkpoint::CheckpointManager` (not legacy `storage::persistence`).
- Checkpoints are active and meaningful after LSN work: recovery starts replay at `manifest.lsn + 1`, not from WAL start.
- Old PR text claiming the interner is "memory-only" is stale for the current code path.

## Quick Decision Guide

**Choose your persistence pattern:**

### Pattern 1: Basic Durability (WAL Only)
**Use when:** Data must survive crashes, but restarts can be slow
```rust
let config = AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .wal_dir("data/wal")
        .build())
    .build();
```
- ✅ Data survives crashes
- ❌ Slow startup (WAL replay)
- ❌ History limited by RAM

### Pattern 2: Fast Restarts (WAL + Index Persistence)
**Use when:** You need fast startup and data persistence
```rust
let config = AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .wal_dir("data/wal")
        .build())
    .persistence(PersistenceConfig {
        enabled: true,
        data_dir: "data/indexes".into(),
        load_on_startup: true,
        ..Default::default()
    })
    .build();
```
- ✅ Data survives crashes
- ✅ Fast startup (6-30x faster)
- ❌ History limited by RAM

### Pattern 3: Unlimited History (WAL + Index Persistence + Cold Storage)
**Use when:** You need unlimited bi-temporal history on disk
```rust
let config = AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .wal_dir("data/wal")
        .build())
    .persistence(PersistenceConfig {
        enabled: true,
        data_dir: "data/indexes".into(),
        load_on_startup: true,
        ..Default::default()
    })
    .historical(HistoricalConfigBuilder::new()
        .enable_tiered_storage(true)
        .cold_storage_path("data/cold.redb")
        .build())
    .build();
```
- ✅ Data survives crashes
- ✅ Fast startup
- ✅ Unlimited historical depth

## Part 1: Write-Ahead Log (WAL)

The WAL ensures **transaction durability** by logging all mutations before applying them.

### Basic Setup

```rust
use aletheiadb::config::WalConfigBuilder;
use aletheiadb::storage::wal::DurabilityMode;

let config = AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .wal_dir("data/wal")  // Where to store WAL files
        .durability_mode(DurabilityMode::GroupCommit {
            max_delay_ms: 10,
            max_batch_size: 200,
        })
        .build())
    .build();
```

### Durability Modes

| Mode | Latency | Throughput | Durability | Use Case |
|------|---------|------------|------------|----------|
| **Synchronous** | ~1.5ms | ~600/sec | Full ACID | Critical data |
| **GroupCommit** | ~10-50ms | ~100K+/sec | Full ACID | Production (recommended) |
| **Async** | <100ns | ~500K+/sec | Eventual | Bulk loading |

### WAL Directory Structure

```
data/wal/
├── 000001.log        # WAL segment files
├── 000002.log
├── 000003.log
└── manifest.json     # Segment metadata
```

### Configuration Options

```rust
WalConfigBuilder::new()
    .wal_dir("data/wal")                    // Directory for WAL files
    .num_stripes(16)                        // Concurrent append stripes (power of 2)
    .stripe_capacity(1024)                  // Ring buffer capacity per stripe
    .segment_size(64 * 1024 * 1024)        // 64MB segment size
    .segments_to_retain(10)                // Keep last 10 segments
    .durability_mode(DurabilityMode::GroupCommit {
        max_delay_ms: 10,
        max_batch_size: 200,
    })
    .build()
```

**See [docs/WAL.md](../WAL.md) for complete WAL documentation.**

## Part 2: Index Persistence

Index persistence saves the **current state** to disk for fast restarts (6-30x faster than WAL replay).

### What Gets Persisted

- **Graph indexes** - Adjacency lists (CSR format, Zstd compressed)
- **Temporal indexes** - Version chains (Zstd compressed)
- **Vector indexes** - HNSW k-NN indexes (binary format)
- **String interner** - Deduplicated labels and property keys

### Basic Setup

```rust
use aletheiadb::storage::index_persistence::PersistenceConfig;
use std::time::Duration;

let config = AletheiaDBConfig::builder()
    .persistence(PersistenceConfig {
        enabled: true,
        data_dir: "data/indexes".into(),
        load_on_startup: true,
        auto_persist_interval: Duration::from_secs(300),  // Save every 5min
        compression_level: 3,  // Zstd compression (1-22)
        ..Default::default()
    })
    .build();
```

### Directory Structure

```
data/indexes/
├── manifest.idx              # Index registry + LSN
├── strings/
│   └── interner.idx          # String deduplication
├── graph/
│   └── adjacency.idx.zst     # Graph structure (compressed)
├── temporal/
│   └── versions.idx.zst      # Version chains (compressed)
└── vector/
    └── embedding/
        ├── meta.idx
        ├── mappings.idx
        └── current.usearch   # HNSW index
```

### Performance Characteristics

| Dataset Size | WAL Replay Time | Index Load Time | Speedup |
|--------------|-----------------|-----------------|---------|
| 100K nodes | 5s | 0.3s | 16x |
| 1M nodes | 60s | 2s | 30x |
| 10M nodes | 10min | 20s | 30x |

### Advanced Configuration

```rust
PersistenceConfig {
    enabled: true,
    data_dir: "data/indexes".into(),
    load_on_startup: true,
    auto_persist_interval: Duration::from_secs(300),

    // Compression
    compression_level: 3,  // 1 (fast) to 22 (max compression)

    // Memory-mapped loading for large indexes
    use_mmap: true,

    // Parallel loading
    parallel_load: true,

    // Safety
    max_file_size: 10 * 1024 * 1024 * 1024,  // 10GB max per index file
    verify_checksums: true,  // CRC32 verification on load

    ..Default::default()
}
```

## Part 3: Cold Storage (Redb)

Cold storage moves **historical versions** to disk, enabling unlimited bi-temporal history without consuming RAM.

### What Gets Stored

- **Historical node versions** - Old property values, labels
- **Historical edge versions** - Old edge properties
- **Bi-temporal metadata** - Valid time, transaction time ranges

### Three-Tier Architecture

```
┌─────────────────────────────────────────┐
│ Hot Tier (RAM)                          │
│ - Current state                         │
│ - Recent history (configurable)         │
│ - Lookup: 22-70ns                       │
└─────────────────┬───────────────────────┘
                  │
┌─────────────────▼───────────────────────┐
│ Warm Tier (LRU Cache)                   │
│ - Recently accessed history             │
│ - Lookup: <1µs                          │
└─────────────────┬───────────────────────┘
                  │
┌─────────────────▼───────────────────────┐
│ Cold Tier (Redb on Disk)                │
│ - Old historical versions               │
│ - Zstd/LZ4 compressed (3-5x ratio)      │
│ - Lookup: <1ms                          │
└─────────────────────────────────────────┘
```

### Basic Setup

```rust
use aletheiadb::config::HistoricalConfigBuilder;

let config = AletheiaDBConfig::builder()
    .historical(HistoricalConfigBuilder::new()
        .enable_tiered_storage(true)
        .cold_storage_path("data/cold.redb")
        .migration_age_threshold(Duration::from_secs(3600))  // Move to cold after 1hr
        .max_hot_versions(1000)  // Keep 1000 versions in RAM per entity
        .build())
    .build();
```

### Advanced Configuration

```rust
HistoricalConfigBuilder::new()
    .enable_tiered_storage(true)
    .cold_storage_path("data/cold.redb")

    // Migration policies
    .migration_age_threshold(Duration::from_secs(3600))  // Age-based migration
    .max_hot_versions(1000)  // Version count threshold
    .memory_pressure_threshold(0.8)  // Migrate at 80% memory

    // Compression
    .compression(CompressionType::Zstd)
    .compression_level(3)

    // Performance
    .batch_migration_size(100)  // Migrate in batches of 100
    .background_migration_interval(Duration::from_secs(60))

    .build()
```

### Cold Storage File

```
data/cold.redb          # Single Redb database file
```

**Redb Features:**
- Pure Rust (no FFI dependencies)
- ACID transactions
- Crash-safe
- Memory-mapped reads
- Concurrent readers, single writer

### When to Use Cold Storage

**Use cold storage if:**
- ✅ You need unlimited bi-temporal history
- ✅ You query historical data infrequently
- ✅ RAM is limited but disk space is available
- ✅ You want to track data evolution over months/years

**Skip cold storage if:**
- ❌ You only care about current state
- ❌ All history fits comfortably in RAM
- ❌ You frequently query all historical versions

**See [docs/guides/tiered-storage-guide.md](tiered-storage-guide.md) for complete cold storage documentation.**

## Complete Example: All Three Systems

```rust
use aletheiadb::{AletheiaDB, AletheiaDBConfig};
use aletheiadb::config::{WalConfigBuilder, HistoricalConfigBuilder};
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::storage::wal::DurabilityMode;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = std::env::current_dir()?.join("my-app-data");

    let config = AletheiaDBConfig::builder()
        // 1. WAL for transaction durability
        .wal(WalConfigBuilder::new()
            .wal_dir(db_path.join("wal"))
            .durability_mode(DurabilityMode::GroupCommit {
                max_delay_ms: 10,
                max_batch_size: 200,
            })
            .build())

        // 2. Index persistence for fast restarts
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: db_path.join("indexes"),
            load_on_startup: true,
            auto_persist_interval: Duration::from_secs(300),
            compression_level: 3,
            ..Default::default()
        })

        // 3. Cold storage for unlimited history
        .historical(HistoricalConfigBuilder::new()
            .enable_tiered_storage(true)
            .cold_storage_path(db_path.join("cold.redb"))
            .migration_age_threshold(Duration::from_secs(3600))
            .max_hot_versions(1000)
            .build())

        .build();

    // Creates all directories automatically!
    let db = AletheiaDB::with_unified_config(config)?;

    println!("✅ Database initialized with full persistence");

    Ok(())
}
```

### File Structure (All Three)

```
my-app-data/
├── wal/                    # WAL (transaction durability)
│   ├── 000001.log
│   ├── 000002.log
│   └── manifest.json
├── indexes/                # Index persistence (fast restarts)
│   ├── manifest.idx
│   ├── strings/
│   │   └── interner.idx
│   ├── graph/
│   │   └── adjacency.idx.zst
│   ├── temporal/
│   │   └── versions.idx.zst
│   └── vector/
│       └── embedding/
│           └── current.usearch
└── cold.redb               # Cold storage (unlimited history)
```

## Common Patterns

### Pattern: Production Web Service

```rust
// Fast restarts + ACID durability + reasonable history depth
AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .wal_dir("data/wal")
        .durability_mode(DurabilityMode::GroupCommit {
            max_delay_ms: 10,
            max_batch_size: 200,
        })
        .build())
    .persistence(PersistenceConfig {
        enabled: true,
        data_dir: "data/indexes".into(),
        load_on_startup: true,
        auto_persist_interval: Duration::from_secs(300),
        ..Default::default()
    })
    .build()
```

### Pattern: Temporal Analytics Platform

```rust
// Unlimited history + fast queries + time-travel
AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .wal_dir("data/wal")
        .durability_mode(DurabilityMode::Async { flush_interval_ms: 100 })
        .build())
    .persistence(PersistenceConfig {
        enabled: true,
        data_dir: "data/indexes".into(),
        load_on_startup: true,
        ..Default::default()
    })
    .historical(HistoricalConfigBuilder::new()
        .enable_tiered_storage(true)
        .cold_storage_path("data/cold.redb")
        .migration_age_threshold(Duration::from_secs(3600))
        .build())
    .build()
```

### Pattern: Bulk Data Loading

```rust
// Maximum throughput during import, then switch to production mode
let import_config = AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .wal_dir("data/wal")
        .durability_mode(DurabilityMode::Async { flush_interval_ms: 1000 })
        .build())
    .persistence(PersistenceConfig {
        enabled: false,  // Disable during import
        ..Default::default()
    })
    .build();

// After import: save indexes once, then switch to production config
db.persist_indexes()?;
```

## Troubleshooting

### "Failed to load persisted indexes"

**Error:**
```
Storage error: Index file missing or unreadable:
The system cannot find the file specified. (os error 2)
```

**Cause:** Persistence files are missing/corrupted, or startup path is misconfigured.

**Solution:** Use `with_unified_config()` with `PersistenceConfig { enabled: true, load_on_startup: true, .. }` and verify `data_dir` points to the directory that contains `indexes/`.

### "Interned IDs became invalid after restart"

**Symptom:** A review or old note claims interned labels/keys are memory-only.

**Current behavior:** Interned strings are persisted in `indexes/strings/interner.idx` and restored during startup.

**What to verify:**
- Persistence is enabled and points to the same `data_dir` on restart.
- `load_on_startup` is `true`.
- Recovery path is `CheckpointManager` (`storage::checkpoint`), not legacy `storage::persistence`.

### "Access is denied" (Windows)

**Error:**
```
Storage error: I/O error while reading persisted index file:
Access is denied. (os error 5)
```

**Causes:**
- Another process has the database open
- Insufficient permissions on data directory
- Antivirus blocking file access

**Solutions:**
- Ensure only one process accesses the database
- Check directory permissions
- Add data directory to antivirus exclusions

### Slow Startup Without Index Persistence

If startup is slow (>30s for 1M nodes), enable index persistence:

```rust
.persistence(PersistenceConfig {
    enabled: true,  // ← Enable this!
    data_dir: "data/indexes".into(),
    load_on_startup: true,
    ..Default::default()
})
```

### RAM Usage Growing Unbounded

If RAM usage grows without limit, enable cold storage:

```rust
.historical(HistoricalConfigBuilder::new()
    .enable_tiered_storage(true)  // ← Enable this!
    .cold_storage_path("data/cold.redb")
    .migration_age_threshold(Duration::from_secs(3600))
    .build())
```

## Related Documentation

- **[WAL.md](../WAL.md)** - Complete WAL documentation
- **[tiered-storage-guide.md](tiered-storage-guide.md)** - Cold storage deep dive
- **[index-persistence-guide.md](index-persistence-guide.md)** - Index persistence details
- **[CONFIGURATION.md](../CONFIGURATION.md)** - All configuration options
- **[examples/file_based_persistence.rs](../../examples/file_based_persistence.rs)** - Working example

## Summary

| Feature | Purpose | File Size | Startup Impact | Query Impact |
|---------|---------|-----------|----------------|--------------|
| **WAL** | Crash recovery | ~1-10MB per segment | Full replay without indexes | None |
| **Index Persistence** | Fast restarts | 60-75% smaller (compressed) | 6-30x faster | None |
| **Cold Storage** | Unlimited history | Depends on history depth | None (lazy load) | <1ms for cold data |

**Recommended Default:** WAL + Index Persistence for most use cases. Add Cold Storage when you need unlimited temporal depth.
