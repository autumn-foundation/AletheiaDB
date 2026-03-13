# ADR-0065: Sybil Memetic Propagation Engine

**Status:** Proposed
**Date:** 2026-06-25
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Simulation

## Context

Graph databases excel at answering questions about static topology ("Who is connected to whom?") and vector databases answer questions about static similarity ("What is most like this?"). However, they struggle to model dynamic processes over the graph topology, such as the spread of information, influence, or "memes" (represented as vectors).

For example, "If a rumor starts at Node A, how far does it spread? Who believes it after 10 time steps given the resistance (inertia) of intermediate nodes?"

We need a mechanism to simulate the propagation of vector states across the graph's structural edges to answer these "what-if" dynamic questions.

## Decision

We will implement the **Sybil Memetic Propagation Engine** as an experimental module in `src/experimental/sybil.rs`.

The `Sybil` engine simulates how a "meme" (a semantic vector representing an idea, belief, or state) propagates through the graph structure. It defines:
1.  **Propagation Model**: Rules determining how a node updates its state based on the states of its incoming neighbors.
2.  **Inertia**: A node's resistance to change or dampening factor (how much it retains its original vector vs. adopting the incoming meme vector).

The simulation runs iteratively, tracking the shifting vectors of nodes over a series of steps.

## Consequences

### Positive
-   **Dynamic Simulation**: Enables predictive modeling of influence spread, disease transmission, or information virality directly within the database engine.
-   **Agentic Strategy**: Agents can test hypotheses about the network (e.g., "Which node is the best seed to propagate this idea?").
-   **Advanced Analytics**: Adds a powerful "System 2" reasoning capability that goes beyond static data retrieval.

### Negative
-   **Computational Cost**: Running simulations across large graph segments iteratively is highly compute-intensive and memory-heavy.
-   **Model Complexity**: Tuning the rules of propagation and inertia to reflect real-world phenomena accurately requires significant domain expertise and calibration.

## References
- `src/experimental/sybil.rs`
