# ADR-0038: Kaleidoscope Layout Engine

**Status:** Accepted
**Date:** 2026-05-20
**Deciders:** GallifreyDB Core Team
**Categories:** visualization, experimental

## Context

Visualizing a graph database is challenging because there are two distinct types of "closeness":
1.  **Topological Closeness:** Nodes are connected by an edge.
2.  **Semantic Closeness:** Nodes have similar vector embeddings (meaning).

Standard force-directed algorithms (like Fruchterman-Reingold) only show topology. Dimensionality reduction techniques (like t-SNE or UMAP) only show semantic clustering.

We need a way to visualize the "Semantic Shape" of the data, where nodes are positioned based on a synthesis of their structural connections and their conceptual similarity.

## Decision

We will implement **Kaleidoscope**, a Semantic Force-Directed Layout Engine, within the `experimental` feature set.

Kaleidoscope uses a physics simulation to determine node positions in a 2D plane:

-   **Nodes** act as charged particles (Coulomb repulsion), preventing overlap.
-   **Edges** act as springs (Hooke's Law), pulling connected nodes together.
-   **Vector Similarity** acts as "Semantic Gravity". If two nodes have a high cosine similarity, an attractive force pulls them together, even if no edge exists between them.

### Algorithm

The simulation runs for `N` iterations (cooling down over time):
1.  Calculate Repulsion (All-pairs).
2.  Calculate Spring Attraction (Edges).
3.  Calculate Semantic Attraction (High similarity pairs).
4.  Update positions based on net force and current temperature.

## Consequences

### Positive

-   **Unified Visualization:** Users can "see" that two clusters are semantically related even if they are disconnected in the graph.
-   **Interactive:** The physics model naturally supports animation and interaction.
-   **Insight:** Helps identify "Missing Edges" (nodes that are semantically identical but not linked).

### Negative

-   **Scalability:** The all-pairs repulsion calculation is O(N^2), limiting real-time layout to small subgraphs (< 1000 nodes).
-   **Parameter Sensitivity:** Balancing the three forces (Repulsion, Spring, Gravity) is difficult. If Gravity is too strong, the graph collapses into a single point. If Repulsion is too strong, semantic clusters blow apart.

## Compliance

This ADR aligns with the "Mosaic" persona's goal of polishing the "Human Interface" and prioritizing visuals.
