# Durability Modes Architecture

This document describes the architecture of GallifreyDB's configurable durability modes, which control when WAL data is synchronized to disk.

## Overview

GallifreyDB uses a **Concurrent WAL with Striped Lock-Free Ring Buffers** architecture that eliminates mutex contention while supporting four durability modes:

| Mode | Write Latency | Throughput | Data at Risk | ACID |
|------|---------------|------------|--------------|------|
| **Synchronous** | ~1.5ms | ~600/sec | None | ✅ Full |
| **GroupCommit** | ~10-50ms* | ~100K+/sec | None | ✅ Full |
| **Async** | <100ns | ~500K+/sec | ~flush_interval | ❌ Eventual |
| **AsyncBatched** | <100ns | ~500K+/sec | ~flush_interval | ❌ Eventual |

*GroupCommit latency = max_delay_ms (default 10ms) + thread scheduling. Provides full ACID by waiting for epoch flush.

## Concurrent WAL Architecture

The concurrent WAL eliminates the previous mutex bottleneck by using striped lock-free ring buffers:

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
                    │  - Drains all stripes│
                    │  - Sorts by LSN     │
                    │  - Writes to segment│
                    │  - fsync per mode   │
                    └─────────────────────┘
```

### Key Design Principles

1. **Lock-free append path**: Multiple threads append concurrently without mutex contention
2. **Global LSN ordering**: Single atomic counter ensures total ordering of all operations
3. **Sorted flush**: Entries are sorted by LSN before writing to disk
4. **Same segment format**: On-disk format is identical to sequential WAL

### Why This Maintains ACID

| Property | How Concurrent WAL Preserves It |
|----------|--------------------------------|
| **Atomicity** | Flush coordinator writes entries atomically; recovery replays complete transactions only |
| **Consistency** | LSN ordering ensures operations applied in correct order; checksums detect corruption |
| **Isolation** | MVCC layer handles isolation (unchanged); WAL only logs committed operations |
| **Durability** | GroupCommit/Sync modes block until fsync completes |

### Performance Improvement

| Metric | Old (Mutex) | New (Striped) | Improvement |
|--------|-------------|---------------|-------------|
| Append latency (async) | ~1-2µs | <100ns | 10-20x |
| Throughput (GroupCommit) | ~15K/sec | 100K+/sec | 6-7x |
| Throughput (Async) | ~100K/sec | 500K+/sec | 5x |
| Concurrent writers | 1 effective | 64 | 64x |

## Data Flow Diagrams

### Synchronous Mode

```mermaid
sequenceDiagram
    participant App as Application
    participant TX as Transaction
    participant WAL as WAL Buffer
    participant Disk as Disk

    App->>TX: write(data)
    TX->>WAL: append(entry)
    WAL->>Disk: fsync()
    Note over Disk: ~1.5ms
    Disk-->>WAL: confirmed
    WAL-->>TX: success
    TX-->>App: committed

    Note over App,Disk: Blocks until disk confirms
```

### GroupCommit Mode

```mermaid
sequenceDiagram
    participant App as Application
    participant TX as Transaction
    participant Coord as GroupCommitCoordinator
    participant BG as Background Flush Thread
    participant Disk as Disk

    App->>TX: write(data)
    TX->>Coord: register_transaction()
    Coord-->>TX: epoch=0
    TX->>TX: append to WAL

    Note over TX: CRITICAL: Release WAL lock

    TX->>TX: wait_for_flush(epoch=0)
    Note over TX: Blocks here until epoch flushed

    BG->>BG: Wake (max_delay_ms timer)
    BG->>Disk: fsync() entire batch
    Disk-->>BG: confirmed
    BG->>Coord: mark_flushed(epoch=0)
    Coord->>Coord: Advance to epoch=1
    Coord-->>TX: epoch=0 flushed!

    TX-->>App: committed (ACID guaranteed)

    Note over App,Disk: Multiple transactions in same epoch<br/>share single fsync - amortized cost
```

### Async Mode

```mermaid
sequenceDiagram
    participant App as Application
    participant TX as Transaction
    participant Buffer as Ring Buffer
    participant BG as Background Thread
    participant Disk as Disk

    App->>TX: write(data)
    TX->>Buffer: push(entry)
    Buffer-->>TX: queued
    TX-->>App: committed (immediate)

    Note over App: Returns without waiting

    loop Continuous
        BG->>Buffer: drain(batch)
        BG->>Disk: write + fsync
        Disk-->>BG: confirmed
    end
