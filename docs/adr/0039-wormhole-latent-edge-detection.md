# ADR-0039: Wormhole (Latent Edge Detection)

**Status:** Proposed
**Date:** 2026-05-25
**Deciders:** AletheiaDB Core Team
**Categories:** experimental, hybrid-search, reasoning

## Context

In a hybrid graph-vector database, relationships exist in two spaces:
1.  **Structural Space (Graph):** Explicit edges (e.g., `A -> KNOWS -> B`).
2.  **Semantic Space (Vector):** Implicit similarity (e.g., `cosine_similarity(A.vector, B.vector) > 0.9`).

A common analytical need is to identify discrepancies between these two spaces. Specifically, finding pairs of nodes that are **semantically close** but **structurally distant** (or disconnected). These represent "Latent Edges" or "Missing Links."

Examples:
*   **Social Networks:** Two users share interests (high semantic similarity) but are not friends (no structural path).
*   **Fraud Detection:** Two accounts behave identically (high semantic similarity) but have no transactional history (disconnected).
*   **Recommendation:** Products that serve the same purpose but are never bought together.

Currently, discovering these patterns requires an O(N²) pairwise comparison or exporting data to external graph data science libraries. We need a native, efficient way to detect these "Wormholes" within AletheiaDB.

## Decision

We will implement the **Wormhole Detector** (codenamed "Wormhole") in `src/experimental/wormhole.rs`.

The detector implements a hybrid search algorithm:
1.  **Semantic Candidate Generation:** For a given set of source nodes, use the HNSW vector index to retrieve the top-k nearest neighbors in vector space.
2.  **Structural Validation:** For each semantic neighbor, perform a Breadth-First Search (BFS) to determine the shortest path distance in the graph.
3.  **Filtration:** If the structural distance exceeds a configured `max_hops` threshold (or is infinite/disconnected), the pair is returned as a `Wormhole`.

The API exposes this as `find_wormholes(candidates, k, max_hops)`.

## Consequences

### Positive

*   **Native Insight Discovery:** Users can find latent relationships without external tools or expensive global algorithms.
*   **Performance:** By using the vector index as a "heuristic filter," we reduce the search space from O(N²) to O(N * k), making link prediction feasible in real-time.
*   **Hybrid Synergy:** Demonstrates the core value proposition of AletheiaDB (Graph + Vector integration).

### Negative

*   **BFS Cost:** The structural check involves a BFS for each of the `N * k` candidates. If `max_hops` is large (>3), this can become computationally expensive. The implementation must strictly enforce the depth limit.
*   **Probabilistic Nature:** The results depend heavily on the quality of the vector embeddings. Poor embeddings will yield false positive "wormholes."
*   **Concurrency:** The BFS must acquire read locks on the graph structure, potentially contending with heavy write workloads.
