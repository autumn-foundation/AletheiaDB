# ADR-0049: Muse (Semantic Ideation)

**Status:** Accepted
**Date:** 2026-01-27
**Deciders:** AletheiaDB Core Team
**Categories:** experimental, semantic-analysis, ai-reasoning

## Context

Current AI agents primarily retrieve existing information (RAG). However, true "intelligence" involves synthesizing new concepts by connecting disparate ideas or identifying gaps in knowledge (conceptual voids).

We need a mechanism to explore the *latent space* between existing knowledge nodes to:
1.  **Identify Voids**: Find areas in the semantic space that are surprisingly empty given the surrounding concepts.
2.  **Propose Novelty**: Generate "Inspirations" (vectors) representing potential new concepts.

This is critical for creative problem solving and hypothesis generation.

## Decision

We will implement the **Muse** module (`src/experimental/muse.rs`) for Semantic Ideation.

"Innovation is connecting two existing modules that haven't met yet."

### 1. Ideation Strategy (`Muse`)

The core idea is to compute the **Centroid** of a set of input concepts and evaluate its surroundings.

-   **Input**: A list of `NodeId`s (the "seeds" or "parents").
-   **Mechanism**:
    1.  Fetch the embedding vectors of all seed nodes.
    2.  Compute their geometric centroid (average vector, normalized).
    3.  Search the vector index for the nearest neighbors to this centroid.
    4.  Calculate metrics: **Novelty** and **Coherence**.

### 2. Metrics

#### Novelty Score
Measures how "empty" the space around the new concept is.

$$
\text{Novelty} = 1.0 - \max(\text{Similarity}(\text{Centroid}, \text{Nearest Neighbor}))
$$

-   **High Novelty**: The new concept is far from anything else. It fills a void.
-   **Low Novelty**: The space is already crowded. The concept is redundant.

#### Coherence Score
Measures how well the new concept connects the input seeds.

$$
\text{Coherence} = \text{Average}(\text{Similarity}(\text{Centroid}, \text{Seed}_i))
$$

-   **High Coherence**: The seeds are related, and the new concept is a natural synthesis.
-   **Low Coherence**: The seeds are unrelated, and the centroid is a meaningless average.

### 3. Feature Gating

Muse is an experimental feature dependent on advanced vector operations. It is guarded by the `nova` feature flag in `Cargo.toml`.

```rust
#[cfg(feature = "nova")]
pub struct Muse<'a> { ... }
```

## Consequences

### Positive

-   **Generative Capability**: Allows the system to propose *new* knowledge, not just retrieve old.
-   **Creativity Metric**: Provides quantifiable scores for "novelty" and "coherence".
-   **Exploration**: Guides agents towards unexplored areas of the knowledge graph.

### Negative

-   **Embedding Quality**: Heavily dependent on the quality and dimensionality of the embedding model. Poor embeddings yield meaningless centroids.
-   **Interpretability**: The "meaning" of a centroid vector is abstract until mapped to a real concept (e.g., by an LLM generating a label for it).

## Alternatives Considered

### Alternative 1: Random Sampling

Pick random points in vector space.

-   **Pros**: Simple.
-   **Cons**: Most points in high-dimensional space are meaningless noise. Centroids are more likely to be meaningful intersections.

### Alternative 2: GANs (Generative Adversarial Networks)

Train a GAN on the embedding space.

-   **Pros**: Potentially better samples.
-   **Cons**: Training complexity and resource overhead far exceed a simple centroid calculation.
