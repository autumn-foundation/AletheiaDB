# ADR-0038: Counterfactual Graph Analysis (Hindsight)

**Status:** Accepted
**Date:** 2026-05-25
**Deciders:** AletheiaDB Core Team
**Categories:** experimental, reasoning, simulation

## Context

Reasoning systems, particularly LLMs (Large Language Models), often need to explore hypothetical scenarios to make decisions.
A common pattern is: "If X were true, what would be the impact on Y?"

In a graph database, this translates to:
*   "If I add a relationship between A and B, does it create a cycle?"
*   "If I remove this node, is the network still connected?"
*   "If this node had a different vector embedding, would it be retrieved by this query?"

Currently, to answer these questions, a user must either:
1.  Clone the entire database (slow, expensive).
2.  Mutate the production database and then rollback (risky, concurrency issues).
3.  Implement the logic in application code (complex, duplicates DB logic).

We need a way to run queries against a "Virtual Graph" that is a *superposition* of the real database and a set of hypothetical changes.

## Decision

We will implement **Counterfactual Graph Analysis** (codenamed "Hindsight") in `src/experimental/hindsight.rs`.

Hindsight provides a lightweight, in-memory **Overlay** on top of `AletheiaDB`.
It introduces a `Scenario` object that records:
1.  **Added Nodes/Edges**: Entities that exist only in the simulation.
2.  **Removed Nodes/Edges**: Entities "tombstoned" in the simulation.
3.  **Modified Properties**: Patches applied to existing entities.

The `Hindsight` engine intercepts read operations (get, traverse, search) and:
1.  Checks the `Scenario`.
2.  If not found/shadowed, falls back to the underlying `AletheiaDB`.

## Consequences

### Positive

*   **Safe Simulation**: Zero risk of corrupting production data. "What If" scenarios are purely ephemeral.
*   **Agentic Reasoning**: Enables AI agents to "think before they act" by simulating the consequences of their proposed changes.
*   **Performance**: Since the overlay is in-memory and typically small (a few dozen changes), overhead is negligible compared to cloning.

### Negative

*   **Memory Bound**: The `Scenario` lives entirely in RAM. This approach is not suitable for "What if we doubled the dataset?" scenarios, only for local perturbations.
*   **Implementation Complexity**: Every query path (Get, Traverse, Vector Search) must be wrapped to respect the overlay logic. Currently, only basic BFS and Vector Search are supported.
*   **Staleness**: The simulation is based on the DB state at the moment of access. If the underlying DB changes significantly, the simulation might become invalid (though MVCC snapshots mitigate this).

## Implementation Details

*   **Location**: `src/experimental/hindsight.rs`
*   **Scenario**: A struct containing `HashMap<NodeId, Node>` (added), `HashSet<NodeId>` (removed), etc.
*   **Engine**: `Hindsight<'a>` wraps `&'a AletheiaDB` and `Scenario`.
*   **Vector Search**: Implements a hybrid search that merges DB results (filtered for removals) with scenario-added vectors.
