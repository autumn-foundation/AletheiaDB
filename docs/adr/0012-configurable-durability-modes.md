# ADR-0012: Configurable Durability Modes

**Status:** Proposed
**Date:** 2026-01-01
**Deciders:** GallifreyDB Core Team
**Categories:** storage, durability, performance

## Context

GallifreyDB's current WAL implementation performs a synchronous `fsync` on every transaction commit. While this provides maximum durability (zero data loss on crash), it severely limits write throughput:

**Current Performance:**
- Node creation: ~1.8ms (99% spent in fsync)
- Edge creation: ~1.6ms
- Throughput: ~600 writes/second

**Target Performance:**
- Node creation: <100µs (batched), <10µs (async)
- Throughput: 15,000-100,000+ writes/second

The fsync operation takes ~1.5ms on modern SSDs because it forces the drive to flush its volatile write cache to persistent storage. This is necessary for durability but not all workloads require the same durability guarantees:

- **Financial transactions**: Must survive any crash (synchronous fsync required)
- **Chat messages**: Losing 100ms of messages on crash is acceptable
- **Bulk imports**: Can be re-run if crash occurs during import
- **Analytics events**: High volume, low criticality

## Decision

We will implement three durability modes with both global configuration and per-transaction override capability:

### DurabilityMode Enum

```rust
/// Controls when WAL data is synced to disk.
/// All modes write to WAL - they differ in when fsync occurs.
pub enum DurabilityMode {
    /// fsync on every commit - maximum durability, lowest performance
    /// Latency: ~1.5ms per operation
    /// Risk: Zero data loss
    Synchronous,

    /// fsync after N operations OR T milliseconds (whichever first)
    /// Latency: ~60µs per operation
    /// Risk: Up to N operations or T ms of data
    Batched {
        batch_size: usize,      // default: 100
        max_delay_ms: u64,      // default: 10
    },

    /// Background thread handles fsync continuously
    /// Latency: ~6µs per operation
    /// Risk: ~10ms of data (background sync interval)
    Async,
}
```

### Data Flow by Mode

```
Synchronous:
  Write → WAL Buffer → fsync() → Return to caller
                          ↑
                     Blocks here (~1.5ms)

Batched:
  Write → WAL Buffer → Return to caller (fast)
                 ↓
           Counter/Timer check
                 ↓
           When threshold reached → fsync()

Async:
  Write → WAL Buffer → Return to caller (immediate)
                 ↓
           Background thread → Continuous fsync loop
```

### Configuration API

**Global Default (set at database creation):**

```rust
pub struct WalConfig {
    pub wal_dir: PathBuf,
    pub default_durability: DurabilityMode,
    pub segment_size_bytes: usize,
}

let db = GallifreyDB::builder()
    .wal_config(WalConfig {
        wal_dir: "/data/wal".into(),
        default_durability: DurabilityMode::Batched {
            batch_size: 100,
            max_delay_ms: 10
        },
        ..Default::default()
    })
    .build()?;
```

**Per-Transaction Override:**

```rust
pub struct WriteOptions {
    pub durability: Option<DurabilityMode>,
}

impl WriteOptions {
    pub fn bulk_import() -> Self {
        Self { durability: Some(DurabilityMode::Async) }
    }

    pub fn critical() -> Self {
        Self { durability: Some(DurabilityMode::Synchronous) }
    }
}

// Usage
db.write_with_options(WriteOptions::critical(), |tx| {
    tx.create_node("Payment", payment_data)
})?;
```

## Consequences

### Positive

- **30-300x write throughput improvement** for batched/async modes
- **Flexibility**: Applications choose appropriate durability per use case
- **Backward compatible**: Default to Synchronous preserves current behavior
- **Simple mental model**: All modes write to WAL, only fsync timing differs
- **Production-ready defaults**: Batched mode balances performance and safety

### Negative

- **Potential data loss**: Batched/Async modes can lose recent writes on crash
- **Complexity**: Three code paths to maintain and test
- **Configuration burden**: Users must understand trade-offs
- **Async mode complexity**: Background thread management, backpressure handling

### Neutral

- WAL format unchanged - only sync timing changes
- Recovery process unchanged - replays all WAL entries
- No impact on read performance

## Alternatives Considered

### Alternative 1: Always Synchronous (Current Behavior)

