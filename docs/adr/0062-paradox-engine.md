# ADR-0062: Paradox Engine

**Status:** Proposed
**Date:** 2026-06-25
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Semantic Analysis

## Context

In complex knowledge graphs, it's possible for an entity's semantic representation (its vector) and its structural behavior (its edges) to evolve in contradictory ways over time. "Drifting apart while growing closer." An AI agent claiming to be an expert in "Quantum Computing" (high semantic similarity) but interacting with no nodes related to that topic (low structural similarity) is exhibiting paradoxical behavior.

We need a way to detect this "Temporal Semantic-Structural Divergence" to identify inconsistencies, hallucinations, or potentially deceptive actors within the graph.

## Decision

We will implement the **Paradox Engine** as an experimental module in `src/experimental/paradox.rs`.

The `Paradox Engine` identifies entities whose semantic meaning and structural context are moving in opposite directions over time. It measures:
1.  **Semantic Convergence**: When an entity's vector moves *closer* to a target concept.
2.  **Structural Divergence**: When the entity loses edges (or gains distance) to nodes representing that same target concept.

A high "Paradox Score" indicates an anomaly where a node is semantically aligned but structurally estranged from a concept, signaling a potential contradiction.

## Consequences

### Positive
-   **Contradiction Detection**: Automatically surfaces discrepancies between what a node "is" and what it "does," acting as a built-in consistency check.
-   **Hallucination Identification**: In AI applications, helps pinpoint generated content that is topically relevant but lacks grounding in the actual structural data.
-   **Trust Metric**: Provides a quantifiable metric for the reliability or "authenticity" of a node's semantic claims.

### Negative
-   **Computational Complexity**: Requires comparing historical semantic vectors and structural ego-networks across time periods, a computationally expensive operation.
-   **Subjectivity**: The definition of "structural divergence" can be nuanced depending on the domain and graph schema.

## References
- `src/experimental/paradox.rs`
