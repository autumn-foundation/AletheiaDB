# ADR-0055: Fossil Semantic Stagnation Detector

**Status:** Proposed
**Date:** 2026-06-25
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Semantic Analysis

## Context

In an evolving knowledge graph where vectors represent the semantic meaning of nodes over time, it's critical to understand not just what changes, but what *fails* to change. Nodes that remain stagnant while their surrounding context shifts may indicate outdated information, stubbornly held beliefs, or "dead" concepts.

We need a way to detect these "Fossils": entities whose semantic vector is static (low displacement) while the average semantic vector of their structural neighbors changes significantly (high context displacement).

## Decision

We will implement **FossilDetector**, an experimental engine in `src/experimental/fossil.rs`.

The `FossilDetector` will:
1.  Compute **Node Displacement**: The Euclidean distance a node's vector moves over a given time window.
2.  Compute **Context Displacement**: The average Euclidean distance its neighbors' vectors move over the same window.
3.  Calculate the **Fossil Index**: A ratio of Context Displacement to Node Displacement.

Nodes with a high Fossil Index are flagged as "Fossils", representing semantic stagnation relative to their environment.

## Consequences

### Positive
-   **Data Hygiene**: Automatically identifies potentially outdated or irrelevant nodes that require manual review or automated deprecation.
-   **Agentic Insight**: AI agents can detect when they (or other entities) are falling behind the evolving consensus of their network.
-   **Temporal Analysis**: Adds a new dimension to temporal graph analysis by contrasting local stagnation with global movement.

### Negative
-   **Computational Cost**: Calculating the Fossil Index requires querying historical vectors for a node and all its neighbors, which can be expensive on highly connected graphs.
-   **False Positives**: A node might be mathematically stagnant simply because it represents a foundational, unchanging truth (e.g., "The speed of light"), even if the discussion around it evolves.

## References
- `src/experimental/fossil.rs`