Keep the current fsync-per-commit behavior.

**Rejected because:**
- 600 writes/sec is too slow for bulk imports and high-throughput workloads
- Competing databases offer configurable durability
- LLM knowledge graph updates need higher throughput

### Alternative 2: Always Async

Make all writes async with no synchronous option.

**Rejected because:**
- Financial and audit use cases require guaranteed durability
- No way to ensure critical operations survive crash
- Violates user expectations for database durability

### Alternative 3: Separate WAL Modes (Multiple WALs)

Create separate WAL files for different durability levels.

**Rejected because:**
- Complicates recovery (must replay multiple WALs in order)
- Potential ordering issues between WALs
- Unnecessary complexity for the flexibility gained

## Implementation Notes

### Batched Mode Implementation

```rust
impl WriteAheadLog {
    fn append_batched(&mut self, entry: WalEntry) -> Result<()> {
        self.buffer.push(entry);
        self.pending_count += 1;

        let should_sync = self.pending_count >= self.config.batch_size
            || self.last_sync.elapsed() >= self.config.max_delay;

        if should_sync {
            self.sync()?;
            self.pending_count = 0;
            self.last_sync = Instant::now();
        }
        Ok(())
    }
}
```

### Async Mode Implementation

```rust
struct AsyncWalWriter {
    sender: crossbeam::channel::Sender<WalEntry>,
    sync_thread: JoinHandle<()>,
}

impl AsyncWalWriter {
    fn append(&self, entry: WalEntry) -> Result<()> {
        // Non-blocking send to background thread
        self.sender.try_send(entry)
            .map_err(|_| StorageError::WalBufferFull)?;
        Ok(())
    }
}

// Background sync thread - uses blocking receive to avoid busy-wait
fn sync_loop(receiver: Receiver<WalEntry>, wal: &mut Wal, sync_interval: Duration) {
    loop {
        match receiver.recv_timeout(sync_interval) {
            Ok(first_entry) => {
                let mut batch = vec![first_entry];
                // Drain any other pending entries non-blockingly
                while let Ok(entry) = receiver.try_recv() {
                    batch.push(entry);
                }

                for entry in batch {
                    wal.append_no_sync(entry);
                }
                wal.sync();
            }
            Err(RecvTimeoutError::Timeout) => {
                // Periodic sync for any buffered data
                wal.sync_if_dirty();
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Sender dropped - drain remaining and exit
                while let Ok(entry) = receiver.try_recv() {
                    wal.append_no_sync(entry);
                }
                wal.sync();
                break;
            }
        }
    }
}
```

### Graceful Shutdown

All modes must flush pending writes on shutdown:

```rust
// Synchronous and Batched modes
impl Drop for WriteAheadLog {
    fn drop(&mut self) {
        // Ensure all buffered entries are synced
        let _ = self.sync();
    }
}

// Async mode requires separate handling - the background thread
// owns the WAL, so AsyncWalWriter must coordinate shutdown
impl Drop for AsyncWalWriter {
    fn drop(&mut self) {
        // Signal background thread to stop accepting new entries
        drop(self.sender.take());

        // Wait for background thread to drain buffer and sync
        if let Some(handle) = self.sync_thread.take() {
            let _ = handle.join();  // Blocks until all entries synced
        }
    }
}
```

**Note:** The async mode's `Drop` is critical - without it, entries in the channel
would be lost on shutdown. The background thread handles the final drain in its
`Disconnected` case (see `sync_loop` above).

## References

- GitHub Issues: [#127](https://github.com/madmax983/GallifreyDB/issues/127), [#128](https://github.com/madmax983/GallifreyDB/issues/128), [#129](https://github.com/madmax983/GallifreyDB/issues/129), [#130](https://github.com/madmax983/GallifreyDB/issues/130), [#131](https://github.com/madmax983/GallifreyDB/issues/131)
- Project: [GallifreyDB Write Performance](https://github.com/users/madmax983/projects/5)
- PostgreSQL synchronous_commit: [Documentation](https://www.postgresql.org/docs/current/runtime-config-wal.html#GUC-SYNCHRONOUS-COMMIT)
- MongoDB Write Concern: [Documentation](https://www.mongodb.com/docs/manual/reference/write-concern/)
- ADR-0007: Write-Ahead Log for Durability (extends this ADR)
