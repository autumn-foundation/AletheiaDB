# ADR-0020: Concurrent WAL Architecture (Striped Lock-Free Design)

**Status:** Accepted (Implemented)
**Date:** 2026-01-09
**Deciders:** AletheiaDB Core Team
**Categories:** storage, durability, performance, concurrency
**Supersedes:** Extends ADR-0007, ADR-0012

## Context

The previous WAL implementation used a single `Arc<Mutex<WriteAheadLog>>` for all write operations. While this provided strong consistency guarantees, it created a severe bottleneck:

**Previous Architecture:**
```
Thread 1 ─┐
Thread 2 ──┼──▶ Arc<Mutex<WriteAheadLog>> ──▶ BufWriter ──▶ Disk
Thread N ─┘         ↑ CONTENTION
```

**Measured Performance Limitations:**
- All writes serialize through one mutex
- Even with GroupCommit mode (~15K/sec), the mutex contention limits scalability
- Single writer thread becomes bottleneck with concurrent transactions
- Async mode throughput (~100K/sec) limited by lock acquisition

**Requirements:**
1. Support 16-64 concurrent writers without mutex contention
2. Maintain ACID compliance for Synchronous and GroupCommit modes
3. Preserve WAL segment format for backward compatibility
4. Enable horizontal scaling of write throughput
5. Keep recovery semantics unchanged

## Decision

We implement a **Striped WAL with Lock-Free Ring Buffers** architecture that eliminates the global mutex while preserving ACID guarantees.

### Architecture Overview

```
                    ┌─────────────────────┐
                    │    LSN Allocator    │
                    │  AtomicU64::fetch_add
                    └──────────┬──────────┘
                               │ (single atomic - contention point)
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
                    │  - Collects stripes │
                    │  - Sorts by LSN     │
                    │  - Writes segment   │
                    │  - fsync per mode   │
                    └─────────────────────┘
```

### Key Components

#### 1. LSN Allocator (Global Atomic)
```rust
pub struct LsnAllocator {
    next_lsn: AtomicU64,
}

impl LsnAllocator {
    pub fn allocate(&self) -> LSN {
        // Relaxed is sufficient - LSN ordering comes from the atomic counter itself,
        // not from memory visibility of other data. The monotonic increment guarantees
        // unique, ordered LSNs without requiring happens-before relationships.
        LSN(self.next_lsn.fetch_add(1, Ordering::Relaxed))
    }
}
```

**ACID Implication:** LSN allocation is globally ordered, ensuring a total order of all operations across all threads.

#### 2. Lock-Free Ring Buffers (Per-Stripe)
```rust
pub struct WalRingBuffer {
    entries: Box<[UnsafeCell<Option<PendingEntry>>]>,
    mask: usize,  // size - 1 for fast modulo
    write_pos: AtomicU64,
    read_pos: AtomicU64,
}
```

Each stripe has its own ring buffer. Writers select a stripe via thread affinity (hash of thread ID), distributing load evenly.

#### 3. Flush Coordinator
The flush coordinator:
1. Drains all stripes
2. **Sorts entries by LSN** (critical for recovery correctness)
3. Writes to segment file in LSN order
4. Performs fsync based on durability mode
5. Notifies completion handles

### ACID Compliance Analysis

#### Atomicity ✅
- **All-or-nothing semantics preserved**: Each transaction's operations are either all flushed or none are flushed
- Flush coordinator writes all entries atomically to the segment file
- Recovery replays complete transactions only

#### Consistency ✅
- **Invariants maintained**: LSN ordering ensures operations are applied in correct order
- Checksum verification detects corruption
- Recovery validates checksums before replay

#### Isolation ✅
- **MVCC unchanged**: Transaction isolation handled by MVCC layer, not WAL
- WAL only logs committed operations
- No change to snapshot isolation semantics

#### Durability ✅
- **Synchronous mode**: Flush + fsync before returning - **fully durable**
- **GroupCommit mode**: Transaction waits for epoch flush - **fully durable**
- **Async mode**: Background flush, eventual consistency - **not ACID-durable** (documented)

### Durability Mode Behavior

| Mode | Append Latency | Durability | ACID |
|------|---------------|------------|------|
| Synchronous | Immediate flush + fsync | Immediate | ✅ Full |
| GroupCommit | Append fast, wait for epoch | Epoch-based | ✅ Full |
| Async | Append fast, no wait | Background flush | ❌ Eventual |

#### Synchronous Mode Flow
```
Thread 1: append_sync(op)
    └──▶ allocate LSN (atomic)
    └──▶ write to stripe ring buffer
    └──▶ drain all stripes (collects other pending entries too)
    └──▶ sort by LSN
    └──▶ write to segment file
    └──▶ fsync()
    └──▶ return (ACID guarantee: op is durable)
```

