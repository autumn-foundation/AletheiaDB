# ADR-0061: Semantic Navigator Vector-Guided Pathfinding

**Status:** Proposed
**Date:** 2026-06-25
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Semantic Search, Traversal

## Context

Standard graph pathfinding algorithms (like Dijkstra's or Breadth-First Search) find the shortest structural path between two nodes. Standard vector searches find nodes with the highest semantic similarity to a query. However, advanced AI agents often need to "reason" their way from one concept to another by finding a *semantically smooth* path through the graph's topology.

We need a mechanism to navigate the graph where the "cost" of traversing an edge is not a fixed structural distance, but the semantic distance between the connected nodes. This allows for exploratory paths that maintain conceptual coherence.

## Decision

We will implement the **Semantic Navigator** as an experimental module in `src/experimental/semantic_navigator.rs`.

The `Semantic Navigator` employs an A* pathfinding algorithm on the graph using vector similarity as both the heuristic and the cost function:
-   **Nodes** represent states.
-   **Edges** represent possible transitions.
-   **Cost** of traversing an edge from node A to node B is calculated as `1.0 - cosine_similarity(Vector_A, Vector_B)`.
-   **Heuristic (h-score)** for a node N to a goal G is `1.0 - cosine_similarity(Vector_N, Vector_G)`.

This approach dynamically favors traversing paths where adjacent nodes share similar meanings and where the path progressively approaches the semantic goal.

## Consequences

### Positive
-   **Smooth Conceptual Transitions**: Generates paths between disparate concepts that "make sense" to a human or LLM by moving through related intermediary concepts.
-   **Directed Discovery**: Offers a targeted form of exploratory traversal that combines structural constraints with semantic goals.
-   **Agentic Reasoning**: Models how a human might "connect the dots" during a brainstorming or research session.

### Negative
-   **Performance Limits**: A* can be slow on dense graphs. Calculating vector similarities dynamically during traversal is compute-intensive compared to standard graph hops.
-   **Dead Ends**: If the graph lacks structural edges that bridge a semantic gap, the navigator might fail to find a path even if the concepts are semantically close in vector space.

## References
- `src/experimental/semantic_navigator.rs`