```

## Component Architecture

```mermaid
graph TB
    subgraph "Write Path"
        APP[Application]
        TX[WriteTransaction]
        WAL[ConcurrentWalSystem]
    end

    subgraph "Lock-Free Append Path"
        LSN[LSN Allocator<br/>AtomicU64]
        S0[Stripe 0<br/>Ring Buffer]
        S1[Stripe 1<br/>Ring Buffer]
        SN[Stripe N<br/>Ring Buffer]
    end

    subgraph "Durability Modes"
        SYNC[Synchronous<br/>Immediate flush]
        GC[GroupCommit<br/>Epoch-based]
        ASYNC[Async<br/>Background flush]
    end

    subgraph "Coordination"
        FLUSH[Flush Coordinator]
        GCCOORD[GroupCommitCoordinator]
        EPOCH[Epoch Tracker]
    end

    subgraph "Storage"
        SEG[Segment Manager]
        FILE[WAL Segment Files]
        DISK[(Disk)]
    end

    APP --> TX
    TX --> WAL
    WAL --> LSN

    LSN --> S0
    LSN --> S1
    LSN --> SN

    S0 --> FLUSH
    S1 --> FLUSH
    SN --> FLUSH

    FLUSH --> SYNC
    FLUSH --> GC
    FLUSH --> ASYNC

    GC --> GCCOORD
    GCCOORD --> EPOCH
    EPOCH -.wait.-> TX

    SYNC --> SEG
    GC --> SEG
    ASYNC --> SEG

    SEG --> FILE
    FILE --> DISK
```

**Key differences from previous architecture:**
- Lock-free ring buffers replace single mutex-protected WAL
- LSN Allocator provides globally ordered sequence numbers
- Flush Coordinator sorts entries by LSN before writing
- All modes share the same lock-free append path

## Configuration

### DurabilityMode Enum

```rust
pub enum DurabilityMode {
    /// Every commit waits for its own fsync
    /// Latency: ~1.5ms | Throughput: ~600/sec | ACID: ✅
    Synchronous,

    /// Multiple commits wait for shared fsync (epoch-based)
    /// Latency: ~10-50ms | Throughput: ~100K+/sec | ACID: ✅
    /// CRITICAL: Transactions BLOCK until their epoch is flushed
    GroupCommit {
        max_delay_ms: u64,      // default: 10ms
        max_batch_size: usize,  // default: 200
    },

    /// Background thread handles fsync, commits return immediately
    /// Latency: <100ns | Throughput: ~500K+/sec | ACID: ❌ (eventual)
    Async {
        flush_interval_ms: u64, // default: 10ms
    },

    /// Like Async but with epoch tracking (for metrics/observability)
    /// Latency: <100ns | Throughput: ~500K+/sec | ACID: ❌ (eventual)
    AsyncBatched {
        max_delay_ms: u64,      // default: 10ms
        max_batch_size: usize,  // default: 200
    },
}
```

### WriteOptions for Per-Transaction Override

```mermaid
graph LR
    subgraph "Global Config (WalConfig)"
        GC[default_durability: GroupCommit]
    end

    subgraph "Per-Transaction Override (WriteOptions)"
        TO1[None → use global default]
        TO2[Some\(Synchronous\) → override for critical txn]
        TO3[Some\(Async\) → override for bulk import]
        TO4[Some\(GroupCommit\) → override params]
    end

    subgraph "Effective Mode"
        EFF[Resolved Durability Mode]
    end

    GC --> TO1
    TO1 --> EFF
    TO2 --> EFF
    TO3 --> EFF
    TO4 --> EFF

    Note1[Example: Bulk import uses Async<br/>while rest of DB uses GroupCommit]
    Note2[Example: Financial txn uses Sync<br/>while rest of DB uses GroupCommit]
```

**Convenience Presets:**

```rust
// Preset for critical operations (Synchronous mode)
db.write_with_options(WriteOptions::critical(), |tx| {
    tx.create_node("Payment", payment_data)
})?;

// Preset for bulk imports (Async mode, 100ms flush)
db.write_with_options(WriteOptions::bulk_import(), |tx| {
    for record in bulk_data {
        tx.create_node("Record", record)?;
    }
    Ok(())
})?;

// Custom configuration (builder pattern)
let options = WriteOptions::new()
    .with_durability(DurabilityMode::GroupCommit {
        max_delay_ms: 5,
        max_batch_size: 500,
    });
