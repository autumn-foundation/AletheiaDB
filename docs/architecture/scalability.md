# Scalability Architecture

This document describes AletheiaDB's scalability architecture, including tiered storage for datasets larger than RAM and horizontal sharding for distributed deployments.

## Scalability Strategy Overview

AletheiaDB uses a two-phase scalability approach:

```mermaid
graph TB
    subgraph "Phase 1: Tiered Storage"
        TS[Single Machine]
        HOT[Hot Tier - RAM]
        COLD[Cold Tier - Disk]
        TS --> HOT
        TS --> COLD
    end

    subgraph "Phase 2: Sharding"
        COORD[Coordinator]
        S1[Shard 1]
        S2[Shard 2]
        S3[Shard N]
        COORD --> S1
        COORD --> S2
        COORD --> S3
    end

    TS -->|When current state exceeds RAM| COORD
```

| Phase | When to Use | Capacity |
|-------|-------------|----------|
| **Tiered Storage** | Historical data exceeds RAM | Unlimited history on disk |
| **Sharding** | Current state exceeds single-machine RAM | Unlimited horizontal scale |

---

## Phase 1: Tiered Storage

### Architecture

```mermaid
graph TB
    subgraph "Query Layer"
        QE[Query Engine]
    end

    subgraph "Hot Tier (RAM)"
        CURRENT[Current Storage]
        CSR[CSR Indexes]
        NODES[DashMap Nodes]
        EDGES[DashMap Edges]

        CURRENT --> CSR
        CURRENT --> NODES
        CURRENT --> EDGES
    end

    subgraph "Warm Tier (RAM Cache)"
        CACHE[LRU Cache]
        RECENT[Recent Versions]
    end

    subgraph "Cold Tier (Disk)"
        ROCKS[RocksDB]
        COMPRESSED[Compressed Versions]
    end

    subgraph "Migration Service"
        MIGRATOR[Background Migrator]
        POLICY[Migration Policy]
    end

    QE --> CURRENT
    QE --> CACHE
    QE --> ROCKS

    CURRENT -.->|age threshold| MIGRATOR
    MIGRATOR --> ROCKS
    ROCKS --> CACHE
```

### Query Routing

```mermaid
flowchart TD
    A[get_version id] --> B{In Hot Tier?}
    B -->|Yes| C[Return from RAM]
    C --> DONE[Done - 22ns]

    B -->|No| D{In Warm Cache?}
    D -->|Yes| E[Return from Cache]
    E --> DONE2[Done - ~500ns]

    D -->|No| F[Fetch from Cold Tier]
    F --> G[Decompress]
    G --> H[Add to Cache]
    H --> I[Return]
    I --> DONE3[Done - ~1ms]

    style C fill:#90EE90
    style E fill:#87CEEB
    style I fill:#FFB6C1
```

### Version Migration Flow

```mermaid
sequenceDiagram
    participant Hot as Hot Tier
    participant Migrator as Migration Service
    participant Cold as Cold Tier (RocksDB)

    loop Every check_interval
        Migrator->>Hot: scan for candidates
        Note over Migrator: age > threshold OR memory > limit

        Migrator->>Hot: get candidate versions
        Hot-->>Migrator: versions[]

        Migrator->>Migrator: compress(versions)
        Migrator->>Cold: batch_write(compressed)
        Cold-->>Migrator: confirmed

        Migrator->>Hot: remove(version_ids)
        Migrator->>Migrator: update version chain pointers
    end
```

### Storage Tiers Detail

```mermaid
graph LR
    subgraph "Hot Tier"
        H1[Current Nodes]
        H2[Current Edges]
        H3[CSR Indexes]
        H4[Recent Versions]
    end

    subgraph "Warm Tier"
        W1[LRU Cache]
        W2[Frequently Accessed History]
    end

    subgraph "Cold Tier"
        C1[RocksDB]
        C2[Zstd Compressed]
        C3[Years of History]
    end

    H4 -->|age > 7 days| C1
    C1 -->|cache miss| W1

    style H1 fill:#90EE90
    style H2 fill:#90EE90
    style H3 fill:#90EE90
    style H4 fill:#90EE90
    style W1 fill:#87CEEB
    style W2 fill:#87CEEB
    style C1 fill:#FFB6C1
    style C2 fill:#FFB6C1
    style C3 fill:#FFB6C1
```

### Performance by Tier

| Tier | Storage | Capacity | Read Latency | Use Case |
|------|---------|----------|--------------|----------|
| **Hot** | RAM | ~256GB | 22-70ns | Current state, live queries |
| **Warm** | RAM | ~1-10GB | 100ns-1µs | Repeated time-travel |
| **Cold** | SSD | Unlimited | 100µs-1ms | Deep history, audits |

---

## Phase 2: Horizontal Sharding

### Sharding Architecture

```mermaid
graph TB
    subgraph "Client Layer"
        CLIENT[Application]
    end

    subgraph "Coordination Layer"
        COORD[Shard Coordinator]
        ROUTER[Query Router]
        TXMGR[Transaction Manager]
    end

    subgraph "Shard 0: People"
        S0_HOT[Hot Tier]
        S0_COLD[Cold Tier]
        S0_WAL[WAL]
    end

    subgraph "Shard 1: Places"
        S1_HOT[Hot Tier]
        S1_COLD[Cold Tier]
        S1_WAL[WAL]
    end

    subgraph "Shard 2: Events"
        S2_HOT[Hot Tier]
        S2_COLD[Cold Tier]
        S2_WAL[WAL]
    end

    CLIENT --> COORD
    COORD --> ROUTER
    COORD --> TXMGR

    ROUTER --> S0_HOT
    ROUTER --> S1_HOT
    ROUTER --> S2_HOT

    TXMGR -.->|2PC| S0_WAL
    TXMGR -.->|2PC| S1_WAL
    TXMGR -.->|2PC| S2_WAL
```