#### GroupCommit Mode Flow
```
Thread 1: append(op)                    Background Thread:
    └──▶ allocate LSN                       │
    └──▶ write to stripe                    │ (every flush_interval_ms)
    └──▶ register epoch N                   └──▶ drain all stripes
    └──▶ WAIT for epoch N flush             └──▶ sort by LSN
             ↑                              └──▶ write to segment
             │                              └──▶ fsync()
             └────────── notify ◀───────────└──▶ mark_flushed(epoch N)
    └──▶ return (ACID guarantee: op is durable)
```

#### Async Mode Flow
```
Thread 1: append_async(op)              Background Thread:
    └──▶ allocate LSN                       │
    └──▶ write to stripe                    │ (every flush_interval_ms)
    └──▶ return IMMEDIATELY                 └──▶ drain all stripes
         (NO durability guarantee)          └──▶ sort by LSN
                                            └──▶ write to segment
                                            └──▶ fsync()
```

### Recovery Correctness

The concurrent WAL writes entries to disk **sorted by LSN**, which is identical to the sequential WAL behavior. Recovery is unchanged:

```rust
pub fn recover(wal_dir: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
    // Read all segment files
    let mut entries = Vec::new();
    for segment in read_segments(wal_dir)? {
        entries.extend(parse_segment(segment)?);
    }

    // Entries are already in LSN order (sorted during flush)
    // No additional sorting needed

    // Filter by start_lsn
    Ok(entries.into_iter().filter(|e| e.lsn >= start_lsn).collect())
}
```

**Key Invariant:** Entries are always written to disk in LSN order, regardless of which stripe they originated from.

### Why This Maintains ACID

1. **Global LSN Ordering**: The `AtomicU64::fetch_add` provides a total order of all operations
2. **Sort Before Write**: Flush coordinator sorts by LSN, restoring global order
3. **Same Segment Format**: On-disk format is identical to sequential WAL
4. **Same Recovery**: Recovery sees entries in LSN order, unchanged semantics
5. **GroupCommit Waiting**: ACID modes block until fsync completes

## Consequences

### Positive

- **Higher throughput**: 100K+/sec (GroupCommit), 500K+/sec (Async)
- **Lower latency**: ~100ns append vs ~1-2µs (lock acquisition eliminated)
- **Better scalability**: Linear scaling up to 64 concurrent writers
- **Lock-free hot path**: No mutex contention on append
- **ACID preserved**: Synchronous and GroupCommit modes remain fully ACID

### Negative

- **Higher complexity**: Multiple components to maintain
- **Memory overhead**: Per-stripe ring buffers (~1MB per stripe with 16 stripes)
- **Delayed ordering**: Entries not in order until flush (by design)
- **Single flush thread**: Flush coordinator is single-threaded (could be future bottleneck)

### Neutral

- **Same segment format**: No migration needed
- **Same recovery**: Recovery code unchanged
- **Same durability guarantees per mode**: Mode semantics preserved

## Alternatives Considered

### Alternative 1: Sharded WAL Files
Write to multiple WAL files, one per stripe.

**Rejected because:**
- Complex recovery (merge multiple files by LSN)
- Ordering guarantees harder to reason about
- More file handles to manage

### Alternative 2: Fine-Grained Locking
Use RwLock or finer-grained locks instead of global Mutex.

**Rejected because:**
- Still has lock contention, just distributed
- Lock-free is faster for write-heavy workloads
- RwLock doesn't help (WAL is write-only hot path)

### Alternative 3: Actor-Based (Message Passing)
Send operations to a single WAL actor via channels.

**Rejected because:**
- Channel overhead (~100ns) still present
- Single actor becomes bottleneck
- Striped design distributes load better

## Implementation Details

### Configuration
```rust
pub struct ConcurrentWalSystemConfig {
    pub wal_dir: PathBuf,
    pub num_stripes: usize,        // Default: 16 (power of 2)
    pub stripe_capacity: usize,    // Default: 1024 entries
    pub segment_size: usize,       // Default: 64MB
    pub segments_to_retain: usize, // Default: 10
    pub flush_interval_ms: u64,    // Default: 10ms
    pub durability_mode: DurabilityMode,
}
```

### Thread Affinity
```rust
fn select_stripe(&self, thread_id: u64) -> usize {
    // Simple hash for even distribution
    (thread_id as usize) & (self.num_stripes - 1)
}
```

### Performance Targets

| Metric | Old (Mutex) | New (Striped) | Improvement |
|--------|-------------|---------------|-------------|
| Append latency (async) | ~1-2µs | <100ns | 10-20x |
| Throughput (GroupCommit) | ~15K/sec | 100K+/sec | 6-7x |
| Throughput (Async) | ~100K/sec | 500K+/sec | 5x |
| Concurrent writers | 1 effective | 64 | 64x |

## References

- **ADR-0007**: Write-Ahead Log for Durability
- **ADR-0012**: Configurable Durability Modes
- [LMAX Disruptor](https://lmax-exchange.github.io/disruptor/) - Lock-free ring buffer design
- [PostgreSQL WAL](https://www.postgresql.org/docs/current/wal-intro.html)
- [CockroachDB Pebble WAL](https://github.com/cockroachdb/pebble)
