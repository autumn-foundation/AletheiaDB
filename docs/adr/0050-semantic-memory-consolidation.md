# ADR-0050: Semantic Memory Consolidation (Mnemosyne)

**Status:** Accepted
**Date:** 2024-05-24
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture

## Context

AletheiaDB stores complete histories of all entities. While crucial for audit trails and time-travel queries, this raw history is often too verbose for direct consumption by Large Language Models (LLMs).

An LLM trying to understand "how a user's interests evolved" might be fed 1,000 versions of a user node, where 900 of them are trivial updates (e.g., login timestamp bumps) or minor vector shifts. This consumes massive context window space with low-signal data.

We need a way to compress this history into a narrative form that preserves *meaning* (semantic shifts) while discarding noise. Simple delta compression (based on property values) is insufficient because it doesn't capture semantic drift—the slow accumulation of small changes that eventually result in a new "concept".

## Decision

We will implement **Mnemosyne**, a Semantic Memory Consolidation engine.

Mnemosyne scans the history of a node and retains a subset of versions called "Key Frames". A version is kept if it meets one of the following criteria:

1.  **Initial State**: The first version is always kept.
2.  **Semantic Drift**: The vector distance between the current version and the last *kept* Key Frame exceeds a configurable `threshold`.
    - This allows us to capture significant shifts in meaning even if they happened gradually over many small updates.
3.  **Structural Change**: Non-vector properties have been added, removed, or modified significantly.

The result is a compressed timeline of `MemoryFrame` objects, each annotated with a `reason` for its retention (e.g., "Vector Shift: 0.8").

## Consequences

### Positive

-   **Context Efficiency**: Reduces historical data volume by 90-99% for LLM consumption, fitting entire entity lifecycles into standard context windows.
-   **Noise Reduction**: Filters out operational churn (e.g., counters, timestamps) that doesn't affect semantic meaning.
-   **Narrative Clarity**: Provides a clear "story arc" of an entity, highlighting only the pivotal moments of change.

### Negative

-   **Lossy Compression**: Subtle changes below the threshold are discarded. An LLM might miss a nuance if the threshold is set too high.
-   **Threshold Tuning**: Requires selecting an appropriate distance threshold (e.g., 0.5 vs 0.8), which may vary by domain or embedding model.

### Neutral

-   **Computation Cost**: Requires a linear scan of history and distance calculations, but this is done on-demand (read time) or can be cached.

## References

-   `src/experimental/mnemosyne.rs`