```

## GroupCommit Mode Internals

### Epoch-Based Coordination

GroupCommit uses an **epoch-based** system where transactions register with the current epoch and wait for that epoch to flush:

```mermaid
stateDiagram-v2
    [*] --> Epoch_0

    Epoch_0 --> Epoch_0: register_transaction()\n[batch_count++]
    Epoch_0 --> Flushing_0: batch_count >= max_batch_size\nOR max_delay_ms timer
    Flushing_0 --> Epoch_1: mark_flushed()\n[wake all waiters]

    Epoch_1 --> Epoch_1: register_transaction()
    Epoch_1 --> Flushing_1: trigger condition
    Flushing_1 --> Epoch_2: mark_flushed()

    note right of Epoch_0
        Multiple transactions accumulate
        All assigned epoch 0
        Transactions WAIT for flush
    end note

    note right of Flushing_0
        Background thread fsyncs
        Single fsync for ALL epoch 0 txns
        Amortized cost across batch
    end note
```

### Transaction Wait Flow

```mermaid
sequenceDiagram
    participant TX1 as Transaction 1
    participant TX2 as Transaction 2
    participant Coord as Coordinator
    participant BG as Flush Thread

    TX1->>Coord: register() → epoch=0
    TX2->>Coord: register() → epoch=0

    par TX1 waits
        TX1->>Coord: wait_for_flush(0)
        Note over TX1: BLOCKED
    and TX2 waits
        TX2->>Coord: wait_for_flush(0)
        Note over TX2: BLOCKED
    end

    BG->>BG: max_delay_ms expires
    BG->>BG: fsync() - flushes both TX1 and TX2
    BG->>Coord: mark_flushed(epoch=0)
    Coord->>Coord: flushed_epoch = 1<br/>current_epoch = 1

    par Notify waiters
        Coord-->>TX1: epoch 0 flushed!
        Coord-->>TX2: epoch 0 flushed!
    end

    TX1-->>TX1: Return to application
    TX2-->>TX2: Return to application

    Note over TX1,TX2: ACID guaranteed - data on disk
```

### Critical Implementation Details

#### Race Condition Fix (Commit 6bd42ff)

**Problem:** There was a race between releasing the WAL lock after `commit_with_mode()` and re-acquiring it to get the coordinator reference:

```rust
// BUGGY CODE (race condition):
let epoch = wal.commit_with_mode(...)?;
// WAL lock released here
let coordinator = self.wal.lock_or_err()?.group_commit_coordinator().cloned();
// ^ coordinator could be None if WAL reconfigured!
if let Some(gc) = coordinator {
    gc.wait_for_flush(epoch)?;  // Might skip wait!
}
```

**Impact:** Silent durability violation if transaction registered with coordinator but coordinator was cleared before wait.

**Fix:** Clone coordinator reference **before** releasing WAL lock:

```rust
// SAFE CODE (no race):
let epoch = wal.commit_with_mode(...)?;
let gc = wal.group_commit_coordinator().cloned();  // Clone BEFORE lock release
// WAL lock released here
if let Some(gc) = gc {
    gc.wait_for_flush(epoch)?;  // Always waits on correct coordinator
}
```

```mermaid
sequenceDiagram
    participant TX as Transaction
    participant WAL as WAL (locked)
    participant Coord as Coordinator

    TX->>WAL: commit_with_mode()
    WAL->>Coord: register_transaction() → epoch
    WAL-->>TX: epoch

    Note over TX,WAL: CRITICAL SECTION - still holding lock

    TX->>WAL: group_commit_coordinator()
    WAL-->>TX: Arc<Coordinator>
    TX->>TX: clone coordinator

    Note over TX: NOW safe to release lock

    TX->>TX: Release WAL lock
    TX->>Coord: wait_for_flush(epoch)
    Note over Coord: No race - we have our coordinator
```

#### Piggybacking Optimization

**Concept:** Synchronous commits can "piggyback" pending async data by triggering an immediate flush before their own fsync.

```mermaid
sequenceDiagram
    participant Async as Async Transaction
    participant Sync as Sync Transaction
    participant WAL as WAL
    participant Signal as FlushSignal
    participant BG as Background Thread

    Async->>WAL: write() with Async mode
    WAL->>WAL: buffer entry
    WAL-->>Async: return (no fsync)
    Note over WAL: has_pending_data = true

    Sync->>WAL: write() with Synchronous mode
    WAL->>WAL: Check has_pending_data?

    alt Pending data exists
        WAL->>Signal: request_flush()
        Signal->>BG: wake up!
        BG->>WAL: fsync() pending data
        Note over BG: Async data flushed "for free"
    end

    WAL->>WAL: fsync() sync transaction
    WAL-->>Sync: return

    Note over Async,BG: Async data durably flushed<br/>without waiting for timer
