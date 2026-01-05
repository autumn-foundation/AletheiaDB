# Write-Ahead Log (WAL) Format and Migration

This document describes the WAL format, versioning strategy, and migration procedures for GallifreyDB.

## WAL Versioning

The Write-Ahead Log (WAL) uses a versioned binary format to enable future evolution.

### Binary Format

**Segment Header (5 bytes):**
```
[magic: 4 bytes "GWAL"][version: 1 byte]
```

**Entry Format:**
```
[LSN: 8 bytes][timestamp: 8 bytes][checksum: 4 bytes][op_type: 1 byte][operation data...]
```

### Current Version: 2

**Features:**
- Full serialization of properties (PropertyMap)
- Full serialization of bi-temporal intervals (32 bytes each)
- Labels serialized for all operation types
- Checksum verification for data integrity

### Legacy Version: 1

**Limitations (no header):**
- Properties were not serialized (data loss on recovery)
- Temporal intervals were not serialized (reconstructed from timestamp)
- Update operations did not serialize labels
- No format version identifier

## Backward Compatibility

The WAL reader automatically detects the format version:

| Version | Identification | Handling |
|---------|----------------|----------|
| V2+ | "GWAL" magic bytes at start | Full deserialization |
| V1 | No header (absence of magic bytes) | Default values |

### V1 Default Values

When reading V1 segments:
- **Properties**: Default to `PropertyMap::new()` (empty)
- **Temporal intervals**: Default to `BiTemporalInterval::current(timestamp)`
- **Update labels**: Default to empty string

**Note**: This represents data loss - information that was never serialized cannot be recovered.

## Migration Tool

### Checking WAL Version

```rust
use gallifreydb::storage::wal::detect_wal_version;
use std::path::Path;

// Check a single segment
let info = detect_wal_version(Path::new("data/wal/000001.log"))?;
println!("Version: {}, needs migration: {}", info.version, info.needs_migration);

if info.needs_migration {
    println!("This segment should be migrated to V{}", info.current_version);
}
```

### Migrating Single Segment

```rust
use gallifreydb::storage::wal::migrate_wal_segment;

// Migrate a single segment (creates .bak backup)
let entries_migrated = migrate_wal_segment(Path::new("data/wal/000001.log"))?;
println!("Migrated {} entries", entries_migrated);
```

### Migrating Directory

```rust
use gallifreydb::storage::wal::migrate_wal_directory;

// Migrate all segments in a directory
let results = migrate_wal_directory(Path::new("data/wal/"))?;
for (path, count) in results {
    println!("Migrated {}: {} entries", path.display(), count);
}
```

## Migration Process

The migration follows these steps:

1. **Backup**: Original segment is renamed to `.log.bak`
2. **Parse**: Entries are read using version-aware parsing
3. **Rewrite**: Entries are written in V2 format with proper header
4. **Verify**: New segment can be read back successfully

### Safety Guarantees

- Original file is preserved as `.log.bak`
- Migration is atomic - either succeeds completely or rolls back
- Checksums verify data integrity after migration
- Failed migrations can be retried

### Data Loss Warning

**Important**: Migration of V1 segments results in data loss for properties and temporal intervals that were never serialized. The migrated entries will have placeholder values.

If you need to preserve this data, you must regenerate it from the application layer before migration.

## Adding New WAL Versions

When adding new serialization features:

### Step 1: Update Version Constant

```rust
// In src/storage/wal/mod.rs
pub const WAL_VERSION: u8 = 3;  // Increment version
```

### Step 2: Update Serialization

```rust
fn serialize_entry(entry: &WalEntry, version: u8) -> Vec<u8> {
    let mut buf = Vec::new();

    // Write header
    buf.extend_from_slice(b"GWAL");
    buf.push(version);

    // Write LSN, timestamp, etc.
    buf.extend_from_slice(&entry.lsn.to_le_bytes());
    buf.extend_from_slice(&entry.timestamp.to_le_bytes());

    // V3+ specific fields
    if version >= 3 {
        // Serialize new fields
    }

    buf
}
```

### Step 3: Update Deserialization

```rust
fn read_segment(path: &Path) -> Result<Vec<WalEntry>> {
    let version = detect_version(path)?;

    // Version-aware parsing
    let (data, len) = if version >= 3 {
        // Deserialize new format
        deserialize_v3(reader)?
    } else if version >= 2 {
        // Use V2 format
        deserialize_v2(reader)?
    } else {
        // Use placeholder for V1
        deserialize_v1_with_defaults(reader)?
    };

    Ok(data)
}
```

