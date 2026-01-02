# GallifreyDB Architecture

> A high-performance bi-temporal graph database designed for LLM knowledge evolution tracking

## Overview

GallifreyDB combines three powerful concepts:
- **Graph Database**: Nodes and edges with property storage
- **Bi-Temporal Tracking**: Valid time + transaction time
- **LLM Integration**: Enabling AI reasoning about knowledge evolution

```mermaid
graph TB
    subgraph "GallifreyDB Architecture"
        API["API Layer<br/>Read/Write Transactions"]
        QE["Query Engine<br/>Graph + Temporal + Vector"]

        subgraph "Storage Layer"
            CS["Current Storage<br/>Fast Path"]
            HS["Historical Storage<br/>Temporal Path"]
            WAL["Write-Ahead Log<br/>Durability"]
        end

        subgraph "Index Layer"
            CI["Current Indexes<br/>DashMap + CSR"]
            TI["Temporal Indexes<br/>B-Tree"]
            VI["Vector Index<br/>HNSW (Future)"]
        end
    end

    LLM["LLM Client"]

    LLM --> API
    API --> QE
    QE --> CS
    QE --> HS
    QE --> CI
    QE --> TI
    CS --> WAL
    HS --> WAL

    style CS fill:#90EE90
    style HS fill:#87CEEB
    style WAL fill:#FFB6C1
```

## Core Design Principles

### 1. Hybrid Storage Architecture

Separate paths for current-state and historical queries:

```mermaid
flowchart LR
    subgraph "Query Types"
        CQ["Current Query<br/>'What is X now?'"]
        TQ["Temporal Query<br/>'What was X at time T?'"]
    end

    subgraph "Storage Paths"
        CS["Current Storage<br/>Zero overhead<br/>Lock-free reads"]
        HS["Historical Storage<br/>Anchor+Delta<br/>Compressed"]
    end

    CQ -->|Fast Path| CS
    TQ -->|Temporal Path| HS

    style CS fill:#90EE90
    style HS fill:#87CEEB
```

### 2. Bi-Temporal Model

Track two independent time dimensions:

```mermaid
graph TB
    subgraph "Bi-Temporal Dimensions"
        VT["Valid Time (VT)<br/>When fact was TRUE in reality"]
        TT["Transaction Time (TT)<br/>When fact was RECORDED"]
    end

    subgraph "Query Patterns"
        Q1["as_of(VT)<br/>'What was true at time T?'"]
        Q2["as_of(TT)<br/>'What did we know at time T?'"]
        Q3["as_of(VT, TT)<br/>'What did we know about T1 at T2?'"]
    end

    VT --> Q1
    TT --> Q2
    VT --> Q3
    TT --> Q3
```

### 3. ACID with MVCC

Full transactional guarantees with high concurrency:

```mermaid
sequenceDiagram
    participant C as Client
    participant TX as Transaction
    participant CS as Current Storage
    participant WAL as Write-Ahead Log
    participant HS as Historical Storage

    C->>TX: begin_write()
    TX->>TX: Capture snapshot

    C->>TX: create_node()
    TX->>TX: Buffer in memory

    C->>TX: commit()
    TX->>TX: Detect conflicts
    TX->>WAL: Log operations
    WAL-->>TX: fsync complete
    TX->>CS: Apply changes
    TX->>HS: Create versions
    TX-->>C: Success
```

## Architecture Layers

### Layer 1: Core Types

Foundational data structures used throughout:

```mermaid
classDiagram
    class NodeId {
        +u64 inner
    }
    class EdgeId {
        +u64 inner
    }
    class VersionId {
        +u64 inner
    }
    class InternedString {
        +u32 inner
        +resolve() String
    }
    class BiTemporalInterval {
        +TimeRange valid_time
        +TimeRange transaction_time
    }
    class TimeRange {
        +Timestamp start
        +Timestamp end
        +contains(Timestamp) bool
        +overlaps(TimeRange) bool
    }
    class PropertyMap {
        +Arc~HashMap~ inner
        +get(key) PropertyValue
        +iter() Iterator
    }
    class PropertyValue {
        <<enumeration>>
        Null
        Bool(bool)
        Int(i64)
        Float(f64)
        String(Arc~str~)
        Bytes(Arc~[u8]~)
        Array(Arc~Vec~)
    }

    BiTemporalInterval --> TimeRange
    PropertyMap --> PropertyValue
```

