# Storage Layer Architecture

The storage layer is responsible for persisting graph data with full bi-temporal tracking while maintaining high performance for current-state queries.

## Overview

```mermaid
graph TB
    subgraph "Storage Layer"
        direction TB

        subgraph "Current Storage"
            CS["CurrentStorage"]
            CS_N["nodes: DashMap"]
            CS_E["edges: DashMap"]
            CS_IDX["CurrentIndexes"]
        end

        subgraph "Historical Storage"
            HS["HistoricalStorage"]
            HS_NV["node_versions"]
            HS_EV["edge_versions"]
            HS_CFG["AnchorConfig"]
        end

        subgraph "Durability"
            WAL["WriteAheadLog"]
            CP["Checkpoints"]
        end
    end

    API["Transaction API"] --> CS
    API --> HS
    CS --> WAL
    HS --> WAL
    WAL --> CP

    style CS fill:#90EE90
    style HS fill:#87CEEB
    style WAL fill:#FFB6C1
```

## Current Storage

### Purpose

Optimized for fast current-state queries with zero temporal overhead.

### Structure

```mermaid
classDiagram
    class CurrentStorage {
        +nodes: DashMap~NodeId, Node~
        +edges: DashMap~EdgeId, Edge~
        +indexes: CurrentIndexes
        +node_id_gen: IdGenerator
        +edge_id_gen: IdGenerator
        +version_id_gen: IdGenerator
    }

    class CurrentIndexes {
        +nodes: DashMap~NodeId, Node~
        +edges: DashMap~EdgeId, Edge~
        +outgoing: RwLock~AdjacencyIndex~
        +incoming: RwLock~AdjacencyIndex~
    }

    class Node {
        +id: NodeId
        +label: InternedString
        +properties: PropertyMap
        +current_version: VersionId
    }

    CurrentStorage --> CurrentIndexes
    CurrentIndexes --> Node
```

### Operations

| Operation | Complexity | Lock Type |
|-----------|------------|-----------|
| `get_node(id)` | O(1) | Lock-free |
| `get_edge(id)` | O(1) | Lock-free |
| `insert_node(node)` | O(1) | Shard lock |
| `get_outgoing_edges(node)` | O(k) | Read lock |
| `rebuild_adjacency()` | O(E log E) | Write lock |

### Memory Layout

```mermaid
graph LR
    subgraph "DashMap Sharding"
        S0["Shard 0<br/>RwLock"]
        S1["Shard 1<br/>RwLock"]
        S2["Shard 2<br/>RwLock"]
        SN["Shard 63<br/>RwLock"]
    end

    H["Hash(NodeId)"] --> S0
    H --> S1
    H --> S2
    H --> SN

    style S0 fill:#90EE90
    style S1 fill:#90EE90
    style S2 fill:#90EE90
    style SN fill:#90EE90
```

## Historical Storage

### Purpose

Maintains version chains with anchor+delta compression for storage efficiency.

### Version Chain Structure

```mermaid
graph LR
    subgraph "Version Chain for Node 42"
        V1["V1: Anchor<br/>name=Alice<br/>age=30<br/>city=NYC"]
        V2["V2: Delta<br/>+age=31"]
        V3["V3: Delta<br/>+score=95"]
        V4["V4: Delta<br/>-city"]
        V5["V5: Anchor<br/>name=Alice<br/>age=31<br/>score=95"]
        V6["V6: Delta<br/>+title=Dr"]
    end

    V1 --> V2 --> V3 --> V4 --> V5 --> V6

    style V1 fill:#FFD700
    style V5 fill:#FFD700
```

### Anchor Decision Logic

```mermaid
flowchart TD
    START["New Version"] --> CHECK1{"First version<br/>for entity?"}
    CHECK1 -->|Yes| ANCHOR["Create Anchor"]
    CHECK1 -->|No| CHECK2{"version_count %<br/>anchor_interval == 0?"}
    CHECK2 -->|Yes| ANCHOR
    CHECK2 -->|No| CHECK3{"delta_chain_length >=<br/>max_delta_chain?"}
    CHECK3 -->|Yes| ANCHOR
    CHECK3 -->|No| DELTA["Create Delta"]

    ANCHOR --> STORE["Store Version"]
    DELTA --> STORE
```

### Configuration

```rust
pub struct AnchorConfig {
    /// Create anchor every N versions (default: 10)
    pub anchor_interval: usize,

    /// Force anchor if chain exceeds this (default: 20)
    pub max_delta_chain: usize,
}
```

### Reconstruction Algorithm

```mermaid
sequenceDiagram
    participant Q as Query
    participant HS as HistoricalStorage
    participant VC as VersionChain

    Q->>HS: reconstruct_at(version_id)
    HS->>VC: Walk backward

    loop Until Anchor Found
        VC->>VC: Collect delta
        VC->>VC: Move to prev_version
    end

    VC-->>HS: Anchor + Deltas[]

    HS->>HS: Start with anchor properties
    loop Apply deltas in forward order
        HS->>HS: Apply changed properties
        HS->>HS: Remove deleted properties
    end

    HS-->>Q: Reconstructed PropertyMap
```

