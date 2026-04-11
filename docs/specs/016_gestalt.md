# 🔭 Vantage Spec: Gestalt (Semantic Subgraph Matching)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-016 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/experimental/gestalt/` (Proposed) |

## 1. 👤 User Stories

> **As a** Fraud Analyst or Threat Hunter,
> **I want to** find "fuzzy" structural patterns in a transaction graph,
> **So that** I can detect money laundering rings or coordinated attack vectors even if they slightly vary their connection structure or transaction amounts over time.

## 2. 🧐 The "So What?" (Business Value)

Current graph databases rely on rigid subgraph matching (e.g., Cypher MATCH). If a bad actor adds an intermediary account (an extra node) or changes the transaction slightly, the rigid query fails.

**The Gap:**
- **Fragile Queries:** Strict pattern matching means slight deviations in graph structure evade detection. Analysts spend hours manually updating queries to account for minor mutations.
- **Lost Detections:** Fraud rings actively mutate their structures to bypass static rules.

**ROI:**
- **Improved Detection:** Gestalt solves the "fragile query" problem by matching subgraphs based on *semantic* similarity and *structural* approximation, leading to a higher detection rate of mutated fraud rings.
- **Productivity:** Lowers the time analysts spend tweaking queries, allowing them to focus on threat analysis rather than query maintenance.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Pattern Subgraph Specification**:
    -   Must define an API/DSL for specifying a Pattern Subgraph containing Node Constraints (Label, Vector Similarity Threshold) and Edge Constraints (Label).
2.  **Semantic Search Integration**:
    -   Must evaluate potential subgraph matches by utilizing the vector search engine to measure semantic similarity of node properties.
3.  **Graceful Degradation**:
    -   Must handle missing semantic data (e.g., missing vectors) gracefully without panicking, simply excluding those nodes from potential matches or assigning a default low similarity score.
4.  **Scoring & Ranking**:
    -   Must provide a cumulative "Gestalt Score" (0.0 to 1.0) for each matched subgraph, representing the overall confidence of the match based on combined node and edge similarities.
    -   Must return a list of concrete subgraphs (nodes and edges) from the database that meet or exceed the global similarity threshold of the pattern.

### Non-Functional Requirements
-   **Metric Definition:**
    -   **Match Recall:** Gestalt identifies >90% of subgraphs where node embeddings deviate by up to 10% from the baseline pattern.
    -   **Query Latency:** Subgraph pattern matching with up to 5 nodes/edges completes in <50ms for a graph of 1M nodes.

## 4. 🚫 Out of Scope (Phase 1)

-   **Real-time Stream Processing**: Continuous pattern detection on streaming data (Phase 2).
-   **Cross-Shard Patterns**: Distributed pattern matching across multiple shards (Phase 2).
-   **Dynamic Edge Weights**: Only semantic vectors on nodes will be scored in MVP; dynamic edge properties won't influence the Gestalt score yet.
-   **Auto-Generation**: Auto-generating patterns from historical examples (This is "Muse" or "Alchemy", not Gestalt).
