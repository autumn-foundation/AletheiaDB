# Write-Ahead Log (WAL) Format

This document describes the WAL format for GallifreyDB.

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

### Current Version: 1

**Features:**
- Full serialization of properties (PropertyMap)
- Full serialization of bi-temporal intervals (32 bytes each)
- Labels serialized for all operation types
- Checksum verification for data integrity

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

## Adding New WAL Versions

When adding new serialization features, bump the version constant and add version-aware serialization/deserialization logic:

```rust
// In src/storage/wal.rs
const WAL_VERSION: u8 = 2;  // Increment version

fn serialize_entry(entry: &WalEntry, version: u8) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"GWAL");
    buf.push(version);
    // ... serialize fields, with version-specific logic
    buf
}
```

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
