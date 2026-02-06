# ADR-0012: Configurable Durability Modes

**Status:** Accepted (Implemented)
**Date:** 2026-01-01
**Deciders:** AletheiaDB Core Team
**Categories:** storage, durability, performance
**Implementation:** Completed 2026-01-08

## Context

AletheiaDB's current WAL implementation performs a synchronous `fsync` on every transaction commit. While this provides maximum durability (zero data loss on crash), it severely limits write throughput:

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
    /// Each commit waits for its own fsync()
    /// Latency: ~1.5ms per operation
    /// ACID: ✅ Full compliance
    /// Risk: Zero data loss
    Synchronous,

    /// Multiple commits share a single fsync (epoch-based coordination)
    /// CRITICAL: Transactions BLOCK until their epoch is flushed
    /// Latency: ~max_delay_ms per operation (10-50ms with overhead)
    /// ACID: ✅ Full compliance (transactions wait for flush)
    /// Risk: Zero data loss (same as Synchronous)
    GroupCommit {
        max_delay_ms: u64,      // default: 10ms
        max_batch_size: usize,  // default: 200
    },

    /// Background thread handles fsync, commits return immediately
    /// Latency: ~6µs per operation
    /// ACID: ❌ Eventual consistency only
    /// Risk: ~flush_interval_ms of data (default 10ms)
    Async {
        flush_interval_ms: u64, // default: 10ms
    },
}
```

### Data Flow by Mode

```
Synchronous:
  Write → WAL Buffer → fsync() → Return to caller
                          ↑
                     Blocks here (~1.5ms)

GroupCommit (ACID-compliant):
  Write → WAL Buffer → Register with epoch N → WAIT for epoch N flush
                 ↓                                      ↑
           Background thread wakes                 Blocks here
                 ↓                                      ↓
           fsync() all epoch N txns → mark_flushed(N) → Wake all waiters
                 ↓
           Return to callers (ACID guaranteed)

Async (eventual consistency):
  Write → WAL Buffer → Return to caller (immediate, NO WAIT)
                 ↓
           Background thread → Continuous fsync loop
```

### Configuration API

**Global Default (set at database creation):**

```rust
pub struct WalConfig {
    pub wal_dir: PathBuf,
    pub segment_size: usize,
    pub segments_to_retain: usize,
    pub durability_mode: DurabilityMode,
}

let config = WalConfig {
    wal_dir: "/data/wal".into(),
    segment_size: 64 * 1024 * 1024,  // 64MB
    segments_to_retain: 10,
    durability_mode: DurabilityMode::GroupCommit {
        max_delay_ms: 10,
        max_batch_size: 200,
    },
};

let db = AletheiaDB::with_wal_config(config);
```

**Per-Transaction Override:**

```rust
pub struct WriteOptions {
    pub durability_mode: Option<DurabilityMode>,
}

impl WriteOptions {
    /// Create new options with default settings
    pub fn new() -> Self;

    /// Set custom durability mode
    pub fn with_durability(self, mode: DurabilityMode) -> Self;

    /// Preset for bulk imports (Async mode, 100ms flush)
    pub fn bulk_import() -> Self;

    /// Preset for critical operations (Synchronous mode)
    pub fn critical() -> Self;
}

// Method 1: Use preset for critical transactions
let payment_id = db.write_with_options(WriteOptions::critical(), |tx| {
    tx.create_node("Payment", payment_data)
})?;

// Method 2: Use preset for bulk imports
db.write_with_options(WriteOptions::bulk_import(), |tx| {
    for record in bulk_data {
        tx.create_node("Record", record)?;
    }
    Ok(())
})?;

// Method 3: Custom configuration
let custom_options = WriteOptions::new()
    .with_durability(DurabilityMode::Async { flush_interval_ms: 50 });

db.write_with_options(custom_options, |tx| {
    tx.create_node("CustomData", data)
})?;

// Method 4: Manual struct construction (also supported)
let manual_options = WriteOptions {
    durability_mode: Some(DurabilityMode::Synchronous),
};

// Regular transaction - uses global default (GroupCommit)
db.write(|tx| {
    tx.create_node("User", user_data)
})?;
```

## Consequences

### Positive

- **25x write throughput improvement** (Synchronous → GroupCommit)
- **100x+ throughput improvement** (Synchronous → Async)
- **ACID compliance maintained**: GroupCommit provides full ACID guarantees
- **Flexibility**: Applications choose appropriate durability per use case
- **Backward compatible**: Default to Synchronous preserves current behavior
- **Simple mental model**: All modes write to WAL, only fsync timing differs
- **Production-ready defaults**: GroupCommit balances ACID + performance

### Negative

- **Potential data loss**: Async mode can lose recent writes on crash (10-50ms)
- **Higher latency**: GroupCommit trades individual fsync speed for batching
- **Complexity**: Three code paths to maintain and test
- **Configuration burden**: Users must understand ACID vs throughput trade-offs
- **Thread overhead**: GroupCommit and Async require background threads

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

### GroupCommit Mode Implementation

GroupCommit uses an **epoch-based coordinator** to manage batched fsyncs while maintaining ACID guarantees:

```rust
pub struct GroupCommitCoordinator {
    state: Mutex<GroupCommitState>,
    flush_complete: Condvar,
    config: GroupCommitConfig,
}

