# ADR-0058: Entanglement Quantum Entanglement Detection

**Status:** Proposed
**Date:** 2026-06-25
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Semantic Analysis

## Context

In complex systems like social networks, markets, or AI knowledge bases, some nodes may act in synchronized ways even if they are not structurally connected (i.e., they have no edges linking them). This phenomenon resembles "spooky action at a distance" or quantum entanglement.

We need a mechanism to detect and measure these synchronous semantic movements across the graph over time. This enables AI agents to identify hidden correlations, external influences acting simultaneously on multiple nodes, or "botnet" style behaviors where disjoint nodes pivot concepts in unison.

## Decision

We will implement the **Entanglement Detector** as an experimental module in `src/experimental/entanglement.rs`.

The `Entanglement Detector` identifies pairs of nodes whose semantic vectors change synchronously over time, even without direct edges. It operates by:
1.  **Measuring Vector Deltas**: Observing how a node's semantic vector changes across transactions.
2.  **Correlating Deltas**: Comparing the vector deltas of two nodes grouped by transaction time.
3.  **Entanglement Score**: Outputting an Entanglement Score, reflecting how closely correlated the *changes* in the two nodes' vectors are over their history.

## Consequences

### Positive
-   **Hidden Correlation Discovery**: Exposes underlying connections or shared external influences that are missing from the graph's explicit topology.
-   **Anomaly Detection**: Helps in identifying coordinated behavior (like bot networks, coordinated market actions).
-   **Semantic Insights**: Offers a dynamic perspective on node relationships beyond static similarity, based entirely on movement over time.

### Negative
-   **Computational Overhead**: Analyzing the history of multiple nodes simultaneously and correlating their vector deltas is expensive.
-   **Coincidence vs. Causality**: High entanglement scores might result from mere coincidence rather than a genuine causal or correlated relationship. It is crucial to use a sufficiently long history to filter out noise.

## References
- `src/experimental/entanglement.rs`