### Layer 2: Graph Entities

Nodes and edges with temporal versioning:

```mermaid
classDiagram
    class Node {
        +NodeId id
        +InternedString label
        +PropertyMap properties
        +VersionId current_version
    }
    class Edge {
        +EdgeId id
        +NodeId source
        +NodeId target
        +InternedString label
        +PropertyMap properties
        +VersionId current_version
    }
    class NodeVersion {
        +VersionId version_id
        +NodeId node_id
        +BiTemporalInterval temporal
        +InternedString label
        +VersionData data
    }
    class VersionData {
        <<enumeration>>
        Anchor(PropertyMap)
        Delta(PropertyDelta, VersionId)
    }
    class PropertyDelta {
        +HashMap changed
        +HashSet removed
        +apply(PropertyMap) PropertyMap
    }

    Node --> NodeVersion : versions
    NodeVersion --> VersionData
    VersionData --> PropertyDelta
```

### Layer 3: Storage

Dual-path storage with WAL durability:

```mermaid
graph TB
    subgraph "Current Storage"
        CS_NODES["nodes: DashMap<NodeId, Node>"]
        CS_EDGES["edges: DashMap<EdgeId, Edge>"]
        CS_IDG["ID Generators (Atomic)"]
    end

    subgraph "Historical Storage"
        HS_CHAINS["Version Chains"]
        HS_ANCHOR["Anchor Config"]

        subgraph "Version Chain Example"
            V1["V1: Anchor<br/>Full Properties"]
            V2["V2: Delta<br/>+name, -age"]
            V3["V3: Delta<br/>+score"]
            V4["V4: Anchor<br/>Full Properties"]
            V1 --> V2 --> V3 --> V4
        end
    end

    subgraph "Write-Ahead Log"
        WAL_FILE["wal.log"]
        WAL_ENTRY["[LSN][Timestamp][Op][CRC32]"]
    end

    CS_NODES --> WAL_FILE
    CS_EDGES --> WAL_FILE
    HS_CHAINS --> WAL_FILE
```

### Layer 4: Indexes

Multiple index strategies for different query types:

```mermaid
graph TB
    subgraph "Current Indexes"
        CI_NODES["Node Index<br/>DashMap O(1)"]
        CI_EDGES["Edge Index<br/>DashMap O(1)"]
        CI_OUT["Outgoing Adjacency<br/>CSR Format"]
        CI_IN["Incoming Adjacency<br/>CSR Format"]
    end

    subgraph "Temporal Indexes"
        TI_VT["Valid Time Index<br/>BTreeMap"]
        TI_TT["Transaction Time Index<br/>BTreeMap"]
    end

    subgraph "CSR Format Detail"
        OFFSETS["offsets: [0, 3, 5, 9, ...]"]
        EDGES["edges: [AdjEntry, AdjEntry, ...]"]
        OFFSETS --> EDGES
    end
```

### Layer 5: Transactions

MVCC with Snapshot Isolation:

```mermaid
stateDiagram-v2
    [*] --> Active: begin()
    Active --> Active: read/write ops
    Active --> Preparing: commit() called
    Preparing --> Committed: validation passed
    Preparing --> Aborted: conflict detected
    Active --> Aborted: rollback()
    Committed --> [*]
    Aborted --> [*]
```

## Data Flow

### Write Path

```mermaid
sequenceDiagram
    participant App
    participant WTX as WriteTransaction
    participant WB as WriteBuffer
    participant WAL
    participant CS as CurrentStorage
    participant HS as HistoricalStorage
    participant IDX as Indexes

    App->>WTX: write_transaction()
    WTX->>WTX: Generate TxId, capture snapshot

    App->>WTX: create_node("Person", props)
    WTX->>WB: Buffer CreateNode op
    WTX-->>App: NodeId

    App->>WTX: commit()
    WTX->>WTX: Validate buffered ops
    WTX->>WTX: Check write-write conflicts
    WTX->>WAL: Append WAL entries
    WAL->>WAL: fsync()

    WTX->>CS: Insert node
    WTX->>HS: Create version (anchor)
    WTX->>IDX: Update temporal indexes
    WTX->>IDX: Rebuild adjacency (batched)

    WTX-->>App: Ok(())
```

### Read Path

