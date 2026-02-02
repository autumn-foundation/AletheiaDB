# 🔭 Vantage Spec: Semantic Pathfinding (Concept-Guided Traversal)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-001 |
| **Status** | ✅ Ready for Implementation |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/algo/pathfinding.rs` (To be created) |

## 1. 👤 User Stories

> **As a** RAG (Retrieval-Augmented Generation) Developer,
> **I want to** find paths between entities that are semantically consistent with a specific concept or topic,
> **So that** I can retrieve multi-hop context for an LLM that explains *how* two things are related without polluting the context window with irrelevant ("off-topic") structural connections.

> **As a** Fraud Analyst,
> **I want to** find connections between a "Suspect Account" and a "Known Laundry Service" that align with the concept of "Financial Transaction",
> **So that** I can ignore benign social connections (like "Shared Address") and focus on the money trail.

## 2. 🧐 The "So What?" (Business Value)

Standard Graph Databases offer shortest-path algorithms (BFS/Dijkstra) which optimize for *structural* efficiency (fewest hops).
Standard Vector Databases offer similarity search (ANN) which optimizes for *semantic* proximity (synonyms).

**The Gap:**
Real-world reasoning often requires traversing a graph but staying "in context". A shortest path often passes through high-degree "supernodes" (like "Internet" or "USA") that lose the semantic thread.

**ROI:**
- **Differentiation:** Positions GallifreyDB as a "Reasoning Engine", not just a storage engine.
- **Utility:** Directly improves RAG accuracy by filtering "hallucination-inducing" noise from retrieved subgraphs.
- **Efficiency:** Prunes the search space using semantic heuristics, potentially faster than exhaustive BFS on massive graphs.

## 3. 📝 Concrete Scenario: "The Apple Ambiguity"

Consider a graph with two clusters:
1.  **Tech Cluster:** Apple Inc -> iPhone -> Foxconn -> Manufacturing
2.  **Food Cluster:** Apple (Fruit) -> Pie -> Flour -> Manufacturing

**Query:** `find_path(start="Apple Inc", end="Manufacturing", concept="Technology")`

-   **Standard BFS:** Might find `Apple Inc` -> `Shareholder` -> `Bakery` -> `Pie` -> `Manufacturing` (if specific edges exist).
-   **Semantic Pathfinding:**
    -   Calculates cost for neighbors of "Apple Inc".
    -   "iPhone" is semantically close to "Technology" -> Low Cost.
    -   "Shareholder" might be neutral -> Medium Cost.
    -   Path follows the "Tech" route because the cumulative semantic cost is lower.

## 4. ✅ Acceptance Criteria

### Functional Requirements
1.  **Public API**:
    -   `find_path(start: NodeId, end: NodeId, concept: Vector) -> Result<Path>`
    -   `find_path_by_text(start: NodeId, end: NodeId, concept: &str) -> Result<Path>` (Convenience wrapper utilizing embedding provider)
2.  **Algorithm**:
    -   Must use **A\*** (A-Star) or similar heuristic search.
    -   **Cost Function**: $Cost(u, v) = W_{struct} + W_{semantic} \cdot (1.0 - \text{sim}(v, \text{concept}))$
    -   Nodes semantically dissimilar to the query concept incur a high penalty, effectively blocking "off-topic" paths.
3.  **Configurability**:
    -   `PathfindingConfig` struct exposing `structural_weight` (default 1.0) and `semantic_weight` (default 5.0).
    -   `max_depth` (default 50).
    -   `heuristic_factor` (for A* optimization).
4.  **Result**:
    -   Return `Vec<NodeId>` (ordered path).
    -   Return `total_cost` and `semantic_score` of the path.

### Performance Requirements
-   **Latency**: < 50ms for a 5-hop path on a graph with 100k nodes (assuming vectors are cached).
-   **Memory**: Must not load the entire graph. Use the existing cache/disk paging mechanisms.

### Edge Cases
-   **Disconnected**: Return `None` or specific error if no path exists within `max_depth`.
-   **No Vector**: If a node lacks a vector, apply a default "Neutral" penalty (configurable).
-   **Self-Loop**: Path from A to A returns cost 0.

## 5. 🛠 Technical Constraints & Recommendations

-   **Dependency**: Use the `pathfinding` crate (or similar) for the generic A* implementation if possible, implementing the `Graph` trait for `GallifreyDB`.
-   **Vector Access**: Accessing vectors for every node expansion is expensive.
    -   *Optimization*: Use `HnswIndex` to pre-filter or cache hot vectors?
    -   *Constraint*: Must work with `TieredStorage` (fetch from disk if needed).
-   **Time-Travel**: Phase 2 will add `find_path_at_time`. For Phase 1, use `CurrentStorage`.

## 6. 📅 Execution Plan

1.  **Core Implementation**:
    -   Create `src/algo/pathfinding.rs`.
    -   Implement `CostFunction` trait.
2.  **Integration**:
    -   Expose via `QueryBuilder` extensions (`.traverse_semantically(...)`).
3.  **Testing**:
    -   Unit tests with small, hand-crafted graphs (The "Apple" scenario).
    -   Integration test with `fishing` module to combine retrieval + pathfinding.
