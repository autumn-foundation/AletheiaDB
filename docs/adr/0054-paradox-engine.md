# ADR-0054: Paradox Engine

**Status:** Accepted
**Date:** 2026-03-03
**Deciders:** AletheiaDB Experimental Team
**Categories:** experimental, temporal, cognitive-dynamics, vector

## Context

In complex, evolving systems (like an agentic knowledge graph or evolving LLM persona), tracking semantic meaning in isolation from structural relationships often misses crucial insights. For instance, an LLM persona might begin emitting content that is increasingly similar to a new topic like "Security" (Semantic Convergence), while simultaneously ceasing to interact with nodes that actually represent "Security Team" or authoritative security sources (Structural Divergence).

This phenomenon—where semantic meaning drifts towards a concept while structural ties drift away—indicates an anomaly, such as a hallucination or the formation of an echo chamber.

We need a system to mathematically detect and quantify these "Paradoxes" across the bitemporal graph to identify when an entity is "drifting apart while growing closer."

## Decision

We will implement the **Paradox Engine** (`ParadoxDetector`) within the `experimental` module of AletheiaDB.

The Paradox Engine will evaluate a node's evolution between two timestamps (`t1` and `t2`) relative to a target semantic concept by computing:

1.  **Semantic Shift**: The change in cosine similarity between the node's vector and the target concept over time.
2.  **Structural Shift**: The change in the average semantic similarity of the node's structural neighbors to the target concept over time.

A high **Paradox Score** (clamped between -1.0 and 1.0) is generated when these two shifts move in opposite directions (e.g., semantics approach while structure diverges).

## Consequences

### Positive

-   **Anomaly Detection**: Enables the detection of hallucinations, echo chambers, and unsupported semantic drift in evolving knowledge bases.
-   **Temporal Synergy**: Combines both the temporal vector features and the temporal adjacency features of AletheiaDB into a single, high-value cognitive metric.
-   **Isolation**: Built in the `experimental` namespace and gated behind the `nova` feature flag, ensuring no risk to the core engine stability.

### Negative

-   **Performance Overhead**: Calculating structural affinity requires historical graph traversals coupled with historical vector lookups, which can be computationally expensive over large subgraphs.
-   **Complexity**: Introduces a complex heuristic metric that requires tuning (e.g., the multiplier to scale the raw paradox score) to be practically useful.

### Neutral

-   **Fallback Mechanisms**: If the temporal query planner is unavailable or lagging, the implementation currently falls back to checking the current transactional state, which could introduce subtle timing differences if not carefully managed.

## Alternatives Considered

### Alternative 1: External Graph Processing

Export the graph slices at `t1` and `t2` and perform the paradox analysis in an external tool like NetworkX or a Python data science environment.

-   **Why not:** Moving data out of the database defeats the purpose of AletheiaDB's high-performance, embedded temporal design. The computation should be pushed down to where the data lives to utilize the low-latency CSR indexes and temporal vector indexes.

### Alternative 2: Separate Semantic and Structural Queries

Force the user to run two separate queries (one for semantic drift, one for structural drift) and compute the paradox score themselves.

-   **Why not:** The combined calculation is a core "Cognitive Dynamic" that is complex to get right (especially handling edge cases and scaling the raw score). Providing it as a unified engine function ensures correctness and optimizes the internal access paths.

## Implementation Notes

-   The engine uses a scaling factor (`x 4.0`) on the raw multiplied delta to produce a more readable score, as multiplying two fractional deltas typically results in a very small number.
-   The current implementation is located in `src/experimental/paradox.rs`.

## References

-   `src/experimental/paradox.rs`
