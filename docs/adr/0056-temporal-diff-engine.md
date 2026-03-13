# ADR-0056: Temporal Diff Engine

**Status:** Proposed
**Date:** 2026-06-25
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Temporal Analysis

## Context

A core value proposition of AletheiaDB's bi-temporal architecture is the ability to query historical data ("Time Travel"). However, agents often need to understand the *differences* between two points in time, rather than just querying the state at two distinct timestamps.

We need a mechanism to efficiently compute the structural and semantic delta (diff) between two historical snapshots. This allows an AI agent to quickly identify what changed, was added, or was removed within a given time range.

## Decision

We will implement the **Temporal Diff Engine** as an experimental module in `src/experimental/temporal_diff.rs`.

The `TemporalDiffEngine` provides tools to generate a `DiffReport` that identifies changes between a baseline timestamp (`t1`) and a comparison timestamp (`t2`). This report highlights:
-   **Node Changes**: New nodes created, nodes deleted, or properties updated.
-   **Edge Changes**: New relationships formed or existing relationships broken.
-   **Semantic Vector Shifts**: Nodes whose semantic embeddings have moved significantly.

## Consequences

### Positive
-   **Efficient Insights**: Provides a structured summary of changes over time, saving agents from manually comparing full graph snapshots.
-   **Auditability**: Enables easier tracking of historical events or regressions in knowledge graphs.
-   **Data Synchronization**: Can serve as a primitive for syncing AletheiaDB data with external systems.

### Negative
-   **Compute Intensive**: Depending on the size of the graph and the time window, comparing two snapshots can involve substantial computation and memory overhead.
-   **Complexity in Diffing Properties**: Deciding what constitutes a "significant" property or semantic change may require tuning thresholds.

## References
- `src/experimental/temporal_diff.rs`
