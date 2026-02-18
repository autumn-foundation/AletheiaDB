# AletheiaDB Architecture

This document describes the core architecture principles, design patterns, and system design of AletheiaDB.

## Table of Contents

- [Architecture Principles](#architecture-principles)
- [System Context (C4 Model)](#system-context-c4-model)
- [Design Patterns](#design-patterns)
- [Hybrid Storage Architecture](#hybrid-storage-architecture)
- [Temporal Query Processing](#temporal-query-processing)
- [LLM Integration Patterns](#llm-integration-patterns)

## Architecture Principles

### 1. Performance First

**Current-State Queries Must Be Fast:**
- Current state stored separately from historical data (hybrid storage architecture)
- Zero abstraction overhead for non-temporal queries
- CSR (Compressed Sparse Row) adjacency representation for cache-friendly traversals
- **Target**: <1µs single-hop traversal, <100µs for 3-hop traversal

**Temporal Queries Must Be Efficient:**
- Anchor+delta compression reduces storage 5-6X
- Temporal B-Tree indexes for range queries
- Anchor-based reconstruction skips unnecessary versions
- **Target**: <10ms for point-in-time reconstruction

### 2. Storage Efficiency

**Compression Strategy:**
- Create anchor (full snapshot) every 10 versions (configurable)
- Delta encoding for incremental changes
- Copy-on-write with `Arc<T>` for property deduplication
- String interning for labels and property keys
- **Target**: <2X overhead vs non-temporal storage

**Immutable History:**
- Historical versions are immutable after creation
- Enables aggressive caching and compression
- Safe for concurrent access without locks

### 3. Correctness Guarantees

**Temporal Consistency:**
- Transaction time is monotonically increasing
- Valid time can be retroactive but must be consistent
- No temporal paradoxes (e.g., deleting an entity before it was created)

**ACID Properties:**
- **Atomicity**: WAL ensures atomic commits
- **Consistency**: Invariants checked on write
- **Isolation**: MVCC provides snapshot isolation
- **Durability**: WAL + fsync guarantees

## System Context (C4 Model)

```mermaid
C4Context
  title System Context diagram for AletheiaDB

  Person(developer, "Developer", "Uses the database for building apps")
  Person(agent, "AI Agent", "LLM (Claude/Cursor): Uses the database for reasoning")

  System(aletheiadb, "AletheiaDB", "Bi-temporal Graph Database")
  System_Ext(filesystem, "File System", "Stores WAL, Indexes, and Cold Data")

  Rel(developer, aletheiadb, "Reads/Writes", "Rust API / AQL")
  Rel(agent, aletheiadb, "Tool Execution", "MCP (stdio)")
  Rel(aletheiadb, filesystem, "Persists", "mmap / fsync")
```

## Design Patterns

### Hybrid Storage Architecture

```mermaid
classDiagram
    namespace Interfaces {
        class MCPServer {
            +serve_stdio()
            +handle_tool_call()
        }
    }
    namespace Core {
        class AletheiaDB
        class QueryEngine
        class TemporalPlanner
        class TraversalEngine
        class StorageEngine["StorageEngine (Trait)"]
    }
    namespace Storage {
        class CurrentStorage
        class HistoricalStorage
        class TieredStorage
        class RedbColdStorage
    }
    namespace Observability {
        class HoneycombClient {
            +send_batch()
        }
    }

    MCPServer --> QueryEngine : Uses
    QueryEngine --> AletheiaDB : Uses
    AletheiaDB --> StorageEngine : Uses (Trait Bound)
    CurrentStorage ..|> StorageEngine : Implements
    HistoricalStorage ..|> StorageEngine : Implements
    HistoricalStorage --> TieredStorage : Uses
    TieredStorage --> RedbColdStorage : Uses
```

**When to Use Each:**
- **Current**: All non-temporal queries, latest state access
- **Historical**: Time-travel, audit trails, temporal analysis, LLM reasoning

### Semantic Clustering ("The Cartographer")

```mermaid
classDiagram
    class Cartographer {
        +analyze(property, k)
        +reify(result)
    }
    class Region {
        +centroid: Vec<f32>
        +cluster_id: i64
    }
    class Node {
        +vector: Vec<f32>
    }

    Cartographer ..> Region : Creates (Reification)
    Node --> Region : LOCATED_IN
```

**Pattern:** Reifying implicit vector similarity into explicit graph structure to enable high-level topological analysis.

### Experimental Features

**Concept Algebra (Semantic Arithmetic)**

```mermaid
classDiagram
    namespace Experimental {
        class ConceptAlgebra {
            +add(a, b)
            +subtract(a, b)
            +analogy(a, b, c)
            +mean(nodes)
        }
    }
    class AletheiaDB
    ConceptAlgebra --> AletheiaDB : Uses (Vector Index)
```

**Sequence: Concept Analogy**

```mermaid
sequenceDiagram
    participant User
    participant CA as ConceptAlgebra
    participant DB as AletheiaDB

    User->>CA: analogy(king, man, woman)
    CA->>DB: get_vector(king)
    CA->>DB: get_vector(man)
    CA->>DB: get_vector(woman)
    CA->>CA: Compute: K - M + W
    CA->>DB: search_vectors(result)
    DB-->>CA: neighbors
    CA-->>User: Result (Queen)
```

**Temporal Resonance (Echo)**

```mermaid
classDiagram
    namespace Experimental {
        class EchoChamber {
            +find_echoes(target, candidates)
        }
        class Resonator {
            <<interface>>
            +resonate(history)
        }
        class ActivityDensityResonator
    }

    EchoChamber --> AletheiaDB : Uses (History)
    EchoChamber --> Resonator : Uses
    ActivityDensityResonator ..|> Resonator : Implements
```

**Sequence: Finding Echoes**

```mermaid
sequenceDiagram
    participant User
    participant Echo as EchoChamber
    participant Res as Resonator
    participant DB as AletheiaDB

    User->>Echo: find_echoes(target, candidates)
    Echo->>DB: get_node_history(target)
    Echo->>Res: resonate(target_history)
    Res-->>Echo: target_fingerprint

    loop Every Candidate
        Echo->>DB: get_node_history(candidate)
        Echo->>Res: resonate(candidate_history)
        Res-->>Echo: candidate_fingerprint
        Echo->>Echo: similarity(target, candidate)
    end

    Echo-->>User: Ranked Results
```

**Semantic Temperature (Thermos)**

```mermaid
sequenceDiagram
    participant User
    participant Thermos
    participant DB as AletheiaDB

    User->>Thermos: measure_node(node_id, window)
    Thermos->>DB: get_node_history(node_id)
    DB-->>Thermos: versions
    Thermos->>Thermos: filter_by_window(versions)
    loop Pairwise
        Thermos->>Thermos: dist = distance(v[i], v[i+1])
        Thermos->>Thermos: volatility += dist
    end
    Thermos->>Thermos: temp = volatility / duration
    Thermos-->>User: ThermalReading
```

**Semantic Spectroscopy (Prism)**

```mermaid
classDiagram
    namespace Experimental {
        class Prism {
            +add_axis(name, vector)
            +analyze(target)
            +analyze_evolution(target, range)
        }
        class Axis {
            +name: String
            +vector: Vec<f32>
        }
        class EvolutionPoint {
            +timestamp: Timestamp
            +scores: Map<String, f32>
        }
    }
    class AletheiaDB
    Prism --> Axis : Contains
    Prism --> AletheiaDB : Uses
    Prism ..> EvolutionPoint : Produces
```

**Counterfactual Graph Analysis (Hindsight)**

```mermaid
classDiagram
    namespace Experimental {
        class Hindsight {
            +add_node()
            +add_edge()
            +find_path()
        }
        class Scenario {
            +added_nodes: Map<NodeId, Node>
            +removed_nodes: Set<NodeId>
            +modified_nodes: Map<NodeId, Props>
        }
    }
    class AletheiaDB

    Hindsight --> Scenario : Owns
    Hindsight --> AletheiaDB : Wraps
```

**Wormhole (Latent Edge Detection)**

```mermaid
classDiagram
    namespace Experimental {
        class WormholeDetector {
            +find_wormholes(candidates, k, max_hops)
        }
        class Wormhole {
            +source: NodeId
            +target: NodeId
            +similarity: f32
            +structural_distance: Option<usize>
        }
    }
    class AletheiaDB

    WormholeDetector --> AletheiaDB : Uses
    WormholeDetector ..> Wormhole : Produces
```

**Sequence: Detecting Wormholes**

```mermaid
sequenceDiagram
    participant User
    participant Wormhole as WormholeDetector
    participant DB as AletheiaDB

    User->>Wormhole: find_wormholes(candidates, k, max_hops)
    loop Every Candidate
        Wormhole->>DB: find_similar(candidate, k)
        DB-->>Wormhole: semantic_neighbors
        loop Every Neighbor
            Wormhole->>DB: bfs_distance(candidate, neighbor, max_hops)
            DB-->>Wormhole: distance
            alt distance is None
                Wormhole->>Wormhole: Record Latent Edge
            end
        end
    end
    Wormhole-->>User: List<Wormhole>
```

**Sherlock (Temporal Pattern Matching)**

```mermaid
classDiagram
    namespace Experimental {
        class Sherlock {
            +investigate(node_id, mystery)
        }
        class Mystery {
            +clues: Vec<Clue>
            +time_window: Duration
        }
        class Clue {
            +key: String
            +value: Option<PropertyValue>
        }
        class Deduction {
            +node_id: NodeId
            +event_times: Vec<Timestamp>
        }
    }
    class AletheiaDB

    Sherlock --> AletheiaDB : Uses (History)
    Sherlock --> Mystery : Consumes
    Sherlock ..> Deduction : Produces
    Mystery --> Clue : Contains
```

**Sequence: Sherlock Investigation**

```mermaid
sequenceDiagram
    participant User
    participant Sherlock
    participant DB as AletheiaDB

    User->>Sherlock: investigate(node, mystery)
    Sherlock->>DB: get_node_history(node)
    DB-->>Sherlock: versions (unsorted)
    Sherlock->>Sherlock: sort_by_valid_time(versions)

    loop Find Start
        Sherlock->>Sherlock: match(clue[0])
        opt Match Found
            loop Next Clues
                Sherlock->>Sherlock: scan_forward()
                Sherlock->>Sherlock: check_window()
            end
        end
    end

    Sherlock-->>User: List<Deduction>
```

**Dreamer (Semantic Trajectory)**

```mermaid
classDiagram
    namespace Experimental {
        class Dreamer {
            +predict_future(node, prop, window, horizon)
        }
    }
    class AletheiaDB

    Dreamer --> AletheiaDB : Uses (History + Vector Index)
```

**Sequence: Dreamer Prediction**

```mermaid
sequenceDiagram
    participant User
    participant Dreamer
    participant DB as AletheiaDB

    User->>Dreamer: predict_future(node, horizon)
    Dreamer->>DB: get_node_history(node)
    Dreamer->>Dreamer: extract_vector_snapshots()
    Dreamer->>Dreamer: velocity = (end - start) / time
    Dreamer->>Dreamer: future = end + (velocity * horizon)
    Dreamer->>DB: search_vectors(future)
    DB-->>Dreamer: neighbors
    Dreamer-->>User: Result
```

**Chronos (Temporal Pathfinding)**

```mermaid
classDiagram
    namespace Experimental {
        class Chronos {
            +find_path_at_time(start, end, valid_time)
            +node_volatility(node, window)
            +path_stability(path, window)
        }
    }
    class AletheiaDB

    Chronos --> AletheiaDB : Uses
```

**Sequence: Snapshot Pathfinding**

```mermaid
sequenceDiagram
    participant User
    participant Chronos
    participant DB as AletheiaDB

    User->>Chronos: find_path_at_time(A, B, T)
    loop BFS
        Chronos->>DB: get_outgoing_edges_at_time(curr, T)
        DB-->>Chronos: edges
        Chronos->>Chronos: traverse
    end
    Chronos-->>User: Path
```

### Cognitive Architecture

The Cognitive Layer provides advanced reasoning services on top of the graph:

**Ariadne (Semantic Thread Weaver)**

```mermaid
sequenceDiagram
    participant User
    participant Ariadne
    participant DB as AletheiaDB

    User->>Ariadne: weave(start, goal, max_steps)
    loop A* Search
        Ariadne->>DB: get_outgoing_edges(current)
        DB-->>Ariadne: explicit_edges
        Ariadne->>DB: find_similar(current_vector)
        DB-->>Ariadne: semantic_candidates
        Ariadne->>Ariadne: Score candidates (Cost + Heuristic)
        Note right of Ariadne: Edges are cheap, Jumps are expensive
    end
    Ariadne-->>User: Narrative Thread
```

**Prophet (Link Prediction)**

```mermaid
classDiagram
    class Prophet {
        +predict_links(target, k)
    }
    class Scorer {
        +adamic_adar(neighbors_a, neighbors_b)
        +vector_similarity(vec_a, vec_b)
    }
    Prophet --> Scorer : Uses
    Prophet --> AletheiaDB : Queries Structure + Vectors
```

**Fishing (Associative Retrieval)**

```mermaid
sequenceDiagram
    participant User
    participant Rod as FishingRod
    participant DB as AletheiaDB

    User->>Rod: cast(bait_vector)
    Rod->>DB: search_vectors(bait_vector)
    DB-->>Rod: School (Initial Candidates)

    loop Spread Net (Graph Expansion)
        Rod->>DB: get_neighbors(school_member)
        DB-->>Rod: neighbors
        Rod->>Rod: Boost Score(neighbor)
    end

    Rod->>Rod: Apply Freshness Decay
    Rod-->>User: Catch (Ranked Results)
```

**Kaleidoscope (Semantic Layout)**

```mermaid
classDiagram
    class LayoutEngine {
        +add_node(id)
        +add_edge(a, b)
        +add_semantic_link(a, b, similarity)
        +step()
    }
    class Point {
        +x: f32
        +y: f32
    }
    LayoutEngine "1" *-- "*" Point : Positions
    Note right of LayoutEngine: Physics: Repulsion (Nodes) + Springs (Edges) + Gravity (Vectors)
```

**Semantic Navigator (Heuristic Pathfinding)**

```mermaid
sequenceDiagram
    participant User
    participant Nav as SemanticNavigator
    participant DB as AletheiaDB

    User->>Nav: find_path(start, end)
    Nav->>DB: get_vector(end)
    loop A* Search
        Nav->>DB: get_neighbors(current)
        loop Each Neighbor
            Nav->>DB: get_vector(neighbor)
            Nav->>Nav: h = 1.0 - cosine_similarity(neighbor, end)
            Nav->>Nav: Update PriorityQueue
        end
    end
    Nav-->>User: Path
```

**Sentinel (Semantic Firewall)**

```mermaid
sequenceDiagram
    participant App
    participant Sentinel
    participant Rule

    App->>Sentinel: validate(properties)
    loop Every Rule
        Sentinel->>Rule: validate(properties)
        alt VectorBanRule
            Rule->>Rule: check_similarity(prop_vector, banned_vectors)
        else NumericRangeRule
            Rule->>Rule: check_bounds(value)
        end
        Rule-->>Sentinel: Result
    end
    Sentinel-->>App: Result (Ok/Err)
```

**Sybil (Memetic Propagation)**

```mermaid
stateDiagram-v2
    [*] --> InitialState
    InitialState --> Iterating
    Iterating --> CalculateUpdates
    state CalculateUpdates {
        [*] --> GetNeighbors
        GetNeighbors --> AverageVectors
        AverageVectors --> BlendSelf
        BlendSelf --> NewState
    }
    NewState --> Iterating : Steps < Max
    Iterating --> FinalState : Steps >= Max
    FinalState --> [*]
```

**Temporal Diff (State Comparison)**

```mermaid
classDiagram
    class TemporalDiff {
        +compute_diff(t1, t2)
    }
    class DiffReport {
        +t1: Timestamp
        +t2: Timestamp
        +changes: Vec<EntityChange>
    }
    class PropertyDiff {
        +added: Vec<String>
        +removed: Vec<String>
        +modified: Map<Key, (Old, New)>
    }
    TemporalDiff ..> DiffReport : Produces
    DiffReport *-- PropertyDiff : Contains
```

**Narrative Generator (History to Text)**

```mermaid
classDiagram
    class NarrativeGenerator {
        +generate_node_narrative(id)
    }
    class NarrativeEvent {
        +timestamp: String
        +description: String
        +changes: Vec<String>
    }
    NarrativeGenerator ..> NarrativeEvent : Produces
    NarrativeGenerator --> AletheiaDB : Reads History
```

### Temporal Query Processing

**Query Types:**

1. **Time Point Query** (as of timestamp T): Lookup in temporal index → Find nearest anchor ≤ T → Apply deltas → Return state
2. **Time Range Query** (between T1 and T2): Range scan temporal index → Reconstruct each version → Stream results
3. **Knowledge Evolution Query** (for LLMs): Track how entity changed over time → Provenance and sources → Identify when understanding shifted

## Hybrid Storage Architecture

AletheiaDB's architecture separates current state from historical data for optimal performance:

### Current Storage Layer
- **Live Graph**: Active nodes and edges in CSR (Compressed Sparse Row) format
- **Hot Indexes**: Frequently accessed indexes in memory
- **Property Storage**: Current property values with Arc-based deduplication
- **Vector Indexes**: Current HNSW indexes for semantic search

**Optimizations:**
- Zero abstraction overhead for non-temporal queries
- Cache-friendly memory layout
- Lock-free concurrent access for reads

### Historical Storage Layer
- **Version Chains**: Linked list of entity versions over time
- **Anchor+Delta Compression**: Full snapshots every N versions (default: 10)
- **Temporal Indexes**: B-Tree indexes for time-based lookup
- **Vector Snapshots**: Historical HNSW indexes for temporal semantic search

**Optimizations:**
- Immutable history (safe for concurrent reads)
- Aggressive compression (5-6X reduction)
- LFU cache for reconstructed versions

### Storage Flow

```mermaid
sequenceDiagram
    participant User
    participant Core as Core (QueryEngine)
    participant Storage as Storage (Current/Historical)
    participant WAL

    Note over User, Core: Write Path
    User->>Core: Write Transaction
    Core->>Storage: Apply Changes (via Trait)
    Storage->>WAL: Append Entry
    WAL-->>Storage: LSN
    Storage-->>Core: Success
    Core-->>User: Commit ID

    rect rgb(240, 240, 240)
        Note right of Storage: Async Background Process
        Storage->>Storage: Background Flush
        Storage->>Storage: Compress & Index
    end

    Note over User, Core: Query Path
    User->>Core: Query (Latest)
    Core->>Storage: Get Node (Current)
    Storage-->>Core: Result
    Core-->>User: Result (Fast Path)

    User->>Core: Query (Time Travel)
    Core->>Storage: Get History
    Storage->>Storage: Reconstruct State
    Storage-->>Core: Versioned Node
    Core-->>User: Result (Temporal Path)
```

## Temporal Query Processing

### Point-in-Time Queries

**Algorithm:**
1. Query temporal index for timestamp T
2. Find nearest anchor ≤ T
3. Apply deltas from anchor to T
4. Return reconstructed state

**Complexity**: O(log N + D) where N = versions, D = deltas since anchor
**Target**: <10ms for typical workloads

### Time Range Queries

**Algorithm:**
1. Range scan temporal index [T1, T2]
2. For each version in range:
   - Reconstruct state (using nearest anchor)
   - Apply predicates/filters
   - Stream result
3. Return iterator over versions

**Complexity**: O(V × (log N + D)) where V = versions in range
**Optimization**: Skip versions that don't match predicates

### Hybrid Queries

Combine graph traversal + vector similarity + temporal queries:

**Example**: "Who did Alice know in 2023 that was similar to Bob?"

```rust
db.query()
    .as_of(timestamp_2023)     // Temporal filter
    .start(alice_id)           // Graph source
    .traverse("KNOWS")         // Graph traversal
    .rank_by_similarity(&bob_embedding, 10)  // Vector ranking
    .execute(&db)?
```

**Query Plan:**
1. Reconstruct Alice's state at 2023
2. Traverse KNOWS edges (using temporal index)
3. Reconstruct each neighbor at 2023
4. Load embeddings from temporal vector index
5. Rank by similarity to Bob's embedding
6. Return top 10

See [Hybrid Query Guide](guides/hybrid-query-guide.md) for complete API reference.

## LLM Integration Patterns

### Temporal Query API for LLMs

**Natural Language-Like Queries:**
```rust
db.as_of("2024-01-15T10:00:00Z").find_node("Person", "name" == "Alice").get_relationships("KNOWS")
db.between("2024-01-01", "2024-12-31").track_changes(node_id).with_provenance()
```

**Query Patterns LLMs Can Use:**
- "What did we know about X at time T?" → `db.as_of(T).get(X)`
- "How has Y changed?" → `db.history(Y).changes()`
- "When did we first record F?" → `db.first_occurrence(F)`
- "Show changes to E between T1 and T2" → `db.between(T1, T2).track_changes(E)`

### Integration Methods

1. **Direct Rust API** (for embedded use)
2. **MCP Server** (for Claude integration)
3. **REST/GraphQL API** (for general LLM tool use)
4. **Natural Query Language** (intuitive for LLMs to generate)

### Provenance Tracking

AletheiaDB tracks data lineage for LLM reasoning:

- **Source Attribution**: Which data source contributed this fact?
- **Temporal Provenance**: When was this fact recorded?
- **Version History**: How has this fact evolved?
- **Contradiction Detection**: Did this fact contradict earlier facts?

**API:**
```rust
let result = db.query()
    .start(node_id)
    .with_provenance()  // Include metadata
    .execute(&db)?;

for row in result {
    if let Some(prov) = row.provenance {
        println!("Source: {:?}", prov.source);
        println!("Valid time: {:?}", prov.valid_time);
        println!("Transaction time: {:?}", prov.tx_time);
    }
}
```

## Future Architecture Considerations

### Scalability

- **Sharding**: Horizontal scale by partitioning graph
- **Distributed Transactions**: Two-phase commit across shards
- **Replication**: High availability via replicas

### Query Language

- **Cypher Extensions**: Temporal extensions to Cypher query language
- **SQL:2011 Temporal Syntax**: `AS OF SYSTEM_TIME` support
- **Time-Aware Pattern Matching**: Temporal graph patterns

### Advanced Features

- **Temporal Graph Algorithms**: Shortest path over time, temporal PageRank
- **Streaming Temporal Queries**: Subscribe to changes in real-time
- **Incremental Materialized Views**: Maintain derived data efficiently
- **LLM-Assisted Query Generation**: Natural language → AletheiaDB queries

## References

- [AeonG: Efficient Temporal Graph Database](https://arxiv.org/abs/2304.12212)
- [XTDB Bi-temporality](https://v1-docs.xtdb.com/concepts/bitemporality/)
- [Temporal Database Concepts](https://en.wikipedia.org/wiki/Temporal_database)
- [Rust Performance Book](https://nnethercote.github.io/perf-book/)
