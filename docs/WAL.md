# Write-Ahead Log (WAL) Format

This document describes the WAL format and architecture for GallifreyDB.

## Architecture Overview

GallifreyDB uses a **Concurrent WAL with Striped Lock-Free Ring Buffers** for high-throughput write operations while maintaining ACID compliance.

### Concurrent WAL Architecture

```
                    ┌─────────────────────┐
                    │    LSN Allocator    │
                    │  AtomicU64::fetch_add
                    └──────────┬──────────┘
                               │
       ┌───────────────────────┼───────────────────────┐
       ▼                       ▼                       ▼
┌─────────────┐         ┌─────────────┐         ┌─────────────┐
│   Stripe 0  │         │   Stripe 1  │         │  Stripe N   │
│ Ring Buffer │         │ Ring Buffer │         │ Ring Buffer │
│ (Lock-free) │         │ (Lock-free) │         │ (Lock-free) │
└──────┬──────┘         └──────┬──────┘         └──────┬──────┘
       └───────────────────────┼───────────────────────┘
                               ▼
                    ┌─────────────────────┐
                    │  Flush Coordinator  │
                    │ - Drains all stripes│
                    │ - Sorts by LSN      │
                    │ - Writes to segment │
                    │ - fsync per mode    │
                    └─────────────────────┘
```

**Key Design Principles:**
1. **Lock-free append path**: Multiple threads can append concurrently without mutex contention
2. **Global LSN ordering**: Single atomic counter ensures total ordering of all operations
3. **Sorted flush**: Entries are sorted by LSN before writing to disk
4. **Same segment format**: On-disk format is identical to sequential WAL

### Performance Characteristics

| Metric | Value | Notes |
|--------|-------|-------|
| Append latency (async) | <100ns | Lock-free path |
| Throughput (GroupCommit) | 100K+/sec | ACID-compliant |
| Throughput (Async) | 500K+/sec | Eventual consistency |
| Concurrent writers | 64 | Linear scaling |

## ACID Compliance

The concurrent WAL maintains full ACID compliance for Synchronous and GroupCommit durability modes:

### Atomicity ✅
- All operations within a transaction are either fully persisted or not at all
- Flush coordinator writes entries atomically to segment files
- Recovery only replays complete transactions

### Consistency ✅
- LSN ordering ensures operations are applied in correct order
- Checksum verification detects any corruption
- Database invariants are preserved across crashes

### Isolation ✅
- Isolation handled by MVCC layer (not WAL)
- WAL only logs committed operations
- Snapshot isolation semantics unchanged

### Durability ✅ (Mode-Dependent)

| Mode | Durability | ACID Compliant |
|------|------------|----------------|
| **Synchronous** | Immediate (fsync on every commit) | ✅ Yes |
| **GroupCommit** | Epoch-based (transactions wait for flush) | ✅ Yes |
| **Async** | Eventual (background flush) | ❌ No |

**Why GroupCommit is ACID-Compliant:**
```
Transaction Flow (GroupCommit):
1. Append operation to stripe buffer (fast, lock-free)
2. Register with epoch N
3. WAIT for epoch N to be flushed ← Blocks here
4. Background thread: drain stripes → sort by LSN → write → fsync
5. Background thread: mark_flushed(epoch N) → wake all waiters
6. Return to caller (data is now durable)
```

The transaction does not return success until the fsync completes, guaranteeing durability.

## WAL Versioning

The WAL uses a versioned binary format to enable future evolution.

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
- CRC32 checksum verification for data integrity

## WAL Recovery

### Recovery Process

On database startup:

1. **Scan WAL directory** for segment files (`*.log`)
2. **Read all segments** in order (by segment ID)
3. **Parse entries** - entries are already in LSN order (sorted during flush)
4. **Verify checksums** for each entry
5. **Replay operations** to reconstruct state

```rust
use gallifreydb::storage::wal_reader::read_wal_entries;

// Read all entries from LSN 1 onwards
let entries = read_wal_entries(Path::new("data/wal"), LSN(1))?;
for entry in entries {
    replay_operation(entry.operation)?;
}
```

### Recovery Correctness with Concurrent WAL

The concurrent WAL writes entries to disk **sorted by LSN**, which is identical to the sequential WAL behavior:

```
Concurrent writes:         On-disk (after flush):
Thread 1: LSN 3           Entry 1: LSN 1
Thread 2: LSN 1           Entry 2: LSN 2  ← Sorted!
Thread 3: LSN 2           Entry 3: LSN 3
```

**Key Invariant:** Entries are always written to disk in LSN order, regardless of which stripe they originated from.

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

## Configuration

### ConcurrentWalSystem Configuration

