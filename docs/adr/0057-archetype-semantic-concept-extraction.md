# ADR-0057: Archetype Semantic Concept Extraction

**Status:** Proposed
**Date:** 2026-06-25
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Semantic Analysis

## Context

In an intelligent graph, groups of nodes often belong to the same category or theme. However, finding the exact defining concept—the "Platonic ideal"—of a cluster of nodes is challenging. AI agents need the ability to extract the core essence of a group of entities, such as a cluster of articles on "machine learning," to better categorize and summarize them.

We need a mechanism to identify the central semantic concept (Archetype) of a collection of vectors and score how purely each node embodies this concept.

## Decision

We will implement the **Archetype Engine** as an experimental module in `src/experimental/archetype.rs`.

The `Archetype` engine calculates the mathematical centroid of a set of input vectors to define the core concept (Archetype). It then scores individual nodes based on their cosine similarity to the computed centroid. This produces:
1.  **Archetype Vector**: The central concept (centroid).
2.  **Purity Scores**: How well each node aligns with the Archetype (where 1.0 is a perfect match and 0.0 is orthogonal).

## Consequences

### Positive
-   **Concept Summarization**: Quickly distills a core theme from a broad group of nodes, aiding in clustering and topic extraction.
-   **Anomaly Detection**: Nodes with low purity scores can be flagged as anomalies or outliers that do not truly fit within their supposed group.
-   **Representative Sampling**: Helps find the most "pure" node in a cluster, which can serve as an archetype or exemplar for that group.

### Negative
-   **Centroid Fallacy**: The centroid of a diverse set of vectors might represent a "mushy" average that doesn't actually exist in the data (the "Platonic ideal" may be a point nobody occupies).
-   **Computational Overhead**: Extracting and averaging vectors requires scanning and reading embeddings for a potentially large group of nodes.

## References
- `src/experimental/archetype.rs`
