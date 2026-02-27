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
    namespace DB {
        class AletheiaDB
    }
    namespace Core {
        class QueryEngine
        class TemporalPlanner
        class TraversalEngine
        class StorageEngine
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

    MCPServer --> DB : Uses
    DB --> Core : "Uses"
    DB --> Storage : "Owns (Arc/RwLock)"
    Core ..> StorageEngine : Defines
    Storage ..|> StorageEngine : Implements
    %% Removed the circular dependency arrow
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

**Ariadne (Semantic Thread Weaver)**

```mermaid
sequenceDiagram
    participant User
    participant Ariadne
    participant DB as AletheiaDB

    User->>Ariadne: weave(start, goal)
    loop A* Search
        Ariadne->>DB: get_outgoing_edges(current)
        Ariadne->>DB: find_similar(current, k)
        Ariadne->>Ariadne: score = cost + heuristic
    end
    Ariadne-->>User: Path (Thread)
```

**Prophet (Link Prediction)**

```mermaid
classDiagram
    class Prophet {
        +predict_links(target, k)
    }
    class Scorer {
        +adamic_adar()
        +vector_similarity()
    }
    Prophet --> Scorer : Uses
    Scorer --> AletheiaDB : Queries
```

**Fishing (Associative Retrieval)**

```mermaid
sequenceDiagram
    participant User
    participant Rod as FishingRod
    participant DB as AletheiaDB

    User->>Rod: cast(bait)
    Rod->>DB: find_similar(bait)
    DB-->>Rod: school (vectors)
    loop Spread Net
        Rod->>DB: get_neighbors(fish)
        DB-->>Rod: catch (neighbors)
    end
    Rod-->>User: Result (Catch)
```

**Kaleidoscope (Force-Directed Layout)**

```mermaid
classDiagram
    class LayoutEngine {
        +run()
        +step()
    }
    class Force {
        +repulsion()
        +attraction()
        +gravity(semantic)
    }
    LayoutEngine --> Force : Applies
```

**Semantic Navigator (A* Pathfinder)**

```mermaid
sequenceDiagram
    participant User
    participant Navigator
    participant DB as AletheiaDB

    User->>Navigator: find_path(start, end)
    loop A*
        Navigator->>DB: get_neighbors(current)
        Navigator->>DB: vector_similarity(neighbor, end)
        Navigator->>Navigator: heuristic = 1.0 - similarity
    end
    Navigator-->>User: Semantic Path
```

**Sentinel (Semantic Firewall)**

```mermaid
sequenceDiagram
    participant User
    participant Sentinel
    participant Rule

    User->>Sentinel: validate(props)
    loop Every Rule
        Sentinel->>Rule: check(props)
        alt Violation
            Rule-->>Sentinel: Error
            Sentinel-->>User: Blocked
        end
    end
    Sentinel-->>User: Allowed
```

**Sybil (Memetic Propagation)**

```mermaid
sequenceDiagram
    participant User
    participant Sybil
    participant Model

    User->>Sybil: simulate(prop, steps)
    loop Steps
        Sybil->>Sybil: get_active_nodes()
        loop Every Node
            Sybil->>Model: next_state(current, neighbors)
            Model-->>Sybil: new_state
        end
        Sybil->>Sybil: update_state()
    end
    Sybil-->>User: Final State
```

**Temporal Diff (State Comparator)**

```mermaid
classDiagram
    class TemporalDiff {
        +compute_diff(t1, t2)
    }
    class DiffReport {
        +changes: Vec<Change>
    }
    TemporalDiff ..> DiffReport : Produces
    TemporalDiff --> AletheiaDB : Queries (History)
```

**Narrative Generator (The Scribe)**

```mermaid
classDiagram
    namespace Experimental {
        class NarrativeGenerator {
            +generate_node_narrative(node_id)
        }
        class GraphContextBuilder {
            +with_history_limit(limit)
            +with_neighbor_limit(limit)
            +build()
        }
    }
    GraphContextBuilder --> NarrativeGenerator : Uses
    GraphContextBuilder --> AletheiaDB : Uses
    NarrativeGenerator --> AletheiaDB : Uses
```

```mermaid
sequenceDiagram
    participant User
    participant Scribe as NarrativeGenerator
    participant DB as AletheiaDB

    User->>Scribe: generate_narrative(node_id)
    Scribe->>DB: get_node_history(node_id)
    DB-->>Scribe: versions
    loop Every Version
        Scribe->>Scribe: compute_diff(prev, curr)
        Scribe->>Scribe: format_natural_language()
    end
    Scribe-->>User: List<NarrativeEvent>
```

**Semantic Memory Consolidation (Mnemosyne)**

```mermaid
classDiagram
    namespace Experimental {
        class Mnemosyne {
            +consolidate_memory(node_id, prop, threshold)
        }
        class MemoryFrame {
            +timestamp: Timestamp
            +version_id: VersionId
            +reason: String
            +properties: PropertyMap
        }
    }
    class AletheiaDB

    Mnemosyne --> AletheiaDB : Uses
    Mnemosyne ..> MemoryFrame : Produces
```

```mermaid
sequenceDiagram
    participant User
    participant Mnemosyne
    participant DB as AletheiaDB

    User->>Mnemosyne: consolidate_memory(node, thresh)
    Mnemosyne->>DB: get_node_history(node)
    DB-->>Mnemosyne: versions
    loop Every Version
        Mnemosyne->>Mnemosyne: dist = vector_distance(last_kept, current)
        alt dist > thresh OR prop_changed
            Mnemosyne->>Mnemosyne: keep_frame(current)
            Mnemosyne->>Mnemosyne: update_last_kept(current)
        end
    end
    Mnemosyne-->>User: List<MemoryFrame>
```

**Context-Aware Faceted Search (Chameleon)**

```mermaid
classDiagram
    namespace Experimental {
        class Chameleon {
            +analyze_context(node_id, prop, k)
            +facet_search(node_id, aspect_idx, limit)
        }
        class Aspect {
            +centroid: Vec<f32>
            +weight: f32
            +exemplars: Vec<NodeId>
        }
    }
    class AletheiaDB

    Chameleon --> AletheiaDB : Uses
    Chameleon ..> Aspect : Produces
```

```mermaid
sequenceDiagram
    participant User
    participant Chameleon
    participant DB as AletheiaDB

    User->>Chameleon: analyze_context(node, k)
    Chameleon->>DB: get_neighbors(node)
    DB-->>Chameleon: neighbors
    Chameleon->>DB: get_vectors(neighbors)
    Chameleon->>Chameleon: cluster(vectors, k) (MiniKMeans)
    Chameleon-->>User: List<Aspect>

    User->>Chameleon: facet_search(node, aspect_idx)
    Chameleon->>DB: search_vectors(aspect.centroid)
    DB-->>Chameleon: results
    Chameleon-->>User: List<NodeId>
```

**Hybrid Entity Synthesis (Chimera)**

```mermaid
classDiagram
    namespace Experimental {
        class ChimeraEngine {
            +synthesize(node_a, node_b, config)
        }
        class SynthesisConfig {
            +alpha: f32
            +strategies: Map
        }
    }
    class AletheiaDB

    ChimeraEngine --> AletheiaDB : Uses
    ChimeraEngine ..> SynthesisConfig : Consumes
```

```mermaid
sequenceDiagram
    participant User
    participant Chimera
    participant DB as AletheiaDB

    User->>Chimera: synthesize(A, B, config)
    Chimera->>DB: get_node(A)
    Chimera->>DB: get_node(B)
    loop Every Property
        Chimera->>Chimera: merge_value(val_A, val_B, strategy)
    end
    Chimera->>DB: create_node(new_props)
    DB-->>Chimera: new_id
    loop Every Edge
        Chimera->>DB: duplicate_edge(original, new_id)
    end
    Chimera-->>User: new_id
```

### Semantic Physics & Pattern Matching

**Semantic Stress (Dissonance)**

```mermaid
classDiagram
    namespace Experimental {
        class DissonanceEngine {
            +calculate_dissonance(node, prop)
        }
    }
    DissonanceEngine --> AletheiaDB : Uses
```

**Semantic Subgraph Matching (Gestalt)**

```mermaid
classDiagram
    namespace Experimental {
        class GestaltMatcher {
            +find_matches(pattern)
        }
        class Pattern {
            +nodes: Vec<PatternNode>
            +edges: Vec<PatternEdge>
        }
        class Match {
            +nodes: Map
            +score: f32
        }
    }
    GestaltMatcher --> Pattern : Consumes
    GestaltMatcher ..> Match : Produces
    GestaltMatcher --> AletheiaDB : Uses
```

**Sequence: Gestalt Matching**

```mermaid
sequenceDiagram
    participant User
    participant Gestalt as GestaltMatcher
    participant DB as AletheiaDB

    User->>Gestalt: find_matches(pattern)
    Gestalt->>Gestalt: select_anchor()
    Gestalt->>DB: search_vectors(anchor_vec)
    DB-->>Gestalt: candidates
    loop Every Candidate
        Gestalt->>Gestalt: backtrack(match)
        alt Match Complete
            Gestalt-->>User: Match Found
        end
    end
```

**Semantic Influence (Gravity)**

```mermaid
classDiagram
    namespace Experimental {
        class GravityWell {
            +analyze_orbit(center, prop, window)
        }
        class OrbitMetrics {
            +velocity: f32
            +start_dist: f32
            +end_dist: f32
        }
    }
    GravityWell --> AletheiaDB : Uses
    GravityWell ..> OrbitMetrics : Produces
```

**Semantic Spreading Activation (Telepathy)**

```mermaid
classDiagram
    namespace Experimental {
        class TelepathyEngine {
            +propagate(seeds)
        }
        class TelepathyConfig {
            +decay: f32
            +threshold: f32
        }
    }
    TelepathyEngine --> AletheiaDB : Uses
    TelepathyEngine --> TelepathyConfig : Uses
```

**Sequence: Spreading Activation**

```mermaid
sequenceDiagram
    participant User
    participant Telepathy
    participant DB as AletheiaDB

    User->>Telepathy: propagate(seeds)
    loop Max Steps
        Telepathy->>DB: get_outgoing_edges(active_nodes)
        DB-->>Telepathy: edges
        loop Every Edge
            Telepathy->>DB: get_vector(target)
            Telepathy->>Telepathy: weight = similarity(source, target)
            Telepathy->>Telepathy: signal = source_strength * weight * decay
            Telepathy->>Telepathy: accumulate(target, signal)
        end
    end
    Telepathy-->>User: Activations
```

**Semantic Graph Alignment (Metaphor)**

```mermaid
classDiagram
    namespace Experimental {
        class Metaphor {
            +align(source, target)
        }
        class Alignment {
            +mappings: Vec<Mapping>
            +score: f32
        }
        class Mapping {
            +source: NodeId
            +target: NodeId
        }
    }
    Metaphor --> AletheiaDB : Uses
    Metaphor ..> Alignment : Produces
    Alignment --> Mapping : Contains
```

**Sequence: Subgraph Alignment**

```mermaid
sequenceDiagram
    participant User
    participant Metaphor
    participant DB as AletheiaDB

    User->>Metaphor: align(source_nodes, target_nodes)
    Metaphor->>DB: fetch_vectors_and_topology()
    Metaphor->>Metaphor: compute_similarity_matrix()

    loop Until All Mapped
        Metaphor->>Metaphor: find_best_pair()
        Metaphor->>Metaphor: record_mapping()
        Metaphor->>Metaphor: boost_neighbors_score()
    end

    Metaphor-->>User: Alignment
```

**Semantic Entity Resolution (Highlander)**

```mermaid
classDiagram
    namespace Experimental {
        class HighlanderDetector {
            +find_duplicates(target, threshold)
        }
        class EntityMerger {
            +merge(survivor, victim)
        }
    }
    class AletheiaDB
    HighlanderDetector --> AletheiaDB : Uses
    EntityMerger --> AletheiaDB : Mutates
```

**Sequence: Entity Merge**

```mermaid
sequenceDiagram
    participant User
    participant Merger as EntityMerger
    participant DB as AletheiaDB

    User->>Merger: merge(survivor, victim)
    Merger->>DB: get_edges(victim)
    loop Move Edges
        Merger->>DB: create_edge(survivor, target)
        Merger->>DB: delete_edge(victim, target)
    end
    Merger->>DB: get_props(victim)
    loop Merge Props
        Merger->>DB: update_node(survivor, missing_prop)
    end
    Merger->>DB: delete_node(victim)
    Merger-->>User: Success
```

**Semantic Bridge Detection (Janus)**

```mermaid
classDiagram
    namespace Experimental {
        class JanusDetector {
            +analyze_node(node_id, property)
        }
        class BridgeScore {
            +total_score: f32
            +inter_cluster_distance: f32
            +intra_cluster_spread: f32
            +is_bridge() bool
        }
    }
    class AletheiaDB
    JanusDetector --> AletheiaDB : Uses
    JanusDetector ..> BridgeScore : Produces
```

**Semantic Ideation (Muse)**

```mermaid
classDiagram
    namespace Experimental {
        class Muse {
            +inspire(seeds)
        }
        class Inspiration {
            +centroid: Vec<f32>
            +novelty_score: f32
            +coherence_score: f32
        }
    }
    class AletheiaDB
    Muse --> AletheiaDB : Uses
    Muse ..> Inspiration : Produces
```

**Sequence: Semantic Ideation**

```mermaid
sequenceDiagram
    participant User
    participant Muse
    participant DB as AletheiaDB

    User->>Muse: inspire(seeds)
    Muse->>DB: get_vectors(seeds)
    Muse->>Muse: compute_centroid()
    Muse->>DB: search_vectors(centroid)
    DB-->>Muse: nearest_neighbors
    Muse->>Muse: novelty = 1.0 - max_sim
    Muse->>Muse: coherence = avg_sim_to_seeds
    Muse-->>User: Inspiration
```

**Semantic Memory Consolidation (Mnemosyne)**

```mermaid
classDiagram
    namespace Experimental {
        class Mnemosyne {
            +consolidate_memory(node_id, vec_prop, threshold)
        }
        class MemoryFrame {
            +timestamp: i64
            +version_id: VersionId
            +reason: String
            +properties: PropertyMap
        }
    }
    class AletheiaDB
    Mnemosyne --> AletheiaDB : Uses
    Mnemosyne ..> MemoryFrame : Produces
```

**Sequence: Memory Consolidation**

```mermaid
sequenceDiagram
    participant User
    participant Mnemosyne
    participant DB as AletheiaDB

    User->>Mnemosyne: consolidate_memory(node, vec_prop, threshold)
    Mnemosyne->>DB: get_node_history(node)
    DB-->>Mnemosyne: versions
    loop Every Version
        Mnemosyne->>Mnemosyne: dist = distance(prev_kept, curr)
        alt dist > threshold OR props_changed
            Mnemosyne->>Mnemosyne: keep(curr)
            Mnemosyne->>Mnemosyne: prev_kept = curr
        else
            Mnemosyne->>Mnemosyne: discard(curr)
        end
    end
    Mnemosyne-->>User: List<MemoryFrame>
```

**Semantic Graph Transformation (Alchemy)**

```mermaid
classDiagram
    namespace Experimental {
        class Alchemist {
            +crystallize_wormholes(candidates, threshold, hops, label)
            +fuse_synonyms(candidates, threshold)
        }
        class WormholeDetector {
            +find_wormholes(candidates, k, max_hops)
        }
        class Wormhole {
            +source: NodeId
            +target: NodeId
            +similarity: f32
        }
    }
    class AletheiaDB

    Alchemist --> WormholeDetector : Uses
    Alchemist --> AletheiaDB : Mutates
    WormholeDetector --> AletheiaDB : Queries
    WormholeDetector ..> Wormhole : Produces
```

**Sequence: Crystallize Wormholes**

```mermaid
sequenceDiagram
    participant User
    participant Alchemist
    participant Detector as WormholeDetector
    participant DB as AletheiaDB

    User->>Alchemist: crystallize_wormholes()
    Alchemist->>Detector: find_wormholes()
    Detector->>DB: find_similar()
    DB-->>Detector: semantic_neighbors
    loop Every Neighbor
        Detector->>Detector: bfs_distance()
        alt No Path Found
            Detector->>Detector: Record Wormhole
        end
    end
    Detector-->>Alchemist: List<Wormhole>

    loop Every Wormhole
        alt similarity > threshold
            Alchemist->>DB: create_edge(source, target)
        end
    end
```

**Sequence: Fuse Synonyms (Semantic Fusion)**

```mermaid
sequenceDiagram
    participant User
    participant Alchemist
    participant DB as AletheiaDB

    User->>Alchemist: fuse_synonyms(candidates)
    loop Find Pairs
        Alchemist->>DB: find_similar(candidate)
        DB-->>Alchemist: neighbors
        Alchemist->>Alchemist: Identify {Survivor, Victim}
    end

    Alchemist->>DB: Begin Transaction
    loop Every Pair
        Alchemist->>DB: get_edges(victim)
        loop Move Edges
            Alchemist->>DB: create_edge(survivor, target)
        end
        Alchemist->>DB: delete_node_cascade(victim)
    end
    DB-->>Alchemist: Commit
```

### Cognitive Dynamics

**Ripple (Semantic Causality)**

```mermaid
sequenceDiagram
    participant User
    participant Ripple as RippleDetector
    participant DB as AletheiaDB

    User->>Ripple: detect_causality(source, target)
    Ripple->>DB: get_node_history(source)
    Ripple->>DB: get_node_history(target)
    Ripple->>Ripple: compute_flux(source_history)
    Ripple->>Ripple: compute_flux(target_history)
    Ripple->>Ripple: cross_correlate(source_flux, target_flux)
    Ripple-->>User: RippleEffect(lag, correlation)
```

**Oracle (Probabilistic Reasoning)**

```mermaid
classDiagram
    namespace Experimental {
        class Oracle {
            +personalized_page_rank(seed, alpha, walks)
            +reachability_probability(start, end, sims)
        }
    }
    class AletheiaDB

    Oracle --> AletheiaDB : Uses (Monte Carlo Simulation)
```

```mermaid
flowchart TD
    Start([Start Walk]) --> CheckTerm{Termination?}
    CheckTerm -- Yes --> RecordVisit[Record Visit]
    CheckTerm -- No --> PickEdge[Pick Random Neighbor]
    PickEdge --> Move[Move to Neighbor]
    Move --> CheckTerm
```

**Kairos (Semantic Event Detection)**

```mermaid
sequenceDiagram
    participant User
    participant Kairos
    participant DB as AletheiaDB

    User->>Kairos: extract_timeline(node, thresh)
    Kairos->>DB: get_node_history(node)
    loop Every Version
        Kairos->>Kairos: Check Vector Drift
        alt drift > threshold OR structural_change
            Kairos->>Kairos: Record TimelineEvent
            Kairos->>Kairos: Update Baseline
        end
    end
    Kairos-->>User: Timeline
```

**Synapse (Adaptive Learning)**

```mermaid
stateDiagram-v2
    [*] --> Unweighted
    Unweighted --> Reinforced : Observe(Traversal)
    Reinforced --> Reinforced : Observe(Traversal)
    Reinforced --> Decayed : Decay(Time)
    Decayed --> Reinforced : Observe(Traversal)
    Decayed --> Unweighted : Decay(Time)
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

## Tiered Storage Architecture

AletheiaDB employs a three-tier storage architecture to support datasets larger than available RAM while maintaining sub-microsecond latency for current-state queries.

### Architecture Overview

```mermaid
flowchart TB
    subgraph QueryEngine["Query Engine"]
        CQ["Current Queries"]
        TQ["Time-Travel Queries"]
    end

    subgraph Tiers["Storage Tiers"]
        subgraph Hot["HOT TIER<br/>(Always RAM)"]
            HN["Current nodes"]
            HE["Current edges"]
            HI["CSR indexes"]
            HL["22ns lookup"]
        end

        subgraph Warm["WARM TIER<br/>(RAM Cache)"]
            WH["Recent history"]
            WC["LRU cache"]
            WL["<1μs lookup"]
        end

        subgraph Cold["COLD TIER<br/>(Disk - Redb)"]
            CV["Old versions"]
            CC["Compressed"]
            CR["Redb B-Trees"]
            CL["<1ms lookup"]
        end
    end

    CQ --> Hot
    TQ --> Warm
    TQ --> Cold

    Hot -.->|"Migration Service<br/>(Background)"| Cold
    Cold -->|"Cache Miss"| Warm
```

### Storage Tiers

| Tier | Storage | Latency | Content |
|------|---------|---------|---------|
| **Hot** | RAM (DashMap) | 22-70ns | Current state, live indexes |
| **Warm** | RAM (LRU Cache) | 100ns-1µs | Recently accessed history |
| **Cold** | Disk (Redb) | 100µs-1ms | Compressed historical versions |

### Data Flow

```mermaid
sequenceDiagram
    participant Client
    participant HistoricalStorage
    participant HotTier as Hot Tier (RAM)
    participant WarmCache as Warm Cache (LRU)
    participant ColdTier as Cold Tier (Redb)

    Client->>HistoricalStorage: get_version(id)
    HistoricalStorage->>HotTier: lookup(id)

    alt Found in Hot
        HotTier-->>HistoricalStorage: version
        HistoricalStorage-->>Client: Ok(version)
    else Not in Hot
        HotTier-->>HistoricalStorage: None
        HistoricalStorage->>WarmCache: get(id)

        alt Found in Warm
            WarmCache-->>HistoricalStorage: cached_version
            HistoricalStorage-->>Client: Ok(version)
        else Not in Warm
            WarmCache-->>HistoricalStorage: None
            HistoricalStorage->>ColdTier: get(id)
            ColdTier-->>HistoricalStorage: version
            HistoricalStorage->>WarmCache: insert(id, version)
            HistoricalStorage-->>Client: Ok(version)
        end
    end
```

## Distributed Architecture (Sharding)

To scale beyond single-machine limits, AletheiaDB implements domain-based partitioning with edge replication.

### Sharding Overview

```mermaid
flowchart TB
    subgraph Coordinator["Shard Coordinator"]
        QR[Query Router]
        TC[Transaction Coordinator]
        SD[Shard Discovery]
        RM[Rebalance Manager]
    end

    subgraph Shard0["Shard 0 - People"]
        N0[Nodes]
        E0[Edges]
        H0[History]
        W0[WAL]
    end

    subgraph Shard1["Shard 1 - Places"]
        N1[Nodes]
        E1[Edges]
        H1[History]
        W1[WAL]
    end

    subgraph Shard2["Shard 2 - Events"]
        N2[Nodes]
        E2[Edges]
        H2[History]
        W2[WAL]
    end

    Client --> Coordinator
    QR --> Shard0
    QR --> Shard1
    QR --> Shard2
    TC --> Shard0
    TC --> Shard1
    TC --> Shard2

    Shard0 <-.->|"Cross-shard edges"| Shard1
    Shard1 <-.->|"Cross-shard edges"| Shard2
    Shard0 <-.->|"Cross-shard edges"| Shard2
```

### Core Concepts

1.  **Domain-Based Partitioning**: Nodes are partitioned by label (e.g., "Person" on Shard 0, "Place" on Shard 1). This ensures related data stays local.
2.  **Edge Replication**: Edges crossing shard boundaries are stored on **both** source and target shards, enabling fast single-hop traversal without network overhead.
3.  **Circuit Breakers**: Network communication is guarded by circuit breakers to prevent cascading failures.

### Distributed Transactions (Two-Phase Commit)

Writes spanning multiple shards are coordinated using a Two-Phase Commit (2PC) protocol with a persistent commit log for crash recovery.

```mermaid
sequenceDiagram
    participant C as Coordinator
    participant CL as Commit Log
    participant SA as Shard A
    participant SB as Shard B

    Note over C: Begin Transaction
    C->>CL: Log PREPARING (participants: A, B)

    par Phase 1: Prepare
        C->>SA: PREPARE(tx_id, operations)
        C->>SB: PREPARE(tx_id, operations)
    end

    SA-->>C: PREPARED
    SB-->>C: PREPARED

    Note over C: All prepared - commit decision
    C->>CL: Log COMMITTED (tx_id)

    par Phase 2: Commit
        C->>SA: COMMIT(tx_id)
        C->>SB: COMMIT(tx_id)
    end

    SA-->>C: COMMITTED
    SB-->>C: COMMITTED

    C->>CL: Clear entry (tx complete)
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

- **Replication**: High availability via replicas (raft-based)
- **Automatic Sharding**: Infer domains from label distribution
- **Shard Splitting**: Subdivide large shards automatically

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
