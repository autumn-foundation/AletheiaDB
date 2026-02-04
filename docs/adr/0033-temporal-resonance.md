# ADR-0033: Temporal Resonance (Echo)

**Status:** Accepted
**Date:** 2026-05-24
**Deciders:** GallifreyDB Core Team
**Categories:** architecture, experimental, temporal, observability

## Context

GallifreyDB stores the complete history of entities (Bi-temporality). While we can query "What happened at time T?", we lack the ability to query based on **Temporal Patterns**.

Use cases include:
*   **Observability**: "Find all servers that had a CPU spike at the same time as Server A."
*   **Security**: "Identify users with login patterns similar to this compromised account."
*   **Finance**: "Find stocks that correlate with this market event."

This requires a way to "fingerprint" the activity history of a node and compare it with others.

## Decision

We will implement **Temporal Resonance** (codenamed "Echo") in `src/experimental/echo.rs`.

The core components are:
1.  **Temporal Fingerprint**: A normalized vector representation of a node's activity over a time window (e.g., a histogram of updates).
2.  **Resonator**: A strategy for generating fingerprints (e.g., `ActivityDensityResonator`).
3.  **Echo Chamber**: An engine that orchestrates the generation and comparison of fingerprints across nodes.

The system uses **Cosine Similarity** on these temporal fingerprints to find "Resonant" nodes (those with similar histories).

## Consequences

### Positive

*   **Pattern Discovery**: Enables finding correlations in the temporal domain, which is orthogonal to graph structure or semantic similarity.
*   **Observability Power**: extremely powerful for root cause analysis in distributed systems (identifying cascading failures).

### Negative

*   **Performance Cost**: Generating fingerprints requires scanning the history of candidate nodes. This is O(N * H) where N is candidates and H is history length. It is not yet indexed.
*   **Parameter Sensitivity**: The "Resonance" depends heavily on the `window_size` and `resolution` (bin size). Incorrect parameters can lead to false positives (aliasing) or false negatives (mismatch).

## Implementation Details

*   **Location**: `src/experimental/echo.rs`
*   **Fingerprint Format**: `Vec<f32>` (normalized bins).
*   **Algorithm**: Linear scan over candidates (for now). Future versions might index these fingerprints using the Vector Search engine (treating temporal patterns as vectors).
