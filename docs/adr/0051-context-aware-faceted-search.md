# ADR-0051: Context-Aware Faceted Search (Chameleon)

**Status:** Accepted
**Date:** 2024-05-24
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Search

## Context

In a graph database, nodes often exhibit polysemy—multiple meanings depending on the context. For example, a node labeled "Apple" might be connected to "iPhone" (Technology context) and "Banana" (Fruit context).

Standard vector search typically relies on a single embedding vector for the node. If this vector is an average of its diverse neighbors or trained on mixed contexts, it lands in a "muddled middle-ground" in the vector space, potentially retrieving irrelevant results for specific queries (e.g., searching for "tech companies" might return "Orange" due to the fruit association).

We need a way to disentangle these meanings to allow for precise, context-specific exploration.

## Decision

We will implement **Chameleon**, a Context-Aware Faceted Search engine.

Chameleon analyzes the local neighborhood of a node (its 1-hop neighbors) and applies unsupervised clustering (specifically **MiniKMeans**) to their vector embeddings. This process identifies distinct clusters, which we call **Aspects**.

Each Aspect consists of:
1.  **Centroid**: The geometric center of the cluster in vector space.
2.  **Weight**: The proportion of neighbors belonging to this cluster.
3.  **Exemplars**: Representative nodes closest to the centroid.

These Aspects can then be used as query vectors for global searches. This effectively allows users to say: "Find nodes similar to the 'Technology' aspect of Apple."

## Consequences

### Positive

-   **Disentangled Search**: Enables high-precision queries by isolating specific semantic contexts.
-   **Exploratory Power**: Users can "pivot" around a node's different meanings, discovering hidden dimensions in the data.
-   **No Retraining**: Works on existing embeddings by leveraging graph topology and neighbor vectors.

### Negative

-   **Computational Overhead**: Requires fetching neighbor vectors and running K-Means clustering at query time (or analyzing beforehand).
-   **Dependency on Density**: Isolated nodes or nodes with few neighbors cannot produce meaningful clusters.
-   **Parameter Sensitivity**: The number of clusters (*k*) must be chosen carefully (or estimated).

### Neutral

-   **Algorithm Choice**: We use `MiniKMeans` for performance, which is an approximation but sufficient for this use case.

## References

-   `src/experimental/chameleon.rs`
