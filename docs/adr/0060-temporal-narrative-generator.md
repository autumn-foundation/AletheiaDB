# ADR-0060: Temporal Narrative Generator

**Status:** Proposed
**Date:** 2026-06-25
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, LLM Integration

## Context

When providing context to an LLM or an end-user, raw bi-temporal data (versions, transaction times, structural property diffs) is often dense, unreadable, and token-inefficient. AI agents require the history of an entity to be summarized in a human-readable "narrative" or log that focuses on the significant changes and events in the entity's lifecycle.

We need a mechanism to translate AletheiaDB's raw version history for a node into a coherent, natural language-like sequence of events.

## Decision

We will implement the **Temporal Narrative Generator** ("Bard") as an experimental module in `src/experimental/temporal_narrative.rs`.

The `Temporal Narrative Generator` processes a node's `VersionInfo` history to emit a sequence of `NarrativeEvent` objects. Each event contains:
1.  **Timestamp**: The ISO 8601 transaction time.
2.  **Version**: The sequential version number.
3.  **Description**: A high-level, human-readable description of what happened (e.g., "Property 'status' changed from 'Active' to 'Inactive'", "Edge 'KNOWS' added to User 123").

## Consequences

### Positive
-   **LLM Context Efficiency**: Dramatically reduces the token footprint required to communicate an entity's history to an LLM, filtering out low-level database artifacts.
-   **Explainability**: Generates an audit trail or history log that is immediately readable by humans without the need for complex query tools.
-   **Event Aggregation**: Provides a building block for higher-level reasoning, abstracting away the low-level diff logic into discrete events.

### Negative
-   **Loss of Fidelity**: Abstracting complex graph diffs into short narrative strings necessarily drops granular details. It is not suitable for exact data replication.
-   **Static Language Generation**: The "narrative" generated is currently hardcoded formatting rules, which may not always capture the true domain-specific semantic importance of a change.

## References
- `src/experimental/temporal_narrative.rs`