### Step 4: Update Migration Support

```rust
fn parse_wal_entries_versioned(entries: &[u8], version: u8) -> Result<Vec<WalEntry>> {
    match version {
        3 => parse_v3_entries(entries),
        2 => parse_v2_entries(entries),
        1 => parse_v1_entries_with_defaults(entries),
        _ => Err(Error::UnsupportedWalVersion(version)),
    }
}
```

### Step 5: Add Tests

```rust
#[test]
fn test_v3_serialization_roundtrip() {
    let entry = create_v3_entry();
    let serialized = serialize_entry(&entry, 3);
    let deserialized = deserialize_entry(&serialized, 3)?;
    assert_eq!(entry, deserialized);
}

#[test]
fn test_v2_to_v3_migration() {
    let v2_segment = create_v2_test_segment();
    let migrated = migrate_wal_segment(v2_segment)?;
    let entries = read_segment(migrated)?;
    // Verify entries are now in V3 format
    assert_eq!(detect_version(migrated)?, 3);
}
```

## WAL Recovery

### Recovery Process

On database startup:

1. **Scan WAL directory** for segment files
2. **Detect version** of each segment
3. **Parse entries** using version-aware reader
4. **Replay operations** in LSN order
5. **Verify checksums** for data integrity

### Handling Corrupted Segments

If a segment fails checksum verification:

```rust
match read_segment(path) {
    Ok(entries) => replay_entries(entries),
    Err(Error::ChecksumMismatch { lsn, expected, actual }) => {
        // Log corruption
        eprintln!("Segment corrupted at LSN {}", lsn);

        // Attempt partial recovery
        let recovered = recover_until_corruption(path, lsn)?;
        replay_entries(recovered);

        // Mark segment as corrupted
        mark_corrupted(path)?;
    }
    Err(e) => return Err(e),
}
```

### Partial Recovery

The WAL supports partial recovery up to the first corrupted entry:

- Entries before corruption are replayed
- Corrupted entry and all following entries are discarded
- Database state is consistent up to last good entry
- Application must re-apply lost transactions

## Performance Considerations

### Write Performance

- **Batching**: Group multiple operations into single WAL write
- **Buffering**: Use buffered I/O to reduce syscall overhead
- **fsync**: Call fsync only at transaction commit, not per-entry

### Recovery Performance

- **Sequential reads**: WAL is optimized for sequential scanning
- **Parallel recovery**: Multiple segments can be parsed in parallel
- **Checkpointing**: Periodic checkpoints reduce replay time

### Storage Management

- **Segment rotation**: Create new segment when current reaches size limit
- **Compaction**: Remove segments older than checkpoint
- **Archival**: Compress and archive old segments for long-term retention

## Configuration

### Tunable Parameters

```rust
pub struct WalConfig {
    /// Directory for WAL segments
    pub wal_dir: PathBuf,

    /// Maximum segment size before rotation (default: 64MB)
    pub max_segment_size: usize,

    /// Buffer size for writes (default: 8KB)
    pub write_buffer_size: usize,

    /// Enable fsync on commit (default: true)
    pub sync_on_commit: bool,

    /// Number of segments to retain (default: 10)
    pub retention_count: usize,
}
```

### Production Recommendations

| Parameter | Recommendation | Rationale |
|-----------|---------------|-----------|
| `max_segment_size` | 64-128 MB | Balance rotation overhead vs recovery time |
| `write_buffer_size` | 8-16 KB | Match filesystem block size |
| `sync_on_commit` | `true` | Durability guarantee |
| `retention_count` | 10-20 | Enough for recovery + debugging |

## Debugging Tools

### Inspecting WAL Contents

```rust
use gallifreydb::storage::wal::inspect_segment;

// Print human-readable segment contents
let entries = inspect_segment(Path::new("data/wal/000001.log"))?;
for entry in entries {
    println!("{:?}", entry);
}
```

### Verifying Segment Integrity

```rust
use gallifreydb::storage::wal::verify_segment;

match verify_segment(Path::new("data/wal/000001.log")) {
    Ok(()) => println!("Segment is valid"),
    Err(e) => eprintln!("Segment corrupted: {}", e),
}
```

## References

- [Write-Ahead Logging on Wikipedia](https://en.wikipedia.org/wiki/Write-ahead_logging)
- [PostgreSQL WAL Documentation](https://www.postgresql.org/docs/current/wal-intro.html)
- [SQLite WAL Mode](https://www.sqlite.org/wal.html)
