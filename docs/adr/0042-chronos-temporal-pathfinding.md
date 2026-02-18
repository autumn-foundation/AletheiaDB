# 42. Chronos: Temporal Graph Analysis & Pathfinding

Date: 2024-05-22

## Status

Proposed

## Context

Most graph databases support traversal on the current state of the graph. However, the connectivity of a graph evolves over time: edges are added, removed, or properties change.

Analyzing historical connectivity is critical for:
-   **Forensics**: "Was there a path from A to B at the time of the incident?"
-   **Routing**: "Find a stable route that has existed for at least 30 days."
-   **Network Analysis**: "How volatile is this section of the network?"

Standard pathfinding algorithms (Dijkstra, BFS) operating on the current graph cannot answer these questions. We need algorithms that respect the temporal dimension.

## Decision

We will implement **Chronos**, a Temporal Graph Analysis engine, within the `Nova` experimental suite.

### Core Concepts

1.  **Snapshot Pathfinding**: Finding a path between nodes as the graph existed at a specific point in time (`valid_time`, `transaction_time`).
2.  **Node Volatility**: A metric representing the frequency of updates to a node within a given time window.
3.  **Path Stability**: A metric (0.0 to 1.0) representing the fraction of a time window during which a specific path (sequence of edges) was continuously valid.

### Algorithm

Chronos provides specialized traversal methods:

1.  **Snapshot BFS**:
    -   Standard BFS logic, but modified edge expansion.
    -   Instead of `get_neighbors(node)`, use `get_neighbors_at_time(node, valid_time, tx_time)`.
    -   This requires reconstructing the edge state or checking edge existence intervals for every step.

2.  **Stability Calculation**:
    -   For a given path (sequence of Edge IDs) and a time window `[T_start, T_end]`:
    -   For each edge, retrieve its valid time intervals within the window.
    -   Compute the intersection of these intervals across all edges in the path.
    -   `Stability = Total_Duration(Intersection) / Total_Duration(Window)`.

## Consequences

### Positive
-   **Time Travel Navigation**: Enables users to explore the graph as it was, not just as it is.
-   **Dynamic Insights**: Provides quantitative metrics (volatility, stability) to understand graph evolution, useful for risk assessment and anomaly detection.
-   **Granularity**: Respects bitemporal valid time and transaction time, ensuring accurate historical reconstruction.

### Negative
-   **Performance Cost**: Snapshot pathfinding is significantly more expensive than standard traversal. Each step may require checking historical versions or reconstructing state, potentially adding O(V) overhead per edge traversal where V is the number of versions.
-   **Complexity**: Temporal interval intersection logic can be complex to implement correctly, especially with bitemporal intervals.
-   **Optimization Need**: The initial implementation uses naive checks. Future optimizations (e.g., Time-Aggregated Graphs or Temporal Indexes) will be necessary for large-scale traversals.
