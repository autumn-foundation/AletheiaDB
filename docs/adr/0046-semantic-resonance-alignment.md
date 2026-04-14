# 46. Semantic Resonance & Alignment (Telepathy & Metaphor)

Date: 2026-03-24

## Status

Proposed

## Context

The "Physics of Meaning" initiative (ADR-0045) introduced engines for analyzing semantic properties of the graph. However, two critical capabilities were identified during the development of advanced agentic workflows that were not covered by the initial set of engines:

1.  **Associative Recall ("Telepathy")**: Agents often need to "think" about a concept and see what else "lights up" in the knowledge graph. This requires a spreading activation mechanism that propagates signals through edges but is modulated by semantic vector similarity (e.g., signal flows strongly between "Apple" and "Tech" if the context is technology, but decays if the context is fruit).
2.  **Structural Analogy ("Metaphor")**: Agents need to map concepts from one domain to another (e.g., "Find the 'Steve Jobs' of the 'Culinary World'"). This requires a subgraph alignment algorithm that considers both topological isomorphism and semantic vector similarity.

These features are currently implemented in `src/experimental/telepathy.rs` and `src/experimental/metaphor.rs` but lack formal architectural definition and documentation.

## Decision

We will formalize `Telepathy` and `Metaphor` as core experimental engines within the AletheiaDB "Cognitive Architecture".

### 1. Telepathy (Semantic Spreading Activation)
*   **Mechanism**: Implements a signal propagation algorithm where edge weight is dynamically calculated as `W = Similarity(Source, Target) * Decay`.
*   **Aggregation**: Uses `MAX` aggregation (instead of `SUM`) for incoming signals to identify the single strongest semantic path and prevent feedback loops in cyclic graphs.
*   **Use Case**: Context expansion, ambiguity resolution, and "associative memory" for LLMs.

### 2. Metaphor (Semantic Graph Alignment)
*   **Mechanism**: Implements a greedy alignment algorithm that iteratively maps nodes between two subgraphs based on a composite score of `VectorSimilarity + StructuralConsistency`.
*   **Propagation**: When a pair `(A, X)` is mapped, it boosts the alignment score of neighbors `(B, Y)` where `A->B` and `X->Y`, enforcing structural consistency.
*   **Use Case**: Analogical reasoning, knowledge migration, and digital twin alignment.

## Consequences

### Positive
*   **Enhanced Reasoning**: Enables LLMs to perform "System 2" thinking tasks (analogy, association) natively within the database.
*   **explainability**: The `Telepathy` activation paths and `Metaphor` mappings provide traceable reasoning chains for why a result was returned.
*   **Efficiency**: Pushes complex graph-vector operations down to the database, avoiding expensive round-trips of raw data to the application layer.

### Negative
*   **Performance Impact**: Both engines are computationally intensive, involving O(N*M) vector similarity calculations and graph traversals. They should be restricted to relatively small subgraphs or "ego networks".
*   **Experimental Status**: As part of the `nova` feature set, these APIs are subject to breaking changes without major version increments.
