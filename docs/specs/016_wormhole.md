# 🔭 Vantage Spec: Wormhole (Semantic-Structural Gaps)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-016 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/experimental/wormhole.rs` |

## 1. 👤 User Stories

> **As a** Fraud Investigator,
> **I want to** detect "wormholes" (nodes that are highly similar in behavioral/semantic vectors but have no direct structural connections),
> **So that** I can uncover hidden coordinated networks or synthetic identities intentionally avoiding direct transactions.

> **As a** Recommendation System Engineer,
> **I want to** identify products or users that are semantically identical but structurally disconnected (e.g., in different clusters),
> **So that** I can accurately predict and suggest missing links or future relationships to increase engagement.

> **As a** Knowledge Graph Administrator,
> **I want to** run a gap analysis to find entities that should be connected but aren't,
> **So that** I can improve the data quality and completeness of my graph through serendipitous discovery.

## 2. 🧐 The "So What?" (Business Value)

Graph databases traditionally only tell you what *is* connected. However, the most valuable insights often come from what *should* be connected but isn't.

**The Gap:**
- **Incomplete Insight:** Structural paths (e.g., A -> B -> C) only show explicit relationships. If two entities share identical traits but have no path between them, standard graph traversals completely miss them.
- **Latency/Complexity Cost:** Attempting to manually find these missing links requires users to first pull all nodes, cluster them externally (e.g., in Python), and then diff the clusters against the graph edges. This is slow, expensive, and out-of-band.

**ROI:**
- **Proactive Intelligence:** Unlocks "Link Prediction" directly within the database.
- **Anomaly Detection:** Finds "Latent Edges" natively, bringing massive value to Security, e-Commerce, and Intelligence use cases.
- **Competitive Advantage:** Very few existing graph databases combine deep structural traversal with vector similarity natively in a single API pass.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Semantic Similarity Thresholding:**
    - The API MUST allow a user to define a target node (or set of candidate nodes) and retrieve its `k` nearest neighbors in vector space.

2.  **Structural Distance Check:**
    - For each identified semantic neighbor, the system MUST compute the shortest path (BFS) structural distance from the source node.
    - The API MUST accept a `max_hops` parameter.

3.  **Wormhole Identification:**
    - A pair of nodes MUST be classified and returned as a `Wormhole` if they are semantically similar (within the top `k`) BUT their structural distance is either greater than `max_hops` or entirely disconnected (`None`).
    - The result MUST include the Source Node ID, Target Node ID, Semantic Similarity Score, and Structural Distance (`None` if disconnected within bounds).

### Non-Functional Requirements

-   **Performance:** Finding wormholes for a candidate node against a graph of 1 million nodes should complete in <10ms for `k=10` and `max_hops=2`, leveraging the HNSW vector index and highly optimized CSR adjacency traversal.

## 4. 🚫 Out of Scope (Phase 1)

- **Automatic Edge Creation:** The system will only *detect* and return wormholes; it will not automatically insert the missing "latent edges" back into the database.
- **Distributed Shortest Path:** In a sharded setup, deep BFS traversals are deferred to a future phase. Phase 1 focuses on localized structural distance.
- **Dynamic Weight Adjustments:** The `max_hops` check treats all edges as structurally equal (unweighted BFS). Dijkstra's algorithm for weighted shortest-path checks is out of scope.
