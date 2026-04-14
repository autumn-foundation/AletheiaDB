# ADR-0052: Chimera Hybrid Entity Synthesis Engine

**Status:** Proposed
**Date:** 2026-06-01
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Data Synthesis

## Context

AletheiaDB's cognitive architecture aims to support high-level reasoning capabilities for AI agents. One key requirement is the ability to generate **hypothetical scenarios** or **synthetic entities** that do not exist in the raw data but are plausible combinations of existing knowledge.

For example:
-   **Scenario Planning:** "What if we merged Department A and Department B?"
-   **Product Ideation:** "Imagine a product with the features of X and the reliability of Y."
-   **Data Augmentation:** generating synthetic training data for downstream models.

Currently, the database supports storing and retrieving existing entities, but lacks a mechanism to programmatically synthesize new entities that inherit properties and relationships from multiple parents in a controlled manner.

## Decision

We will implement **Chimera**, a Hybrid Entity Synthesis Engine.

Chimera allows users to generate a new node (a "Chimera") that is the semantic and structural fusion of two existing nodes. It employs configurable strategies to blend their properties and automatically inherits their relationships.

### Architecture

Chimera is implemented as an experimental module (`src/experimental/chimera.rs`) gated by the `nova` feature.

It provides a `ChimeraEngine` that accepts two source nodes (`NodeId`) and a `SynthesisConfig`.

**Core Capabilities:**
1.  **Property Blending:** Merges properties from both parents using strategies like:
    -   `Mean` / `Sum` (for numeric values)
    -   `Min` / `Max` (for comparable values)
    -   `Concatenate` (for strings)
    -   `Lerp` (Linear Interpolation for vectors, controlled by an alpha parameter)
    -   `KeepA` / `KeepB` (preference-based selection)
2.  **Structural Inheritance:** The new Chimera node inherits incoming and outgoing edges from both parents, effectively placing it in the graph as if it were both entities simultaneously.
3.  **Vector Synthesis:** Special handling for vector embeddings to place the new entity at a semantic midpoint (or weighted point) between parents.

## Consequences

### Positive

-   **Hypothetical Reasoning:** Enables "what-if" analysis directly within the database without external processing.
-   **Semantic Interpolation:** Allows exploring the "latent space" between concepts by creating intermediate nodes.
-   **Graph Connectivity:** Automatically preserves the structural context of the merged entities, making the new node immediately useful for traversals.

### Negative

-   **Semantic Drift:** Merging incompatible concepts (e.g., "Apple" the fruit and "Apple" the company) may result in nonsensical properties or relationships.
-   **Graph Explosion:** Overuse of synthesis can clutter the graph with hypothetical nodes, potentially degrading performance if not managed (e.g., by using a temporary transaction or specific "hypothetical" labels).

### Neutral

-   **Configuration Complexity:** Users must carefully choose merge strategies for different property keys to get meaningful results.

## References

-   `src/experimental/chimera.rs`
