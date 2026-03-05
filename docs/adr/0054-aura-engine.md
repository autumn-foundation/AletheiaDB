# ADR-0054: Aura Engine (Semantic Essence over Time)

**Status:** Accepted
**Date:** 2026-06-16
**Deciders:** Nova, Codex
**Categories:** Experimental, Cognitive Architecture, Reasoning, Temporal Analysis

## Context

While AletheiaDB provides tools to track how a node's vector changes over time (like Tremor or Kairos), we lacked a mechanism to quantify a node's *fundamental identity* across its entire lifespan. A node's immediate semantic vector represents what it is thinking or doing right now, which is susceptible to short-term volatility or noise.

For example, if an LLM agent gets hijacked or experiences a transient context shift, its immediate vector changes drastically. However, its deep history—its core essence—should remain relatively stable. We needed a way to calculate this enduring identity and measure when a node is acting "out of character".

## Decision

We have implemented the **Aura Engine** (`AuraEngine` in `src/experimental/aura.rs`) as part of the experimental `nova` feature set.

The `AuraEngine` introduces the concept of an "Aura"—an exponentially time-weighted average of a node's semantic vector over its entire history.

**Mechanism:**
1.  **Aura Vector:** The engine iterates through the temporal version history of a given node and a specific vector property.
2.  **Exponential Decay:** It applies a time-weighted average using a configurable `half_life_us`. Recent states receive more weight, but the aggregate weight of deep history ensures long-term stability.
3.  **Divergence Score:** The engine calculates the cosine distance (1.0 - similarity) between the computed Aura vector and the node's *current* state vector.

## Consequences

### Positive

*   **Identity Hijack Detection:** Enables the system to flag when an agent or user starts outputting vectors that are highly divergent from their established Aura.
*   **Core Concept Extraction:** Allows the distillation of a noisy concept down to its most stable, enduring semantic meaning by filtering out recent volatility.
*   **Isolation:** The feature is completely isolated in `src/experimental/aura.rs` behind the `nova` feature flag, ensuring no disruption to core database execution logic.

### Negative

*   **Computational Overhead:** Calculating an Aura requires fetching and iterating over the entire version history of a node, which can be expensive for nodes with extensive histories.
*   **Tuning Complexity:** The `half_life_us` parameter requires careful tuning depending on the domain to correctly balance short-term responsiveness with long-term stability.

## References

- `src/experimental/aura.rs`
