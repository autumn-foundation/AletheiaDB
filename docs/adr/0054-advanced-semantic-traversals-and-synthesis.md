# ADR-0054: Advanced Semantic Traversals & Synthesis

**Status:** Accepted
**Date:** 2026-06-25
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Semantic Search, Traversal

## Context

As AletheiaDB evolves to support advanced AI agents, traditional similarity-based querying (e.g., nearest neighbors) is no longer sufficient. Agents need ways to break out of "filter bubbles," explore surprising connections, detect global paradigm shifts, align perspectives, synthesize core concepts from disparate ideas, and predict the future collision of semantic trajectories.

We have identified several advanced patterns that are missing from our cognitive architecture:
1.  **Novelty Exploration:** Finding the *least* similar path to foster creative brainstorming.
2.  **Scenic Routing:** Finding paths that maximize semantic divergence between endpoints.
3.  **Macro Shift Detection:** Identifying "earthquakes" or paradigm shifts across the entire graph over time.
4.  **Subjective Traversals:** Modifying graph distance based on a specific semantic "Lens".
5.  **Concept Synthesis:** Automatically materializing a core concept that bridges multiple disparate ideas.
6.  **Collision Prediction:** Predicting when two concepts' semantic trajectories will intersect.

## Decision

We will implement a suite of advanced experimental engines in `src/experimental/` to support these capabilities:

1.  **Voyager (Maximal Novelty Traversal):**
    *   **Goal:** Explore the graph by intentionally choosing neighbors with the lowest cosine similarity.
    *   **Mechanism:** Greedy traversal that penalizes similarity to break out of local minimums.

2.  **Serendipity (The Scenic Route Finder):**
    *   **Goal:** Find paths between nodes that maximize semantic divergence along the way.
    *   **Mechanism:** Pathfinding where the cost is inversely proportional to semantic distance, encouraging "leaps" in meaning.

3.  **Tremor (Semantic Earthquake Detector):**
    *   **Goal:** Detect global or local semantic shifts over time.
    *   **Mechanism:** Compares the global distribution of vectors (centroids) between two time windows.

4.  **Spectre (Semantic Perspective Engine):**
    *   **Goal:** View the graph through a specific "Lens" (vector).
    *   **Mechanism:** Warps the effective distance between nodes during traversal based on their alignment with the Lens.

5.  **Luna (Semantic Subgraph Synthesis):**
    *   **Goal:** Materialize the core concept bridging multiple ideas.
    *   **Mechanism:** Computes the semantic center of gravity for seed nodes and dynamically instantiates a new core node connected to the seeds.

6.  **Omen (Semantic Collision Detection):**
    *   **Goal:** Predict future interactions of concepts based on their semantic trajectories.
    *   **Mechanism:** Models trajectories as velocity vectors and solves for the minimal future distance between them.

## Consequences

### Positive
*   **Agent Creativity:** Agents can now generate novel ideas and creative associations (`Voyager`, `Serendipity`).
*   **Predictive Power:** System can warn users about impending structural or semantic collisions (`Omen`, `Tremor`).
*   **Dynamic Structure:** The graph can self-organize by materializing implicit core concepts (`Luna`).

### Negative
*   **Computational Overhead:** Calculating centroids (`Tremor`), velocities (`Omen`), and warped pathfinding (`Serendipity`, `Spectre`) can be CPU-intensive and requires fetching many vectors.
*   **Complexity:** These features introduce significant algorithmic complexity beyond standard CRUD and HNSW searches.

## References
- `src/experimental/voyager.rs`
- `src/experimental/serendipity.rs`
- `src/experimental/tremor.rs`
- `src/experimental/spectre.rs`
- `src/experimental/luna.rs`
- `src/experimental/omen.rs`
