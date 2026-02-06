# Index Persistence Guide

**Last Updated:** 2026-01-15
**Status:** Stable
**Related:** [ADR-0023](../adr/0023-index-persistence-layer.md), [Design Doc](../plans/2026-01-15-index-persistence-design.md)

## Overview

AletheiaDB's index persistence layer enables **fast cold starts** by saving all indexes to disk, eliminating the need for full WAL replay on every restart.

**Benefits:**
- ⚡ **Fast Cold Starts**: 2-5 seconds instead of 30-60 seconds for 1M nodes
- 💾 **Reduced Memory Pressure**: Indexes load directly from disk
- 🛡️ **Data Integrity**: CRC32 checksums and atomic writes
- 🔒 **Security**: Built-in DoS protection

**Supported Indexes:**
- ✅ Vector indexes (HNSW k-NN search)
- ✅ Graph indexes (CSR adjacency)
- ✅ Temporal indexes (bi-temporal version chains)
- ✅ String interner (label/property key deduplication)

## Quick Start

### Basic Usage

```rust
use aletheiadb::AletheiaDB;
use aletheiadb::storage::index_persistence::IndexPersistenceManager;

// Create database
let db = AletheiaDB::new();

// Set up persistence
let manager = IndexPersistenceManager::new("data/my-database");
manager.ensure_directories()?;

// ... work with the database ...

// Save indexes
manager.save_string_interner()?;
let manifest = db.create_manifest()?;  // Get current LSN
manager.save_manifest(&manifest)?;

// Later: Load indexes on startup
if manager.indexes_exist() {
    let manifest = manager.load_manifest_and_strings()?;
    println!("Loaded database at LSN {}", manifest.lsn);
}
```

### Automatic Persistence (Recommended)

For production use, enable automatic background persistence:

```rust
use aletheiadb::AletheiaDB;
use aletheiadb::config::PersistenceConfig;

let config = PersistenceConfig {
    enabled: true,
    base_path: "data/my-database".into(),
    save_interval_secs: 300,  // Save every 5 minutes
    save_on_shutdown: true,
};

let db = AletheiaDB::with_persistence_config(config)?;

// Database automatically saves indexes in the background
// No manual save calls needed!
```

## Configuration

### Persistence Directory Structure

When you specify a base path like `"data/my-database"`, AletheiaDB creates:

```
data/my-database/
├── indexes/
│   ├── manifest.idx          # Index registry + LSN
│   ├── strings/
│   │   └── interner.idx      # String deduplication
│   ├── graph/
│   │   └── adjacency.idx     # Graph structure
│   ├── temporal/
│   │   └── versions.idx      # Version chains
│   └── vector/
│       ├── embedding/
│       │   ├── meta.idx
│       │   ├── mappings.idx
│       │   └── current.usearch
│       └── title_embedding/
│           ├── meta.idx
│           ├── mappings.idx
│           └── current.usearch
└── wal/                       # Write-ahead log (separate)
    └── ...
```

### Persistence Config Options

```rust
pub struct PersistenceConfig {
    /// Enable index persistence
    pub enabled: bool,

    /// Base directory for index files
    pub base_path: PathBuf,

    /// How often to save indexes (seconds)
    pub save_interval_secs: u64,

    /// Save on clean shutdown
    pub save_on_shutdown: bool,

    /// Save after N transactions
    pub save_after_transactions: Option<u64>,
}
```

**Recommended Settings:**

| Environment | Interval | Save on Shutdown | Notes |
|-------------|----------|------------------|-------|
| Development | 60s | true | Frequent saves for testing |
| Production (Low Write) | 300s | true | 5-minute saves |
| Production (High Write) | 600s | true | 10-minute saves |
| Test/CI | disabled | false | Use in-memory for speed |

## Usage Patterns

### Pattern 1: Manual Save/Load

**Use Case:** Full control over when indexes are persisted

