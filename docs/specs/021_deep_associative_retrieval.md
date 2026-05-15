# 🔭 Vantage Spec: Deep Associative Retrieval (Depth > 1 Fishing)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/experimental/fishing.rs` |

## 1. 👤 User Story

> **As a** Researcher or RAG Developer,
> **I want to** retrieve associative information across multiple hops (Depth > 1) from a starting concept,
> **So that** I can discover indirect, non-obvious relationships that provide deeper context than immediate neighbors, simulating extended cognitive association.

## 2. 🧐 The "So What?" (Business Value)

Currently, the Associative Retrieval ("Fishing") feature in AletheiaDB is limited to a depth of 1 (direct neighbors). While useful for immediate context, it fails to capture transitive relationships which are critical in complex domains like investigative journalism, fraud detection, and advanced scientific reasoning.

**The Gap:**
- **Shallow Context:** Retrieving only depth-1 neighbors misses the "friend of a friend" connections that often hold the most valuable insights.
- **Manual Chaining:** Users currently have to implement multi-step traversal logic and manually aggregate and score the results to achieve deep associative retrieval.

**ROI:**
- **RAG Enhancement:** Supplying LLMs with multi-hop context drastically reduces hallucination on complex reasoning tasks that require connecting disparate facts.
- **Product Differentiation:** Native, scored, deep associative retrieval positions AletheiaDB as a superior reasoning engine compared to standard vector or graph databases.

**Success Metric Definition:**
- **Performance:** A depth-3 query on a densely connected graph (1M nodes, average degree 10) completes in `< 50ms`.
- **Quality:** The scoring algorithm gracefully decays the relevance of nodes as structural depth increases, ensuring the results remain conceptually relevant to the original "bait".

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Deep Fishing API**:
    - The existing `cast(bait, config)` API MUST support a configurable `max_depth` parameter.
    - The algorithm MUST traverse up to `max_depth` hops from the starting node(s).
2.  **Scoring Algorithm Updates**:
    - The scoring function MUST incorporate a structural decay factor (e.g., `depth_penalty`).
    - Score = `(Vector_Similarity * vector_weight) + (Graph_Connection * graph_weight * (decay_factor ^ depth)) + (Freshness * freshness_weight)`.
3.  **Path Provenance**:
    - The `Catch` result object MUST include the structural path (the sequence of edges and nodes) that connected the "bait" to the result, explaining *how* the association was made.
4.  **Resource Limits**:
    - To prevent combinatorial explosion, the API MUST support a `max_nodes_visited` or `timeout` parameter, returning partial results if the limit is reached.

### Non-Functional Requirements

-   **Memory Safety:** The traversal must be implemented using memory-efficient graph algorithms (e.g., iterative deepening or A* variants) rather than naive recursive DFS.

## 4. 🚫 Out of Scope (Phase 1)

-   **Bidirectional Deep Fishing:** Starting from two separate concepts and finding the deepest, most associative path between them (this is Semantic Pathfinding, handled separately).
-   **Dynamic Decay Functions:** Allowing users to inject custom Lua or WASM functions to define how the score decays per hop. We will use a fixed mathematical decay model for the MVP.
