# ADR-0043: Cognitive Architecture Components

**Status:** Accepted
**Date:** 2026-03-15
**Deciders:** AletheiaDB Core Team, Codex
**Categories:** architecture, cognitive-services, experimental

## Context

AletheiaDB has evolved from a pure bi-temporal graph database into a system capable of advanced reasoning and simulation. Several experimental modules have been introduced to support these capabilities:

1.  **Ariadne** (Semantic Thread Weaver): Narrative pathfinding.
2.  **Prophet** (Link Prediction): Topological + Semantic prediction.
3.  **Fishing** (Associative Retrieval): Vector-based graph expansion.
4.  **Kaleidoscope** (Semantic Layout): Force-directed visualization.
5.  **SemanticNavigator** (A* Pathfinding): Heuristic traversal.
6.  **Sentinel** (Semantic Firewall): Data validation.
7.  **Sybil** (Memetic Propagation): Influence simulation.
8.  **TemporalDiff** (Diff Engine): Structural/Semantic comparison.
9.  **NarrativeGenerator** (Natural Language History): Story generation.

These components currently reside in `src/experimental` but lack formal architectural documentation, making the system difficult to understand as a whole ("A system without a diagram is a maze").

## Decision

We will formalize these components as the **Cognitive Layer** of AletheiaDB. This layer sits above the Core Query Engine and provides specialized reasoning services that combine graph topology, vector semantics, and temporal history.

Each component will be documented with Mermaid diagrams in `docs/ARCHITECTURE.md` to ensure transparency.

## Consequences

### Positive

-   **Clarity:** Developers and AI agents can understand the purpose and interaction of these advanced modules.
-   **Structure:** Groups related "reasoning" features together, paving the way for a potential `aletheiadb-cognitive` crate.
-   **Discoverability:** Features like "Sentinel" (validation) and "Sybil" (simulation) become visible parts of the toolkit rather than hidden experimental code.

### Negative

-   **Maintenance:** More documentation to keep in sync with code changes.
-   **Complexity:** The architectural surface area increases.

## Component Overview

| Component | Metaphor | Function |
| :--- | :--- | :--- |
| **Ariadne** | Thread Weaver | Connects events into coherent narratives. |
| **Prophet** | Oracle | Predicts missing links. |
| **Fishing** | Fishing Rod | Retrieves related concepts (associative memory). |
| **Kaleidoscope** | Lens | Visualizes semantic structure (2D layout). |
| **SemanticNavigator** | Compass | Finds semantically smooth paths. |
| **Sentinel** | Shield | Validates data against semantic rules. |
| **Sybil** | Virus/Meme | Simulates information propagation. |
| **TemporalDiff** | Time Machine | Compares graph states across time. |
| **NarrativeGenerator** | Storyteller | Converts history to natural language. |