```rust
use aletheiadb::storage::index_persistence::IndexPersistenceManager;

let manager = IndexPersistenceManager::new("data/my-db");
manager.ensure_directories()?;

// Save indexes manually
fn save_indexes(db: &AletheiaDB, manager: &IndexPersistenceManager) -> Result<()> {
    // 1. Save string interner (always first)
    manager.save_string_interner()?;

    // 2. Save manifest with current LSN
    let manifest = db.create_manifest()?;
    manager.save_manifest(&manifest)?;

    // 3. Save graph index (if present)
    if let Some(graph_data) = db.export_graph_index()? {
        save_graph_index(&graph_data, &manager.graph_path().join("adjacency.idx"))?;
    }

    // 4. Save temporal index (if present)
    if let Some(temporal_data) = db.export_temporal_index()? {
        save_temporal_index(&temporal_data, &manager.temporal_path().join("versions.idx"))?;
    }

    // 5. Vector indexes save themselves automatically

    println!("Indexes saved at LSN {}", manifest.lsn);
    Ok(())
}

// Load indexes on startup
fn load_indexes(db: &mut AletheiaDB, manager: &IndexPersistenceManager) -> Result<()> {
    if !manager.indexes_exist() {
        println!("No persisted indexes found, starting fresh");
        return Ok(());
    }

    let manifest = manager.load_manifest_and_strings()?;
    println!("Loaded indexes at LSN {}", manifest.lsn);

    // Import indexes back into database
    db.import_graph_index(&manager.graph_path().join("adjacency.idx"))?;
    db.import_temporal_index(&manager.temporal_path().join("versions.idx"))?;
    // Vector indexes load automatically

    Ok(())
}
```

### Pattern 2: Periodic Background Saves

**Use Case:** Automated persistence without blocking operations

```rust
use std::time::Duration;
use std::sync::Arc;
use tokio::time;

async fn run_periodic_save(
    db: Arc<AletheiaDB>,
    manager: Arc<IndexPersistenceManager>,
    interval: Duration,
) {
    let mut ticker = time::interval(interval);

    loop {
        ticker.tick().await;

        match save_indexes(&db, &manager) {
            Ok(_) => println!("Background save completed"),
            Err(e) => eprintln!("Background save failed: {}", e),
        }
    }
}

// Usage
#[tokio::main]
async fn main() {
    let db = Arc::new(AletheiaDB::new());
    let manager = Arc::new(IndexPersistenceManager::new("data/my-db"));

    // Spawn background save task
    tokio::spawn(run_periodic_save(
        Arc::clone(&db),
        Arc::clone(&manager),
        Duration::from_secs(300),  // Save every 5 minutes
    ));

    // Application continues...
}
```

### Pattern 3: Save on Shutdown

**Use Case:** Ensure latest state is persisted when application exits

```rust
use signal_hook::consts::SIGTERM;
use signal_hook::iterator::Signals;

fn setup_shutdown_handler(
    db: Arc<AletheiaDB>,
    manager: Arc<IndexPersistenceManager>,
) {
    let mut signals = Signals::new(&[SIGTERM]).unwrap();

    std::thread::spawn(move || {
        for sig in signals.forever() {
            println!("Received signal {:?}, saving indexes...", sig);
            if let Err(e) = save_indexes(&db, &manager) {
                eprintln!("Failed to save on shutdown: {}", e);
            } else {
                println!("Indexes saved successfully");
            }
            std::process::exit(0);
        }
    });
}
```

## Performance Optimization

### Cold Start Performance

**Benchmark:** 1M nodes, 5M edges, 1 vector index (384 dimensions)

| Scenario | Time | Notes |
|----------|------|-------|
| No indexes (WAL replay) | 30-60s | Full reconstruction |
| With indexes | 2-5s | Direct load from disk |
| **Speedup** | **6-30x** | Depends on data size |

**Tips for Faster Cold Starts:**

1. **Use parallel loading** - `load_indexes_parallel()` loads graph, temporal, and vector indexes concurrently (~3x faster)
2. **Use memory-mapped loading** - `load_graph_index_mmap()` for multi-GB indexes that exceed available RAM
3. **Use SSD storage** for index files
4. **Enable multiple vector indexes** (they load in parallel)
5. **Keep manifest small** (it loads first)
6. **Prewarm the page cache** (OS-level optimization)

### Save Performance

| Index Type | Size (1M nodes) | Save Time | Notes |
|------------|-----------------|-----------|-------|
| Manifest | <1KB | <1ms | Tiny, always fast |
| String Interner | 100KB-10MB | 10-50ms | Depends on string count |
| Graph Index | 500MB-2GB | 500-1000ms | Largest index |
| Temporal Index | 200MB-1GB | 200-500ms | Depends on version count |
| Vector Index | 300MB-1GB | 300-800ms | Per property |

**Total Save Time:** ~1-3 seconds for full database

**Tips for Faster Saves:**

1. **Use delta encoding** - `save_graph_index_delta()` for incremental saves (60-75% size reduction)
2. **Use compression** - `save_graph_index_compressed()` with zstd (levels 0-22) reduces disk I/O
3. **Save less frequently** in production (5-10 minute intervals)
4. **Use background threads** to avoid blocking main operations
5. **Disable temporal indexes** if not needed (reduces save time)
6. **Limit vector properties** (each property adds save overhead)

### Disk Space

**Overhead:** ~1.5x raw data size

| Data Size | Index Size | Total |
|-----------|------------|-------|
| 100MB | 150MB | 250MB |
| 1GB | 1.5GB | 2.5GB |
| 10GB | 15GB | 25GB |

