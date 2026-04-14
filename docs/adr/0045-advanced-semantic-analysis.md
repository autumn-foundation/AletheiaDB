# 45. Advanced Semantic Analysis (Physics of Meaning)

Date: 2024-05-22

## Status

Proposed

## Context

Graph databases traditionally focus on structural connectivity (who is connected to whom), while vector databases focus on semantic similarity (what is similar to what). AletheiaDB bridges this gap, but raw similarity search is insufficient for higher-order reasoning.

Recent experimental work has introduced three new engines that analyze the interplay between structure and semantics:
1.  **Dissonance (Semantic Stress)**: Detects nodes whose structural position contradicts their semantic meaning (e.g., a "Fruit" node connected only to "Tech" nodes).
2.  **Gestalt (Semantic Subgraph Matching)**: Finds complex patterns where nodes match by semantic concept rather than exact label (e.g., "Find a [Engineer-like] working for a [Startup-like]").
3.  **Gravity (Semantic Influence)**: Measures the "mass" and "pull" of a node on its neighbors over time, identifying influential entities that drive semantic change.

These components are currently undocumented and lack a formalized architectural decision record.

## Decision

We will formalize the inclusion of these "Semantic Physics" engines in the `src/experimental` module.

The architecture defines three distinct analytical domains:
*   **Semantic Stress (Dissonance)**: `Dissonance = Avg(Sim(KNN)) - Avg(Sim(GraphNeighbors))`. High dissonance indicates an anomaly or hallucination.
*   **Semantic Pattern Matching (Gestalt)**: Extends subgraph isomorphism to include vector similarity constraints as first-class citizens in the query pattern.
*   **Semantic Influence (Gravity)**: Models nodes as celestial bodies with mass (degree) and gravity (vector attraction), analyzing the orbits (velocity/trajectory) of their neighbors.

## Consequences

### Positive
*   **Anomaly Detection**: `Dissonance` provides a native way to detect "hallucinations" or bad data in knowledge graphs.
*   **Flexible Querying**: `Gestalt` allows for "fuzzy" graph pattern matching, essential for AI agents that reason in concepts rather than rigid schemas.
*   **Trend Analysis**: `Gravity` enables measuring the velocity of adoption or the polarization of communities over time.

### Negative
*   **Computational Cost**: These analyses require heavy vector index usage (KNN searches) and graph traversals, potentially impacting performance if run on large subgraphs.
*   **Complexity**: Introduces advanced mathematical concepts (orbits, stress tensors) into the database API.
*   **Experimental Stability**: As experimental features, the APIs for these engines are subject to change.