struct GroupCommitState {
    current_epoch: u64,
    batch_count: usize,
    flushed_epoch: u64,
    last_flush_error: Option<String>,
}

impl GroupCommitCoordinator {
    /// Transaction registers and gets epoch number
    pub fn register_transaction(&self) -> (u64, bool) {
        let mut state = self.state.lock().unwrap();
        state.batch_count += 1;
        let epoch = state.current_epoch;
        let should_flush = state.batch_count >= self.config.max_batch_size;
        (epoch, should_flush)
    }

    /// Transaction blocks until its epoch is flushed
    pub fn wait_for_flush(&self, epoch: u64) -> Result<()> {
        let mut state = self.state.lock_or_err()?;
        let timeout = Duration::from_millis(self.config.max_delay_ms * 10)
            + Duration::from_millis(200);

        while state.flushed_epoch <= epoch {
            let (new_state, timeout_result) =
                self.flush_complete.wait_timeout(state, timeout)?;
            state = new_state;

            if timeout_result.timed_out() && state.flushed_epoch <= epoch {
                return Err(Error::Timeout);
            }
        }

        // Check for flush errors
        if let Some(ref error) = state.last_flush_error {
            return Err(Error::FlushFailed(error.clone()));
        }

        Ok(())
    }

    /// Background thread calls this after fsync
    pub fn mark_flushed(&self, result: Result<()>) {
        let mut state = self.state.lock().unwrap();
        state.last_flush_error = result.err().map(|e| e.to_string());
        state.flushed_epoch = state.current_epoch + 1;
        state.current_epoch += 1;
        state.batch_count = 0;
        self.flush_complete.notify_all();  // Wake all waiting transactions
    }
}
```

**Critical Race Condition Fix (Commit 6bd42ff):**

```rust
// Transaction commit flow - FIXED version
let (commit_timestamp, wait_epoch, coordinator) = {
    let mut wal = self.wal.lock().unwrap();

    self.log_operations_to_wal(&mut wal, commit)?;
    let epoch = wal.commit_with_mode(self.durability_mode)?;

    // CRITICAL: Clone coordinator BEFORE releasing lock
    let gc = wal.group_commit_coordinator().cloned();

    (commit, epoch, gc)
    // WAL lock released here
};

// Safe to wait now - we have our coordinator reference
if let Some(epoch) = wait_epoch {
    if let Some(gc) = coordinator {
        gc.wait_for_flush(epoch)?;  // ACID guaranteed
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

All modes use `FlushGuard` (RAII pattern) to ensure pending writes are synced on shutdown:

```rust
pub struct FlushGuard {
    signal: Arc<FlushSignal>,
    thread_handle: Option<JoinHandle<()>>,
}

impl Drop for FlushGuard {
    fn drop(&mut self) {
        // Signal shutdown
        self.signal.request_shutdown();

        // Wait for thread to complete (it will do a final flush)
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();  // Blocks until all entries synced
        }
    }
}

// Background flush thread loop
fn flush_thread_loop<F>(signal: Arc<FlushSignal>, flush_fn: F, interval: Duration)
where
    F: Fn(),
{
    loop {
        let should_continue = signal.wait_for_interval(interval);

        // Always flush before checking whether to exit
        // This ensures piggybacking works and shutdown flushes
        flush_fn();

        if !should_continue {
            // Shutdown requested - we've already done final flush above
            break;
        }
    }
}
```

**Key Design:**
- Both GroupCommit and Async modes use `FlushGuard`
- `Drop` implementation ensures no data loss on shutdown
- Background thread always flushes before exiting
- WAL holds `FlushGuard`, which is dropped when database closes

## Implementation Status

**Status:** ✅ Fully Implemented (2026-01-08)

**Key Commits:**
- Initial implementation: `feature/durability-modes` branch
- Race condition fix: `6bd42ff` - Fixed coordinator cloning race in WriteTx
- CI flakiness fix: `6bd42ff` - Updated test thresholds for GroupCommit
- Documentation: `af85124` - Updated architecture docs

**Test Coverage:**
- 16 integration tests in `tests/durability_modes.rs`
- Unit tests in `src/storage/wal/group_commit.rs` (14 tests)
- All 725 tests passing

**Performance Results:**
| Mode | Latency | Throughput | ACID |
|------|---------|------------|------|
| Synchronous | ~1.5ms | ~600/sec | ✅ |
| GroupCommit | ~10-50ms | ~15K/sec | ✅ |
| Async | ~6µs | ~100K+/sec | ❌ |

## References

- **Architecture Documentation:** [durability-modes.md](../architecture/durability-modes.md)
- **GitHub Issues:** [#127](https://github.com/madmax983/AletheiaDB/issues/127), [#128](https://github.com/madmax983/AletheiaDB/issues/128), [#129](https://github.com/madmax983/AletheiaDB/issues/129), [#130](https://github.com/madmax983/AletheiaDB/issues/130), [#131](https://github.com/madmax983/AletheiaDB/issues/131)
- **Project:** [AletheiaDB Write Performance](https://github.com/users/madmax983/projects/5)
- **PostgreSQL synchronous_commit:** [Documentation](https://www.postgresql.org/docs/current/runtime-config-wal.html#GUC-SYNCHRONOUS-COMMIT)
- **MongoDB Write Concern:** [Documentation](https://www.mongodb.com/docs/manual/reference/write-concern/)
- **ADR-0007:** Write-Ahead Log for Durability (extends this ADR)
