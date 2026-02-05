# ADR-0035: Chronos Temporal Analysis

**Status:** Accepted
**Date:** 2026-05-20
**Deciders:** GallifreyDB Core Team
**Categories:** analysis, temporal, experimental

## Context

GallifreyDB stores the complete history of the graph, but standard graph algorithms (like Breadth-First Search or PageRank) typically operate on a single static snapshot (usually the current state). While the core `GallifreyDB` struct provides low-level "Time Travel" APIs (`get_node_at_time`), there is a gap in higher-level analytical capabilities.

Users need to answer questions about the *evolution* of the graph, such as:
1.  "Did a path exist between A and B during the 2023 financial crisis?"
2.  "How stable is the relationship between these entities?" (Do they keep breaking up?)
3.  "Which nodes are the most volatile?" (High frequency updates).

Running these analyses by manually querying snapshots in a loop is inefficient and cumbersome.

## Decision

We will implement a dedicated temporal analysis module, **Chronos**, within the `experimental` feature set.

Chronos will provide three core capabilities:

1.  **Snapshot Pathfinding:** A modified BFS that traverses the graph as it existed at a specific `valid_time`. It ensures that every edge traversed was valid at that exact moment.
2.  **Node Volatility Analysis:** A metric defined as `(Number of Versions) / (Time Window Duration)`. This quantifies how "hot" a node is in terms of updates.
3.  **Path Stability Analysis:** A metric defined as the fraction of a time window where a path (sequence of edges) was *continuously* valid. This requires calculating the intersection of valid time intervals for all edges in the path.

### Key Implementation Details

-   **Isolation:** Chronos is implemented as a wrapper struct `Chronos<'a>` holding a reference to `GallifreyDB`.
-   **BFS Logic:** The pathfinding algorithm queries `get_outgoing_edges_at_time` at each step, ensuring temporal consistency.
-   **Interval Intersection:** For path stability, we compute the intersection of validity intervals for all edges involved. If the intersection is empty, the stability is 0.

## Consequences

### Positive

-   **Deep Insight:** Users can now reason about the *dynamics* of the graph, not just its structure.
-   **Abstraction:** Hides the complexity of iterating through version chains and checking temporal overlap.
-   **Diagnostic Value:** Volatility metrics are useful for identifying hotspots or "flickering" data sources.

### Negative

-   **Performance Cost:** Temporal pathfinding is slower than static pathfinding because it requires checking validity predicates for every edge traversal.
-   **Memory Overhead:** Constructing interval intersections for long paths over long histories involves allocating intermediate vectors of `TimeRange`.

## Compliance

This ADR supports the "Time Lord" capabilities of GallifreyDB and aligns with `SPEC-006` (Temporal Analysis).