```

**Benefits:**
- Async writes get durability "for free" when sync writes occur
- Reduces time window for data loss
- No performance penalty for sync writes
- Opportunistic optimization

## Async Mode Internals

```mermaid
graph TB
    subgraph "Writer Threads"
        W1[Writer 1]
        W2[Writer 2]
        W3[Writer N]
    end

    subgraph "Lock-Free Buffer"
        RB[Ring Buffer / Channel]
    end

    subgraph "Background Sync"
        BG[Sync Thread]
        BATCH[Batch Accumulator]
    end

    subgraph "Disk I/O"
        WAL[WAL File]
        FS[fsync]
    end

    W1 -->|push| RB
    W2 -->|push| RB
    W3 -->|push| RB

    RB -->|drain| BG
    BG --> BATCH
    BATCH -->|write| WAL
    WAL --> FS

    style RB fill:#f9f,stroke:#333
    style BG fill:#bbf,stroke:#333
```

### Backpressure Handling

```mermaid
flowchart TD
    A[append entry] --> B{Buffer full?}
    B -->|No| C[Push to buffer]
    C --> D[Return success]

    B -->|Yes| E{Backpressure policy?}
    E -->|Block| F[Wait for space]
    F --> C

    E -->|Fallback| G[Switch to Sync mode]
    G --> H[Direct fsync]
    H --> D

    E -->|Error| I[Return BufferFull error]
```

## Performance Comparison

```mermaid
graph LR
    subgraph "Latency"
        SYNC_L[Sync: ~1.5ms]
        GC_L[GroupCommit: ~10-50ms]
        ASYNC_L[Async: <100ns]
    end

    subgraph "Throughput"
        SYNC_T[~600/sec]
        GC_T[~100K+/sec]
        ASYNC_T[~500K+/sec]
    end

    subgraph "ACID Compliance"
        SYNC_A[✅ Full]
        GC_A[✅ Full]
        ASYNC_A[❌ Eventual]
    end

    SYNC_L -.->|Higher latency but..| GC_L
    GC_L -.->|Much higher latency but..| ASYNC_L

    SYNC_T -.->|150x more| GC_T
    GC_T -.->|5x more| ASYNC_T

    SYNC_A -.->|Same ACID| GC_A
    GC_A -.->|Loses ACID| ASYNC_A

    Note[GroupCommit: Best balance<br/>ACID + High throughput<br/>Lock-free concurrent append]
```

## Use Case Decision Tree

```mermaid
flowchart TD
    A[Choose Durability Mode] --> B{Need ACID guarantees?}
    B -->|Yes| C{Can tolerate 10-50ms latency?}

    C -->|No| SYNC[Use Synchronous]
    C -->|Yes| GC[Use GroupCommit]

    B -->|No| D{Bulk import / High throughput?}
    D -->|Yes| ASYNC[Use Async]
    D -->|No| GC2[Use GroupCommit\nBest default]

    SYNC --> SYNC_DESC[Individual fsync per txn<br/>~1.5ms latency<br/>~600 writes/sec<br/>✅ ACID]
    GC --> GC_DESC[Shared fsync across batch<br/>~10-50ms latency<br/>~15K writes/sec<br/>✅ ACID]
    GC2 --> GC_DESC
    ASYNC --> ASYNC_DESC[Background fsync<br/>~6µs latency<br/>~100K+ writes/sec<br/>❌ Eventual consistency]
```

## Graceful Shutdown

All modes ensure pending writes are synced on shutdown using `FlushGuard`:

```mermaid
sequenceDiagram
    participant App as Application
    participant DB as GallifreyDB
    participant WAL as WAL
    participant Guard as FlushGuard
    participant BG as Background Thread

    App->>DB: shutdown()
    DB->>WAL: close()

    alt GroupCommit Mode
        WAL->>Guard: drop(FlushGuard)
        Guard->>BG: signal shutdown
        BG->>BG: Final flush
        BG->>BG: fsync() all pending
        BG->>BG: Exit thread
        Guard->>Guard: join() thread
        Note over Guard: Blocks until thread complete
    else Async Mode
        WAL->>Guard: drop(FlushGuard)
        Guard->>BG: signal shutdown
        BG->>BG: Final flush
        BG->>BG: fsync() all pending
        BG->>BG: Exit thread
        Guard->>Guard: join() thread
    end

    WAL-->>DB: closed
    DB-->>App: shutdown complete

    Note over App,BG: FlushGuard RAII ensures<br/>no data loss on shutdown
```

## Related Documentation

- [ADR-0020: Concurrent WAL Architecture](../adr/0020-concurrent-wal-architecture.md)
- [ADR-0012: Configurable Durability Modes](../adr/0012-configurable-durability-modes.md)
- [ADR-0007: Write-Ahead Log for Durability](../adr/0007-wal-durability.md)
- [WAL Format Documentation](../WAL.md)
