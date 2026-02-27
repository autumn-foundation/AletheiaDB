# ADR-0053: Cognitive Dynamics & Probabilistic Reasoning

**Status:** Accepted
**Date:** 2026-06-15
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Reasoning

## Context

The initial "Cognitive Architecture" (ADR-0043) introduced components for narrative generation (`Ariadne`), associative recall (`Telepathy`), and semantic mapping (`Metaphor`). However, advanced agentic workflows require capabilities that go beyond static structural or semantic analysis. Agents need to reason about **Time**, **Causality**, **Probability**, and **Adaptation**.

Specifically, we identified four missing capabilities:
1.  **Causality Detection:** "Did Event A cause Event B?" (Correlation of changes over time).
2.  **Probabilistic Reasoning:** "What is the likelihood of X?" (Monte Carlo simulations on the graph).
3.  **Event Summarization:** "What significant things happened to this entity?" (Filtering noise from history).
4.  **Adaptive Learning:** "Which paths are most useful?" (Reinforcement/Hebbian learning based on usage).

These capabilities are essential for "System 2" thinking in AI agents, allowing them to form hypotheses, estimate risks, and learn from experience.

## Decision

We will implement **Cognitive Dynamics**, a suite of experimental engines that introduce probabilistic and temporal reasoning to AletheiaDB.

The components are:

### 1. Ripple (Semantic Causality Detector)
*   **Goal:** Identify causal relationships between nodes by analyzing time-lagged correlations of their "Semantic Flux" (rate of vector change).
*   **Mechanism:**
    *   Computes a time series of vector changes (`Flux = |V_t - V_{t-1}|`) for source and target nodes.
    *   Performs a cross-correlation analysis to find the optimal time lag where correlation is maximized.
    *   Output: `RippleEffect { lag, correlation, confidence }`.

### 2. Oracle (Probabilistic Graph Reasoning)
*   **Goal:** Answer probabilistic questions about graph topology using Monte Carlo simulations.
*   **Mechanism:**
    *   **Personalized PageRank (PPR):** Estimates node relevance/influence relative to a seed set using random walks with restart.
    *   **Reachability:** Estimates the probability `P(Source -> Target)` within `k` steps by running `N` random simulations.
*   **Why:** Exact computation of these metrics on large, dynamic graphs is expensive; approximation is sufficient for agent reasoning.

### 3. Kairos (Semantic Event Detection)
*   **Goal:** Extract a timeline of "Significant Events" from a node's verbose history.
*   **Mechanism:**
    *   Scans the version history of a node.
    *   Filters out minor updates (noise).
    *   Emits a `TimelineEvent` only if:
        *   **Vector Shift:** The semantic distance from the last significant event > `threshold`.
        *   **Structural Change:** Key properties or edges were added/removed.
*   **Benefit:** Reduces context window usage for LLMs by 90%+.

### 4. Synapse (Adaptive Graph Hebbian Learning)
*   **Goal:** Enable the graph to "learn" and optimize traversal paths based on usage.
*   **Mechanism:**
    *   **Hebbian Learning:** "Cells that fire together, wire together." Incrementing an edge's weight when it is successfully traversed.
    *   **Forgetting:** Applying a decay factor to all weights over time.
    *   **Adaptive Pathfinding:** A modified A* algorithm that balances **Semantic Cost** (1 - Similarity) with **Synaptic Weight** (Popularity).
    *   `Cost = SemanticCost * (1.0 / (1.0 + log2(1 + Usage)))`

## Consequences

### Positive

*   **Higher-Order Reasoning:** Agents can now ask "Why?" (Ripple), "What if?" (Oracle), "What happened?" (Kairos), and "How do I get there efficiently?" (Synapse).
*   **Dynamic Adaptation:** The database performance and relevance improve over time as `Synapse` learns common paths.
*   **Noise Reduction:** `Kairos` allows agents to focus on signal rather than noise in high-frequency data.

### Negative

*   **Computational Cost:**
    *   `Ripple` requires O(History) scans and signal processing.
    *   `Oracle` is CPU-intensive due to random walks.
*   **Probabilistic Nature:** Results from `Oracle` and `Ripple` are estimates, not guarantees. This must be communicated clearly in the API.
*   **State Management:** `Synapse` introduces mutable state (weights) that must be persisted and managed (decayed) separately from the immutable graph history.

## References

-   `src/experimental/ripple.rs`
-   `src/experimental/oracle.rs`
-   `src/experimental/kairos.rs`
-   `src/experimental/synapse.rs`