**Why Overhead?**
- Bitcode encoding (compact but not zero)
- CRC32 checksums (4 bytes per file)
- Metadata and mappings
- HNSW index structure overhead

## Error Handling

### Common Errors and Solutions

#### 1. Corrupted Index File

**Error:**
```
Error: Index file corrupted: data/my-db/indexes/manifest.idx
  Caused by: CRC32 checksum mismatch: expected 1234, got 5678
```

**Cause:** Bit rot, disk error, or crash during write

**Solution:**
```rust
// Option 1: Delete corrupted file and rebuild from WAL
fs::remove_file("data/my-db/indexes/manifest.idx")?;
db.rebuild_indexes_from_wal()?;

// Option 2: Restore from backup
restore_from_backup("data/my-db/indexes/")?;
```

#### 2. Size Limit Exceeded

**Error:**
```
Error: Size limit exceeded: Vector dimension 150000 exceeds maximum allowed dimension 100000
```

**Cause:** Malformed index file or attack attempt

**Solution:**
```rust
// Delete the malformed file
fs::remove_file("data/my-db/indexes/vector/embedding/mappings.idx")?;

// Rebuild vector index
db.rebuild_vector_index("embedding")?;
```

#### 3. Missing Index File

**Error:**
```
Error: Missing required index file: manifest.idx
```

**Cause:** First startup or incomplete save

**Solution:**
```rust
if !manager.indexes_exist() {
    println!("No indexes found, starting fresh");
    // Database will rebuild from WAL automatically
}
```

#### 4. Version Mismatch

**Error:**
```
Error: Manifest version 2 not supported (max supported: 1)
```

**Cause:** Index file from newer AletheiaDB version

**Solution:**
```
Upgrade AletheiaDB to a version that supports format v2
or rebuild indexes from WAL with current version
```

### Error Recovery Strategy

```rust
fn load_with_fallback(
    db: &mut AletheiaDB,
    manager: &IndexPersistenceManager,
) -> Result<()> {
    match manager.load_manifest_and_strings() {
        Ok(manifest) => {
            println!("Loaded indexes at LSN {}", manifest.lsn);
            db.import_indexes(&manager)?;
        }
        Err(e) => {
            eprintln!("Failed to load indexes: {}", e);
            eprintln!("Falling back to WAL replay");
            db.rebuild_from_wal()?;
        }
    }
    Ok(())
}
```

## Best Practices

### 1. Save Frequency

**Too Frequent (< 30s):**
- ❌ High disk I/O
- ❌ Can impact write performance
- ❌ Unnecessary for most workloads

**Recommended (5-10 minutes):**
- ✅ Good balance of freshness and performance
- ✅ Minimal impact on writes
- ✅ Acceptable recovery time (replay 5-10 min of WAL)

**Too Infrequent (> 1 hour):**
- ❌ Long recovery time on crash
- ❌ Risk of large WAL replay
- ⚠️ Acceptable for read-heavy workloads

### 2. Backup Strategy

**Recommended Approach:**

```bash
# 1. Save indexes
aletheiadb save-indexes --path data/my-db

# 2. Backup the entire indexes directory
tar -czf backup-$(date +%Y%m%d-%H%M%S).tar.gz data/my-db/indexes/

# 3. Upload to cloud storage
aws s3 cp backup-*.tar.gz s3://my-backups/aletheiadb/

# 4. Keep WAL segments for point-in-time recovery
tar -czf wal-$(date +%Y%m%d-%H%M%S).tar.gz data/my-db/wal/
```

### 3. Monitoring

**Key Metrics to Track:**

```rust
use aletheiadb::metrics::PersistenceMetrics;

let metrics = db.persistence_metrics()?;

println!("Last save: {:?}", metrics.last_save_time);
println!("Save duration: {:?}", metrics.last_save_duration);
println!("Index size: {} MB", metrics.total_index_size_mb);
println!("LSN: {}", metrics.current_lsn);
```

**Alert Thresholds:**
- Save duration > 30s: Investigate performance
- Days since last save > 1: Check background save task
- Index size growing > 2x data: Possible corruption

### 4. Development vs Production

**Development:**
```rust
PersistenceConfig {
    enabled: true,
    save_interval_secs: 60,      // Save every minute
    save_on_shutdown: true,       // Always save on exit
    ..Default::default()
}
```

**Production:**
```rust
PersistenceConfig {
    enabled: true,
    save_interval_secs: 300,     // Save every 5 minutes
    save_on_shutdown: true,       // Always save on exit
    save_after_transactions: Some(10000),  // Also save after 10K txns
    ..Default::default()
}
```

