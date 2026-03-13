# ADR-0059: Aura Semantic Essence Over Time

**Status:** Proposed
**Date:** 2026-06-25
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Semantic Analysis

## Context

In an intelligent graph, a node's semantic vector represents its current state or thought process. However, this instantaneous state doesn't capture the node's long-term identity or underlying nature. For an AI agent interacting over time, tracking whether an entity's current behavior deviates from its historical essence is a crucial requirement.

We need a mechanism to quantify a node's long-term semantic essence and measure how far its current state diverges from that core identity.

## Decision

We will implement the **Aura Engine** as an experimental module in `src/experimental/aura.rs`.

The `Aura` engine calculates the "Aura" of a node, defined as an exponentially time-weighted average of its semantic vector over its entire history. Recent states hold more weight, but deep history still exerts a measurable gravitational pull. It outputs:
1.  **Aura Vector**: The node's historical, time-weighted average semantic state.
2.  **Divergence**: The distance (Euclidean or Cosine) between the node's current vector and its Aura.

## Consequences

### Positive
-   **Identity Tracking**: Establishes a baseline "identity" for a node over time, smoothing out short-term fluctuations or noise.
-   **Anomaly Detection**: High divergence alerts the system to a node acting "out of character", such as an AI agent being hijacked or a core concept shifting radically.
-   **Predictive Baseline**: Provides a more stable semantic anchor for predicting the node's future behavior or relevance.

### Negative
-   **Memory & Computation**: Re-calculating the Aura requires iterating through all historical versions of a node's vector and applying an exponential decay function.
-   **Parameter Tuning**: The choice of the decay factor (half-life) significantly alters what the Aura represents (short-term vs. long-term essence), requiring careful tuning for specific domains.

## References
- `src/experimental/aura.rs`
