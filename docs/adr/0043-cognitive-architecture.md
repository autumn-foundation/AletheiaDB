# 43. Cognitive Architecture Components

Date: 2024-05-21

## Status

Accepted

## Context

AletheiaDB provides a strong foundation for bitemporal graph and vector storage. However, AI agents and advanced applications require higher-order cognitive functions—reasoning, association, prediction, and explanation—that go beyond simple CRUD or traversal operations. Agents need to "think" about the data, not just retrieve it.

Traditional graph databases offer traversal, and vector databases offer similarity search. But bridging these two worlds to perform tasks like "finding a narrative thread" (Ariadne), "associative memory recall" (Fishing), or "memetic propagation" (Sybil) requires significant custom code. By embedding these capabilities directly into the database's experimental layer, we enable "agentic" workflows to run closer to the data, reducing latency and complexity.

## Decision

We will implement a suite of "Cognitive Architecture" components in the `src/experimental` module. These components provide specialized reasoning capabilities that leverage both the graph structure and vector embeddings of AletheiaDB.

The components are:

1.  **Ariadne (The Weaver)**: Finds narrative threads connecting disparate events via causality (edges) and semantic similarity (vectors).
2.  **Prophet (The Seer)**: Predicts missing links using topological (Adamic-Adar) and semantic signals.
3.  **Fishing (The Net)**: Performs associative retrieval, simulating human memory recall by casting a "bait" (vector) and spreading a "net" (graph traversal).
4.  **Kaleidoscope (The Lens)**: Visualizes the semantic shape of data via a force-directed layout engine that balances structural and semantic forces.
5.  **Semantic Navigator (The Pathfinder)**: Navigates the graph using semantic proximity (vectors) as a heuristic for A* pathfinding.
6.  **Sentinel (The Guardian)**: Enforces semantic integrity and safety rules on incoming data (e.g., banning "toxic" vectors).
7.  **Sybil (The Simulator)**: Models the propagation of information, beliefs, or memes through the network.
8.  **Temporal Diff (The Comparator)**: Analyzes structural and property changes between two points in time.
9.  **Narrative Generator (The Scribe)**: Converts graph history into human-readable narratives.

## Consequences

### Positive
*   **Agentic Native**: Enables complex agentic workflows (memory, reasoning, safety) natively within the DB.
*   **Reduced Glue Code**: Reduces the need for external logic in Python/LangChain to coordinate graph and vector operations.
*   **Performance**: Runs reasoning loops closer to the data, avoiding network round-trips for each step of a traversal.

### Negative
*   **API Surface**: Increases the surface area of the API.
*   **Experimental Status**: These components are experimental and may change or be removed.
*   **Complexity**: Adds complexity to the codebase and requires maintenance of higher-level algorithms.
*   **Performance Overhead**: Some components (like Kaleidoscope or Sybil) can be compute-intensive and may impact database performance if not managed carefully.
