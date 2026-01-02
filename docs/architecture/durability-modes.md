# Durability Modes Architecture

This document describes the architecture of GallifreyDB's configurable durability modes, which control when WAL data is synchronized to disk.

## Overview

GallifreyDB supports three durability modes that trade off write latency against durability guarantees:

| Mode | Write Latency | Throughput | Data at Risk |
|------|---------------|------------|--------------|
| **Synchronous** | ~1.5ms | ~600/sec | None |
| **Batched** | ~60µs | ~15K/sec | Up to 100 ops or 10ms |
| **Async** | ~6µs | ~100K+/sec | ~10ms |

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

### Batched Mode

```mermaid
sequenceDiagram
    participant App as Application
    participant TX as Transaction
    participant WAL as WAL Buffer
    participant Timer as Sync Timer
    participant Disk as Disk

    App->>TX: write(data)
    TX->>WAL: append(entry)
    WAL->>WAL: increment counter
    WAL-->>TX: buffered
    TX-->>App: committed (fast)

    Note over WAL: Counter < batch_size

    App->>TX: write(data)
    TX->>WAL: append(entry)
    WAL->>WAL: counter == batch_size

    WAL->>Disk: fsync()
    Disk-->>WAL: confirmed
    WAL->>WAL: reset counter

    Note over Timer: Or max_delay expires
    Timer->>WAL: trigger sync
    WAL->>Disk: fsync()
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
        WAL[WriteAheadLog]
    end

    subgraph "Durability Modes"
        SYNC[Synchronous Handler]
        BATCH[Batched Handler]
        ASYNC[Async Handler]
    end

    subgraph "Sync Infrastructure"
        TIMER[Batch Timer]
        BGTHREAD[Background Thread]
        BUFFER[Ring Buffer]
    end

    subgraph "Storage"
        FILE[WAL File]
        DISK[(Disk)]
    end

    APP --> TX
    TX --> WAL

    WAL --> SYNC
    WAL --> BATCH
    WAL --> ASYNC

    SYNC --> FILE
    BATCH --> TIMER
    BATCH --> FILE
    ASYNC --> BUFFER
    BUFFER --> BGTHREAD
    BGTHREAD --> FILE

    FILE --> DISK
```

## Configuration

### DurabilityMode Enum

```rust
pub enum DurabilityMode {
    /// Every commit waits for fsync
    Synchronous,

    /// Batch multiple commits before fsync
    Batched {
        batch_size: usize,      // default: 100
        max_delay_ms: u64,      // default: 10
    },

    /// Background thread handles fsync
    Async,
}
```

### WriteOptions for Per-Transaction Override

```mermaid
graph LR
    subgraph "Global Config"
        GC[default_durability: Batched]
    end

    subgraph "Transaction Options"
        TO1[None → use global]
        TO2[Some(Sync) → override]
        TO3[Some(Async) → override]
    end

    subgraph "Effective Mode"
        EFF[Resolved Durability]
    end

    GC --> TO1
    TO1 --> EFF
    TO2 --> EFF
    TO3 --> EFF
```

## Batched Mode Internals

```mermaid
stateDiagram-v2
    [*] --> Idle

    Idle --> Buffering: append(entry)
    Buffering --> Buffering: append(entry)\n[count < batch_size]
    Buffering --> Syncing: count >= batch_size
    Buffering --> Syncing: timer expired
    Syncing --> Idle: fsync complete

    note right of Buffering
        Writes return immediately
        Entries buffered in memory
    end note

    note right of Syncing
        Single fsync for entire batch
        Much more efficient
    end note
```

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
    subgraph "Latency (log scale)"
        SYNC_L[Sync: 1.5ms]
        BATCH_L[Batched: 60µs]
        ASYNC_L[Async: 6µs]
    end

    subgraph "Throughput"
        SYNC_T[600/sec]
        BATCH_T[15K/sec]
        ASYNC_T[100K+/sec]
    end

    SYNC_L -.->|25x faster| BATCH_L
    BATCH_L -.->|10x faster| ASYNC_L

    SYNC_T -.->|25x more| BATCH_T
    BATCH_T -.->|7x more| ASYNC_T
```

## Use Case Decision Tree

```mermaid
flowchart TD
    A[Choose Durability Mode] --> B{Financial/Audit data?}
    B -->|Yes| SYNC[Use Synchronous]

    B -->|No| C{Bulk import?}
    C -->|Yes| ASYNC[Use Async]

    C -->|No| D{High throughput needed?}
    D -->|Yes| E{Can tolerate 10ms loss?}
    E -->|Yes| ASYNC
    E -->|No| BATCH[Use Batched]

    D -->|No| BATCH

    SYNC --> SYNC_DESC[Zero data loss\n~600 writes/sec]
    BATCH --> BATCH_DESC[Balanced\n~15K writes/sec]
    ASYNC --> ASYNC_DESC[Maximum speed\n~100K+ writes/sec]
```

## Graceful Shutdown

All modes ensure pending writes are synced on shutdown:

```mermaid
sequenceDiagram
    participant App as Application
    participant DB as GallifreyDB
    participant WAL as WAL
    participant BG as Background Thread

    App->>DB: shutdown()
    DB->>WAL: close()

    alt Batched Mode
        WAL->>WAL: flush pending batch
        WAL->>WAL: fsync()
    else Async Mode
        WAL->>BG: signal shutdown
        BG->>BG: drain buffer
        BG->>WAL: fsync()
        BG-->>WAL: shutdown complete
    end

    WAL-->>DB: closed
    DB-->>App: shutdown complete
```

## Related Documentation

- [ADR-0012: Configurable Durability Modes](../adr/0012-configurable-durability-modes.md)
- [ADR-0007: Write-Ahead Log for Durability](../adr/0007-wal-durability.md)
- [WAL Format and Migration](../../CLAUDE.md#wal-format-and-migration)