**Testing/CI:**
```rust
PersistenceConfig {
    enabled: false,              // Disable for speed
    ..Default::default()
}
```

## Troubleshooting

### Index Files Keep Growing

**Symptom:** Index directory size increases indefinitely

**Diagnosis:**
```bash
du -sh data/my-db/indexes/
# If much larger than expected...

# Check for old snapshot files
find data/my-db/indexes/vector/ -name "snapshot_*" | wc -l
```

**Solution:**
```rust
// Clean up old vector snapshots (keep last 10)
db.cleanup_vector_snapshots(10)?;

// Or disable temporal vector snapshots if not needed
db.disable_temporal_vector_snapshots()?;
```

### Slow Cold Starts

**Symptom:** Database takes >10s to start with indexes

**Diagnosis:**
```bash
# Time each load step
time aletheiadb load-indexes --path data/my-db --verbose

# Output:
# Manifest: 1ms
# Strings: 50ms
# Graph: 2000ms  ← SLOW
# Temporal: 500ms
# Vector: 800ms
```

**Solutions:**

1. **Use parallel loading:**
   ```rust
   use aletheiadb::storage::index_persistence::load_indexes_parallel;

   // Load graph, temporal, and vector indexes concurrently (~3x faster)
   load_indexes_parallel(&db, &manager)?;
   ```

2. **Use memory-mapped loading for large indexes:**
   ```rust
   use aletheiadb::storage::index_persistence::graph::load_graph_index_mmap;

   // Handles multi-GB indexes without loading entire file into RAM
   let graph_data = load_graph_index_mmap(&graph_path)?;
   ```

3. **Optimize graph index:**
   ```rust
   // Reduce node property sizes
   db.compact_graph_properties()?;
   ```

4. **Use SSD storage:**
   ```bash
   mv data/my-db /ssd/data/my-db
   ```

5. **Prewarm page cache (Linux):**
   ```bash
   vmtouch -t data/my-db/indexes/
   ```

### Checksum Mismatches After Crash

**Symptom:** CRC32 errors after power loss

**Diagnosis:**
```rust
manager.verify_checksums()?;
// Lists all corrupted files
```

**Solution:**
```rust
// Delete corrupted files
manager.remove_corrupted_indexes()?;

// Rebuild from WAL
db.rebuild_indexes_from_wal()?;

// Save fresh indexes
manager.save_all_indexes(&db)?;
```

## FAQ

### Q: Do I need to manually save indexes?

**A:** Not if you use `PersistenceConfig` with automatic saves. Manual saves are only needed for fine-grained control.

### Q: What happens if I delete index files?

**A:** Database falls back to WAL replay on next startup. No data is lost, but startup will be slower.

### Q: Can I move index files between machines?

**A:** Yes, but ensure:
- Same AletheiaDB version (check manifest version)
- Same index configuration (vector dimensions, HNSW params)
- Copy the entire `indexes/` directory

### Q: How do I verify index integrity?

**A:**
```rust
manager.verify_checksums()?;  // Checks all CRC32 checksums
manager.validate_formats()?;  // Validates magic bytes and versions
```

### Q: Can I disable specific index types?

**A:** Yes:
```rust
PersistenceConfig {
    save_graph: true,
    save_temporal: false,   // Skip temporal index
    save_vectors: true,
    ..Default::default()
}
```

### Q: What's the maximum database size for persistence?

**A:** Theoretical limit: Available disk space. AletheiaDB provides several features for handling large databases:

**Implemented:**
- **Memory-mapped loading** (`load_graph_index_mmap()`) - Handle multi-GB indexes without loading entire files into RAM
- **Parallel loading** (`load_indexes_parallel()`) - Load indexes concurrently for faster startup (~3x speedup)
- **Delta encoding** (`save_graph_index_delta()`) - Incremental saves for faster updates (60-75% size reduction)
- **Compression** (`save_graph_index_compressed()`) - Zstd compression to reduce disk I/O and storage

**Future enhancements:**
- Index sharding for horizontal scaling (when needed for >100GB databases)
- Distributed index coordination
- Streaming index loading

## See Also

- [ADR-0023: Index Persistence Layer](../adr/0023-index-persistence-layer.md) - Architecture decision
- [Design Document](../plans/2026-01-15-index-persistence-design.md) - Technical design
- [Vector Search Integration Guide](vector-search-integration.md) - Vector index specifics
- [Hybrid Query Guide](hybrid-query-guide.md) - Querying with indexes

## Support

For issues, questions, or feature requests, please:
- Check the [Troubleshooting](#troubleshooting) section
- Search existing [GitHub Issues](https://github.com/madmax983/AletheiaDB/issues)
- Create a new issue with `[persistence]` tag
