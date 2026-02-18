# 🔭 Vantage: Spec for Hindsight (Counterfactual Graph Simulation)

**Spec ID:** SPEC-001
**Feature:** Hindsight (Counterfactual Graph Simulation)
**Author:** Vantage (Product Manager)
**Status:** Proposed
**Related Component:** `src/experimental/hindsight.rs`

## 1. 👤 User Story

> "As an AI Agent Developer, I want to simulate hypothetical changes to the knowledge graph *without* committing them to the database, so that my agent can verify the consistency of new information and explore 'what-if' scenarios (reasoning) without polluting the long-term memory with hallucinations or tentative plans."

## 2. 🧠 The "So What?" (Business Value)

### The Problem: Hallucination Pollution
Current RAG (Retrieval-Augmented Generation) systems are often "write-once, read-forever." If an Agent infers a wrong fact and writes it to the DB, that fact pollutes future queries. Agents need a "sandbox" or "imagination" where they can test facts before committing them.

### The Opportunity: Causal Reasoning
To move from "Chatbots" to "Reasoning Agents," the system must support counterfactuals: *"If I assume X is true, does it contradict Y?"* AletheiaDB's bitemporal nature is great for *history*, but we need a future-facing/hypothetical layer.

### Success Metrics
- **Safety**: 100% isolation (virtual changes never leak to DB until explicit commit).
- **Performance**: < 5ms overhead for creating a scenario and running a hybrid query on it (vs. direct DB query).
- **Usability**: Zero-friction API (looks exactly like the standard `GraphView` API).

## 3. ✅ Acceptance Criteria

### 3.1 Core Functionality (CRUD)
The `Hindsight` engine must act as a mutable overlay on the immutable database.

- [ ] **Virtual Additions**: Can `add_node` and `add_edge` with temporary IDs (e.g., > `MAX_VALID_ID`).
- [ ] **Virtual Updates (Patch)**: Can `update_node` properties. This must merge with existing DB properties (patch semantics).
- [ ] **Virtual Removals**: Can `remove_node` and `remove_edge`. Removed items must be excluded from all subsequent queries in the scenario.
- [ ] **Diff Reporting**: A `diff()` method must return a structured summary of all changes in the scenario (Added/Modified/Removed).

### 3.2 Hybrid Search Overlay
Vector search is the primary interface for RAG. Hindsight must transparently merge virtual and physical results.

- [ ] **Unified Results**: `find_similar(vector, k)` returns top-k from:
    - (DB Nodes - Removed Nodes - Modified Nodes)
    - (Added Nodes)
    - (Modified Nodes with new vectors)
- [ ] **Correct Ranking**: Results must be correctly sorted by similarity score across both sources.
- [ ] **Filtration**: Removed nodes must *never* appear in results.

### 3.3 Graph Traversal Overlay
Pathfinding and traversal must respect the virtual topology.

- [ ] **Virtual Connectivity**: `get_outgoing_edges(node)` returns (DB edges - Removed edges) + (Added edges).
- [ ] **Pathfinding**: `find_path(start, end)` can find paths that only exist due to virtual edges.
- [ ] **Broken Paths**: `find_path` fails if a critical edge/node was virtually removed.

### 3.4 Time Travel Base
Hindsight must support branching from *any* point in history, not just the present.

- [ ] **Historical Fork**: `Hindsight::at(valid_time, tx_time)` creates a simulation based on the DB state at that timestamp.
- [ ] **Use Case**: "What if I had done X yesterday?" (Root Cause Analysis).

## 4. 🚫 Out of Scope (Phase 1)

- **Persistent Scenarios**: Storing scenarios to disk. (Phase 1 is in-memory only).
- **Multi-User Collaboration**: Sharing a scenario between threads/users.
- **Transaction Integration**: "Committing" a scenario to the DB as a real transaction. (This is a nice-to-have, but manually applying the diff is acceptable for Phase 1).

## 5. 📝 Usage Example

```rust
use aletheiadb::experimental::hindsight::Hindsight;

// 1. Create a "What-If" Scenario based on current state
let mut scenario = Hindsight::new(&db);

// 2. Imagine a new fact
let fact_id = scenario.add_node("Fact",
    props! { "text" => "Sky is Green", "vector" => [...] }
)?;

// 3. Query the imagination
// This finds the new fact because it's in the overlay
let results = scenario.find_similar("vector", query_vec, 5)?;

// 4. Verify consistency (e.g., check if it contradicts existing knowledge)
// If consistent -> Commit to real DB
// If inconsistent -> Discard scenario
```

## 6. 🛠️ Technical Considerations (Constraints)

- **Resource Isolation**: Virtual scenarios must be lightweight and automatically garbage-collected when the simulation object is dropped.
- **Identifier Safety**: Virtual entities must have distinct IDs that cannot conflict with persistent database IDs.
- **Concurrency Model**: Scenarios are intended for local, single-threaded simulation by an agent (no need for complex locking overhead).
