# 🔭 Vantage Spec: Associative Retrieval ("Fishing")

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-002 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/experimental/fishing.rs` |

## 1. 👤 User Story

> **As a** Researcher or Analyst,
> **I want to** retrieve information by "casting a net" around a concept (vector) and expanding to its neighbors,
> **So that** I can simulate human-like associative recall (finding things that are similar *or* related) without constructing complex multi-step queries.

## 2. 🧐 The "So What?" (Business Value)

Traditional queries are either:
- **Precise**: `MATCH (n) WHERE n.id = 123` (Lookup)
- **Semantic**: `SIMILAR TO vector` (Similarity)

**The Gap:**
Human memory is associative. If I think of "Apples", I might remember "Oranges" (similarity) and "Pie" (structural relationship).
"Fishing" combines these into a single scored retrieval operation.

**ROI:**
- **Usability**: drastically simplifies "explore around this concept" workflows for UI/UX.
- **RAG Enhancement**: Allows retrieving a "School" of related context that includes both synonyms and direct neighbors, providing richer context for LLMs.

## 3. ✅ Acceptance Criteria

### Functional Requirements
1.  **Fishing API**:
    - Must expose a `cast(bait, config)` method.
    - `Bait` can be a Node ID (use its vector) or a raw Vector.
    - Returns a list of `Catch` objects (Node + Score + Provenance).

2.  **Scoring Algorithm**:
    - Score = (Vector_Similarity * vector_weight) + (Graph_Connection * graph_weight) + (Freshness * freshness_weight).
    - Must support customizable weights for each component.

3.  **Freshness Bias**:
    - Must optionally boost scores for recently modified nodes (Time Decay function).

4.  **Provenance**:
    - Each result must explain *why* it was caught (e.g., "Vector Similarity: 0.9" or "Linked from Node X").

### Non-Functional Requirements
-   **Performance**: Latency < 20ms for Depth=1 queries on 1M node graph.
-   **Safety**: Must handle missing vector indexes gracefully (return specific error).

## 4. 🚫 Out of Scope (Phase 1)

-   **Deep Traversal**: Depth > 1 (Fishing deep into the graph). Currently limited to direct neighbors.
-   **Complex Filters**: Filtering the "School" by complex predicates (e.g., `WHERE property > 10`) before expansion.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State (`experimental`) | Required State (`core`) | Action |
| :--- | :--- | :--- | :--- |
| **Code** | Implemented (`fishing.rs`) | Implemented | Move to `src/algo/` or `src/query/` |
| **Depth** | Limited to Depth 1 | Configurable | Implement BFS expansion for depth > 1 |
| **API** | `FishingRod` struct | Integrated `QueryBuilder` | Add `.fish()` or `.associate()` to Query API |
| **Tests** | Basic Unit Tests | Property Tests | Add randomized property tests |

## 6. 📅 Execution Plan

1.  **Review** `src/experimental/fishing.rs` implementation against metrics.
2.  **Implement** Depth > 1 support (Backlog item).
3.  **Integrate** into main `QueryBuilder` API.
4.  **Promote** from `experimental` to `core`.