```mermaid
sequenceDiagram
    participant App
    participant RTX as ReadTransaction
    participant CS as CurrentStorage
    participant IDX as CurrentIndexes

    App->>RTX: read_transaction()
    RTX->>RTX: Capture snapshot

    App->>RTX: get_node(node_id)
    RTX->>CS: Lookup node
    RTX->>RTX: Check visibility
    RTX-->>App: Node

    App->>RTX: get_outgoing_edges(node_id)
    RTX->>IDX: CSR lookup
    RTX->>RTX: Filter by visibility
    RTX-->>App: Vec<EdgeId>
```

### Time-Travel Path

```mermaid
sequenceDiagram
    participant App
    participant DB
    participant TI as TemporalIndex
    participant HS as HistoricalStorage

    App->>DB: as_of(timestamp).get_node(id)
    DB->>TI: Find versions at timestamp
    TI-->>DB: VersionId

    DB->>HS: Get version chain
    HS->>HS: Walk to nearest anchor
    HS->>HS: Collect deltas
    HS->>HS: Reconstruct properties
    HS-->>DB: NodeVersion

    DB-->>App: Historical Node State
```

## Performance Characteristics

### Target Metrics

| Operation | Target | Achieved |
|-----------|--------|----------|
| Single-hop traversal | <1µs | ~22ns |
| 3-hop traversal | <100µs | TBD |
| Time-travel reconstruction | <10ms | ~20ns* |
| Write transaction | <10ms | ~7-12µs |
| Storage overhead | <2x | ~20% (anchor+delta) |

*For versions close to anchor

### Scalability

```mermaid
graph LR
    subgraph "Concurrency Model"
        R1["Reader 1"]
        R2["Reader 2"]
        R3["Reader N"]
        W1["Writer"]
    end

    subgraph "Lock Strategy"
        DM["DashMap<br/>Lock-free reads<br/>Sharded writes"]
        CSR["CSR Index<br/>RwLock<br/>Rebuild on commit"]
    end

    R1 -->|No lock| DM
    R2 -->|No lock| DM
    R3 -->|No lock| DM
    W1 -->|Shard lock| DM
    W1 -->|Write lock| CSR
```

## Module Organization

```
gallifreydb/
├── src/
│   ├── lib.rs              # Public exports
│   ├── db.rs               # GallifreyDB orchestrator
│   │
│   ├── core/               # Core primitives
│   │   ├── id.rs           # NodeId, EdgeId, VersionId
│   │   ├── temporal.rs     # BiTemporalInterval, TimeRange
│   │   ├── property.rs     # PropertyValue, PropertyMap
│   │   ├── graph.rs        # Node, Edge
│   │   └── interning.rs    # InternedString, StringInterner
│   │
│   ├── storage/            # Persistence layer
│   │   ├── current.rs      # CurrentStorage
│   │   ├── historical.rs   # HistoricalStorage
│   │   ├── version.rs      # NodeVersion, EdgeVersion
│   │   ├── wal.rs          # WriteAheadLog
│   │   └── persistence.rs  # Checkpointing
│   │
│   ├── index/              # Query indexes
│   │   ├── current.rs      # CurrentIndexes (DashMap)
│   │   ├── temporal.rs     # TemporalIndexes (BTree)
│   │   └── adjacency.rs    # AdjacencyIndex (CSR)
│   │
│   ├── api/                # Transaction API
│   │   └── transaction/
│   │       ├── types.rs    # TxId, TxState
│   │       ├── visibility.rs # Snapshot Isolation
│   │       ├── read_tx.rs  # ReadTransaction
│   │       ├── write_tx.rs # WriteTransaction
│   │       └── write_buffer.rs
│   │
│   └── utils/
│       └── error.rs        # Error types
│
├── benches/                # Criterion benchmarks
├── docs/
│   ├── adr/               # Architecture Decision Records
│   └── architecture/      # This documentation
└── tests/                 # Integration tests
```

## Related Documentation

### Core Architecture
- [Storage Layer](storage-layer.md) - Detailed storage architecture
- [Transaction System](transaction-system.md) - MVCC and isolation
- [Index Layer](index-layer.md) - Index structures and algorithms
- [Data Model](data-model.md) - Core types and temporal model

### Performance & Scalability
- [Durability Modes](durability-modes.md) - Configurable WAL sync strategies (Sync/Batched/Async)
- [Scalability](scalability.md) - Tiered storage and horizontal sharding

### Decision Records
- [ADRs](../adr/README.md) - Architecture Decision Records
