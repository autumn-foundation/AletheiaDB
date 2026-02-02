# 🔭 Vantage Spec: Semantic Pathfinding (Concept-Guided Traversal)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-001 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/experimental/semantic_pathfinding.rs` (Planned) |

## 1. 👤 User Story

> **As a** RAG (Retrieval-Augmented Generation) Developer,
> **I want to** find nodes that are semantically similar but structurally distant, and the paths connecting them,
> **So that** I can discover and explain non-obvious relationships without polluting the context window with irrelevant ("off-topic") structural connections.

## 2. 🧐 The "So What?" (Business Value)

Standard Graph Databases offer shortest-path algorithms (BFS/Dijkstra) which optimize for *structural* efficiency (fewest hops).
Standard Vector Databases offer similarity search (ANN) which optimizes for *semantic* proximity (synonyms).

**The Gap:**
Real-world reasoning often requires traversing a graph but staying "in context".
*Example:* Finding a connection between "Apple" (the company) and "Foxconn" (the manufacturer).
- *Structural Search* might go through "Steve Jobs" -> "Pixar" -> "Disney" -> ... (drifting off topic).
- *Semantic Pathfinding* with query "Manufacturing" forces the traversal to prefer nodes related to supply chain/tech, avoiding the "Pixar" detour.

**ROI:**
- **Differentiation:** Positions GallifreyDB as a "Reasoning Engine", not just a storage engine.
- **Utility:** Directly improves RAG accuracy by filtering "hallucination-inducing" noise from retrieved subgraphs.

## 3. ✅ Acceptance Criteria

### Functional Requirements
1.  **Weighted Traversal API**:
    - Must expose `find_path(start_node, end_node, query_concept)` where `query_concept` is a vector or text string.
    - The algorithm must use an A* (or Dijkstra) approach where `Cost = Structural_Cost + (1.0 - Cosine_Similarity(Node, Query))`.
    - Nodes with high semantic similarity to the query must be "cheaper" to traverse.

2.  **Time-Travel Support**:
    - Must expose `find_path_at_time(start, end, query, timestamp)`.
    - Must strictly respect the `valid_time` of edges and nodes (do not traverse edges that didn't exist or were deleted at `timestamp`).

3.  **Result Format**:
    - Returns an ordered `Vec<NodeId>` representing the path.
    - Should optionally return the "Semantic Cost" of the path (confidence score).
    - Must handle cases where no path is found gracefully (return empty `Option` or specific Error, no panic).

4.  **Configuration**:
    - Allow tuning the balance between Structural Weight vs. Semantic Weight (e.g., `alpha` parameter).

### Non-Functional Requirements (Constraints)
-   **Performance**:
    - Latency < 50ms for 5-hop paths on 1M node graph.
    - Must be significantly faster than exhaustive BFS for "deep" graphs when a strong semantic signal exists.
-   **Safety**: Must handle `NaN` in vectors gracefully (no panics).
-   **Scale**: Must support paths up to 50 hops (configurable `max_depth`).

## 4. 🚫 Out of Scope (Phase 1)

-   **Bi-directional Search**: Starting from both ends simultaneously (complexity with semantic heuristics).
-   **Negative Constraints**: "Find path A->B avoiding topic X".
-   **Multi-Vector Queries**: "Start with topic A, then switch to topic B halfway" (Semantic waypoint navigation).

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Code** | Missing (`src/experimental/semantic_pathfinding.rs`) | Implemented | Create experimental module |
| **Algorithm** | None | Optimized `A*` | Implement heuristic traversal |
| **Time-Travel** | N/A | Robust | Ensure `CurrentStorage` visibility logic handles deletions |
| **API** | None | Public `Graph` Trait method | Expose via `QueryBuilder` |

## 6. 📅 Execution Plan

1.  **Create** `src/experimental/semantic_pathfinding.rs` (gated by `nova`).
2.  **Implement** A* heuristic using `pathfinding` crate or internal structure.
3.  **Refactor** `CurrentStorage` access to ensure time-travel correctness.
4.  **Test** with the standard "Apple vs. Orange" dataset.
