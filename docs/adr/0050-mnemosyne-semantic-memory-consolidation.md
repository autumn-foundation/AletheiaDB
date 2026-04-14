# 50. Mnemosyne: Semantic Memory Consolidation

Date: 2024-05-22

## Status

Superseded by [ADR 0050: Semantic Memory Consolidation](0050-semantic-memory-consolidation.md)

## Context

AI agents and long-running processes generate vast amounts of state history. Storing every micro-change (e.g., small vector drifts during learning) creates noise and bloats context windows when retrieving history for LLMs. A mechanism is needed to "forget" insignificant intermediate states while preserving semantically significant milestones.

Current systems require manual filtering or complex queries to extract meaningful "Key Frames" from history. This results in:
- **Token Bloat**: LLMs receive redundant context.
- **Noise**: Subtle drifts obscure major shifts.
- **Latency**: Post-processing large history sets is slow.

## Decision

We will implement **Mnemosyne**, a Semantic Memory Consolidation engine, within the `src/experimental` module.

Mnemosyne filters node history based on "Semantic Drift" (vector distance thresholds) and structural changes, producing a compressed sequence of "Key Frames".

The core logic is:
1.  **Always Keep Initial State**: The birth of an entity is always significant.
2.  **Semantic Drift**: Calculate the vector distance between the *last kept frame* and the current version. If `distance > threshold`, keep the current version as a new Key Frame.
3.  **Structural Change**: If non-vector properties change (add/remove/modify), keep the version.
4.  **Consolidation**: Discard all intermediate versions that do not meet the criteria.

## Consequences

### Positive
- **Reduced Token Usage**: Drastically reduces the context window needed for history retrieval.
- **Semantic Clarity**: Highlights "turning points" in an entity's lifecycle, filtering out noise.
- **Agentic Native**: Provides a "memory" function that mimics biological consolidation (forgetting the trivial).

### Negative
- **Loss of Fidelity**: Intermediate states are discarded in the view (though kept in storage). Fine-grained analysis might miss subtle trends.
- **Computational Cost**: Computing consolidation requires vector distance calculations for potentially many historical versions.
- **Heuristic Based**: Relying on a single threshold might over-simplify complex semantic trajectories.
