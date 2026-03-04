# ADR-0054: Aura Engine - Semantic Essence over Time

**Status:** Proposed
**Date:** 2026-03-04
**Deciders:** Codex, Atlas, Nova
**Categories:** Experimental, Cognitive Architecture, Semantic Analysis

## Context

We have tools to track how a node's vector changes over time (e.g., Semantic Spectroscopy, Semantic Temperature), but we lack a mechanism to quantify who a node *fundamentally is* across its entire lifespan.

For AI agents and long-lived entities, their immediate semantic vector changes constantly based on their current context or task. If an agent is hijacked or begins acting erratically, its immediate vector shifts, but its deep history should theoretically remain stable. We need a way to distill a noisy concept or an entity's history down to its most stable, enduring semantic meaning by filtering out recent volatility.

## Decision

We will implement the **Aura Engine**, an experimental feature (`src/experimental/aura.rs`) gated behind the `nova` feature flag.

The Aura Engine calculates the "Aura" of a node, defined as an **exponentially time-weighted average** of its semantic vector over its entire history.

### Mechanism

1.  **Retrieve History:** The engine fetches the complete version history of a given node.
2.  **Weighted Average:** It iterates through the history, extracting the target vector property. It applies an exponential decay function based on a provided `half_life_us`.
    *   Recent states have more weight.
    *   Deep history still exerts a gravitational pull.
3.  **Divergence Score:** The engine computes the semantic distance (divergence) between the node's calculated Aura and its *current* vector state.

## Consequences

### Positive

*   **Identity Hijack Detection:** Agents or systems can monitor the divergence score. A sudden, massive spike indicates the entity is acting highly "out of character" compared to its established historical baseline.
*   **Core Concept Extraction:** Allows querying for the enduring "essence" of a concept, independent of its latest, potentially noisy, update.
*   **Safe Execution:** The feature is completely isolated in `src/experimental/aura.rs`, does not alter core DB execution logic, and uses safe fallbacks to avoid `f32` panics.

### Negative

*   **Computational Cost:** Calculating the Aura requires retrieving and iterating through the entire history of a node, which can be expensive for nodes with millions of versions.
*   **Parameter Tuning:** The `half_life_us` parameter requires careful tuning depending on the domain to balance the responsiveness to new changes versus the stability of the historical essence.

## References

-   `src/experimental/aura.rs`
-   PR #1711: 🌟 Nova: Aura Engine - Semantic Essence over Time