### Storage Efficiency

```mermaid
pie title Storage Comparison (1000 versions, 10% change per version)
    "Full Copies" : 1000
    "Anchor+Delta" : 145
```

| Scenario | Full Copies | Anchor+Delta | Savings |
|----------|-------------|--------------|---------|
| 100 versions, 10% change | 100x | ~19x | 81% |
| 1000 versions, 5% change | 1000x | ~145x | 85% |

## Write-Ahead Log

### Purpose

Ensures durability by logging operations before applying them.

### Entry Format

```mermaid
graph LR
    subgraph "WAL Entry Structure"
        LSN["LSN<br/>8 bytes"]
        TS["Timestamp<br/>8 bytes"]
        OP["Operation<br/>Variable"]
        CRC["CRC32<br/>4 bytes"]
    end

    LSN --> TS --> OP --> CRC
```

### Operations Logged

```mermaid
classDiagram
    class WalOperation {
        <<enumeration>>
        CreateNode
        CreateEdge
        UpdateNode
        UpdateEdge
        DeleteNode
        DeleteEdge
        BeginTransaction
        CommitTransaction
        AbortTransaction
    }

    class CreateNode {
        +node_id: NodeId
        +label: String
        +properties: PropertyMap
        +valid_time: TimeRange
    }

    class UpdateNode {
        +node_id: NodeId
        +properties: PropertyMap
        +valid_time: TimeRange
    }

    WalOperation --> CreateNode
    WalOperation --> UpdateNode
```

### Write Path

```mermaid
sequenceDiagram
    participant TX as Transaction
    participant WAL as WriteAheadLog
    participant BUF as BufWriter
    participant FILE as File

    TX->>WAL: append(entry)
    WAL->>WAL: Serialize entry
    WAL->>WAL: Calculate CRC32
    WAL->>BUF: write_all(bytes)

    TX->>WAL: sync()
    WAL->>BUF: flush()
    WAL->>FILE: sync_all() [fsync]
    FILE-->>WAL: Durable
    WAL-->>TX: Ok
```

### Recovery Process

```mermaid
flowchart TD
    START["Database Start"] --> CHECK{"WAL exists?"}
    CHECK -->|No| INIT["Initialize fresh"]
    CHECK -->|Yes| RECOVER["Begin Recovery"]

    RECOVER --> READ["Read WAL entries"]
    READ --> VERIFY{"Valid CRC?"}
    VERIFY -->|No| STOP["Stop at corruption"]
    VERIFY -->|Yes| TRACK["Track transaction state"]

    TRACK --> COMMITTED{"Transaction<br/>committed?"}
    COMMITTED -->|Yes| APPLY["Apply operation"]
    COMMITTED -->|No| SKIP["Skip (rollback)"]

    APPLY --> MORE{"More entries?"}
    SKIP --> MORE
    MORE -->|Yes| READ
    MORE -->|No| DONE["Recovery complete"]

    DONE --> TRUNCATE["Truncate incomplete TX"]
```

## Checkpointing

### Purpose

Reduce recovery time by creating periodic snapshots.

### Checkpoint Structure

```mermaid
graph TB
    subgraph "Checkpoint"
        META["Metadata<br/>timestamp, lsn"]
        SNAP_C["Current Storage<br/>Snapshot"]
        SNAP_H["Historical Storage<br/>Snapshot"]
        SNAP_I["Index State"]
    end

    META --> SNAP_C
    META --> SNAP_H
    META --> SNAP_I
```

### Recovery with Checkpoint

```mermaid
sequenceDiagram
    participant DB as Database
    participant CP as Checkpoint
    participant WAL as WAL

    DB->>CP: Load latest checkpoint
    CP-->>DB: State at LSN 1000

    DB->>WAL: Replay from LSN 1001
    loop For each entry > checkpoint LSN
        WAL->>DB: Apply operation
    end

    DB->>DB: Ready for queries
```

## Data Flow Summary

```mermaid
flowchart LR
    subgraph "Write Path"
        W1["Buffer Write"] --> W2["Validate"]
        W2 --> W3["Log to WAL"]
        W3 --> W4["fsync"]
        W4 --> W5["Apply to Current"]
        W5 --> W6["Create Version"]
        W6 --> W7["Update Indexes"]
    end

    subgraph "Read Path (Current)"
        R1["Query"] --> R2["DashMap Lookup"]
        R2 --> R3["Return Node"]
    end

    subgraph "Read Path (Historical)"
        H1["Time Query"] --> H2["Temporal Index"]
        H2 --> H3["Find Version"]
        H3 --> H4["Reconstruct"]
        H4 --> H5["Return State"]
    end
```

## Related Documentation

- [ADR-0001: Hybrid Storage Architecture](../adr/0001-hybrid-storage-architecture.md)
- [ADR-0004: Anchor+Delta Compression](../adr/0004-anchor-delta-compression.md)
- [ADR-0007: Write-Ahead Log](../adr/0007-wal-durability.md)