### Domain-Based Partitioning

```mermaid
graph LR
    subgraph "Shard Assignment"
        PERSON[Person, User, Account] --> S0[Shard 0]
        PLACE[Place, Location, Address] --> S1[Shard 1]
        EVENT[Event, Transaction, Activity] --> S2[Shard 2]
    end

    subgraph "Edge Replication"
        S0 <-->|VISITED| S1
        S0 <-->|ATTENDED| S2
        S1 <-->|HOSTED| S2
    end
```

### Query Routing Decision Tree

```mermaid
flowchart TD
    A[Incoming Query] --> B{Single node lookup?}
    B -->|Yes| C[Route by label]
    C --> D[Execute on single shard]

    B -->|No| E{Single-hop traversal?}
    E -->|Yes| F{Cross-shard edge?}
    F -->|No| D
    F -->|Yes| G[Use replicated edge locally]

    E -->|No| H[Multi-hop traversal]
    H --> I[Build execution plan]
    I --> J[Scatter to relevant shards]
    J --> K[Gather and merge results]
```

### Edge Replication Detail

```mermaid
graph TB
    subgraph "Shard 0 (People)"
        P1[Person: Alice]
        E1[VISITED → Place:Paris@S1]
    end

    subgraph "Shard 1 (Places)"
        PL1[Place: Paris]
        E2[VISITED ← Person:Alice@S0]
    end

    P1 --> E1
    E2 --> PL1

    E1 -.->|replicated| E2

    style E1 fill:#FFE4B5
    style E2 fill:#FFE4B5
```

**Benefits:**
- Outgoing traversal from Alice: local on Shard 0
- Incoming traversal to Paris: local on Shard 1
- Single-hop traversal never crosses network

### Distributed Transaction (2PC)

```mermaid
sequenceDiagram
    participant C as Coordinator
    participant S0 as Shard 0
    participant S1 as Shard 1

    Note over C: Create edge: Alice (S0) → Paris (S1)

    C->>S0: PREPARE (create edge)
    C->>S1: PREPARE (create replica)

    S0-->>C: PREPARED
    S1-->>C: PREPARED

    alt All Prepared
        C->>S0: COMMIT
        C->>S1: COMMIT
        S0-->>C: COMMITTED
        S1-->>C: COMMITTED
    else Any Failed
        C->>S0: ABORT
        C->>S1: ABORT
    end
```

### Rebalancing Flow

```mermaid
stateDiagram-v2
    [*] --> Monitoring

    Monitoring --> Planning: imbalance detected
    Planning --> DualWrite: plan approved

    DualWrite --> Migrating: writes go to both
    Migrating --> Cutover: data copied

    Cutover --> Cleanup: routing updated
    Cleanup --> Monitoring: old data removed

    note right of DualWrite
        No downtime
        Writes to both old and new
    end note

    note right of Cutover
        Atomic routing update
        Reads switch instantly
    end note
```

### Shard Topology Example

```mermaid
graph TB
    subgraph "Production Cluster"
        LB[Load Balancer]

        subgraph "Coordinators (HA)"
            C1[Coordinator 1]
            C2[Coordinator 2]
            C3[Coordinator 3]
        end

        subgraph "Data Shards"
            subgraph "Shard 0"
                S0P[Primary]
                S0R[Replica]
            end

            subgraph "Shard 1"
                S1P[Primary]
                S1R[Replica]
            end

            subgraph "Shard 2"
                S2P[Primary]
                S2R[Replica]
            end
        end
    end

    LB --> C1
    LB --> C2
    LB --> C3

    C1 --> S0P
    C1 --> S1P
    C1 --> S2P

    S0P -.->|replicate| S0R
    S1P -.->|replicate| S1R
    S2P -.->|replicate| S2R
```

---

## Capacity Planning

### Single Machine (Tiered Storage)

| RAM | Current Nodes | Historical Depth | SSD Required |
|-----|---------------|------------------|--------------|
| 64GB | ~300M | Unlimited | 500GB-2TB |
| 128GB | ~600M | Unlimited | 1-4TB |
| 256GB | ~1.2B | Unlimited | 2-8TB |

### Sharded Cluster

| Nodes | RAM/Node | Total Current Nodes | Throughput |
|-------|----------|---------------------|------------|
| 3 | 256GB | ~3.6B | ~300K reads/sec |
| 10 | 256GB | ~12B | ~1M reads/sec |
| 50 | 256GB | ~60B | ~5M reads/sec |

---

## Related Documentation

- [ADR-0013: Tiered Storage Architecture](../adr/0013-tiered-storage-architecture.md)
- [ADR-0014: Graph Sharding Strategy](../adr/0014-graph-sharding-strategy.md)
- [ADR-0001: Hybrid Storage Architecture](../adr/0001-hybrid-storage-architecture.md)
- [Storage Layer](./storage-layer.md)