```rust
use gallifreydb::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
use gallifreydb::storage::wal::DurabilityMode;

let config = ConcurrentWalSystemConfig {
    /// Directory for WAL segments
    wal_dir: PathBuf::from("data/wal"),

    /// Number of stripes (should be power of 2, default: 16)
    num_stripes: 16,

    /// Ring buffer capacity per stripe (default: 1024)
    stripe_capacity: 1024,

    /// Maximum segment size before rotation (default: 64MB)
    segment_size: 64 * 1024 * 1024,

    /// Number of segments to retain (default: 10)
    segments_to_retain: 10,

    /// Background flush interval in ms (default: 10)
    flush_interval_ms: 10,

    /// Durability mode
    durability_mode: DurabilityMode::GroupCommit {
        max_batch_size: 200,
        max_delay_ms: 10,
    },

    /// Write buffer size for segment files (default: 64KB)
    write_buffer_size: 64 * 1024,
};

let wal = ConcurrentWalSystem::new(config)?;
```

### Durability Modes

```rust
pub enum DurabilityMode {
    /// fsync on every commit - maximum durability
    /// Latency: ~1.5ms, Throughput: ~600/sec
    Synchronous,

    /// Batched fsync with epoch-based waiting - ACID compliant
    /// Latency: ~10-50ms, Throughput: ~100K/sec
    GroupCommit {
        max_delay_ms: u64,      // default: 10
        max_batch_size: usize,  // default: 200
    },

    /// Background fsync, commits return immediately - NOT ACID
    /// Latency: <100ns, Throughput: ~500K/sec
    Async {
        flush_interval_ms: u64, // default: 10
    },

    /// Like Async but with epoch tracking (for metrics)
    AsyncBatched {
        max_delay_ms: u64,
        max_batch_size: usize,
    },
}
```

### Production Recommendations

| Parameter | Recommendation | Rationale |
|-----------|---------------|-----------|
| `num_stripes` | 16-32 | Match expected concurrent writers |
| `stripe_capacity` | 1024 | Balance memory vs backpressure |
| `segment_size` | 64-128 MB | Balance rotation overhead vs recovery time |
| `segments_to_retain` | 10-20 | Enough for recovery + debugging |
| `durability_mode` | `GroupCommit` | Best balance of ACID + performance |

## Component Details

### LSN Allocator

The LSN allocator provides globally unique, monotonically increasing sequence numbers:

```rust
pub struct LsnAllocator {
    next_lsn: AtomicU64,
}

impl LsnAllocator {
    /// Allocate a single LSN (atomic operation)
    pub fn allocate(&self) -> LSN {
        LSN(self.next_lsn.fetch_add(1, Ordering::SeqCst))
    }

    /// Set next LSN (for recovery)
    pub fn set_next_lsn(&self, lsn: LSN) {
        self.next_lsn.store(lsn.0, Ordering::SeqCst);
    }
}
```

### Flush Coordinator

The flush coordinator manages segment files and coordinates flushing:

```rust
impl FlushCoordinator {
    /// Flush entries to disk
    pub fn flush(&self, mut entries: Vec<PendingEntry>, sync: bool) -> Result<FlushStats> {
        // 1. Sort by LSN to restore global order
        entries.sort_by_key(|e| e.lsn);

        // 2. Write to segment file
        for entry in &entries {
            self.write_entry(entry)?;
        }

        // 3. fsync if required
        if sync {
            self.sync()?;
        }

        // 4. Notify completion handles
        for entry in entries {
            if let Some(notifier) = entry.completion {
                notifier.complete(Ok(()));
            }
        }

        Ok(stats)
    }
}
```

## Debugging Tools

### Inspecting WAL Contents

```rust
use gallifreydb::storage::wal_reader::read_wal_entries;

// Print all entries
let entries = read_wal_entries(Path::new("data/wal"), LSN(1))?;
for entry in entries {
    println!("LSN {}: {:?}", entry.lsn.0, entry.operation);
}
```

### Checking WAL Metrics

```rust
let wal = ConcurrentWalSystem::new(config)?;

// After some operations...
println!("Total appends: {}", wal.total_appends());
println!("Total flushed: {}", wal.total_flushed());
println!("Current LSN: {:?}", wal.current_lsn());
```

## Adding New WAL Versions

When adding new serialization features:

1. Increment `WAL_VERSION` constant
2. Add version-aware serialization logic
3. Add version-aware deserialization logic
4. Update tests for new version

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

## References

- [ADR-0007: Write-Ahead Log for Durability](adr/0007-wal-durability.md)
- [ADR-0012: Configurable Durability Modes](adr/0012-configurable-durability-modes.md)
- [ADR-0020: Concurrent WAL Architecture](adr/0020-concurrent-wal-architecture.md)
- [Write-Ahead Logging on Wikipedia](https://en.wikipedia.org/wiki/Write-ahead_logging)
- [PostgreSQL WAL Documentation](https://www.postgresql.org/docs/current/wal-intro.html)
- [LMAX Disruptor](https://lmax-exchange.github.io/disruptor/) - Lock-free ring buffer design
