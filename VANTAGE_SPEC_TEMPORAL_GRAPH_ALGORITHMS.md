# 🔭 Vantage: Spec for Temporal Graph Algorithms (Shortest Path Over Time)

## 👤 User Story
**As a** Logistics Analyst or Network Engineer,
**I want** to execute graph algorithms (like Shortest Path) across specific historical time windows or point-in-time snapshots,
**so that** I can analyze historical network topologies, supply chain bottlenecks, or financial routing paths as they existed in the past, without interference from current-state data.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
AletheiaDB currently excels at structural and semantic traversal for a given point in time or across a history of a single entity. However, complex graph algorithms (like A*, Dijkstra, or PageRank) currently only operate efficiently on the current state. Users analyzing historical systemic failures (e.g., "Why did the packet drop on Tuesday?", or "What was the shortest supply chain route in Q1 2023?") are forced to manually traverse and reconstruct paths, which is slow and error-prone. Natively supporting temporal algorithms unlocks enterprise use cases in compliance, logistics, and network forensics.

**Success Metric Definition:**
- **Performance:** Executing a point-in-time Shortest Path query (A*) across a 5-hop distance on a graph with 1 million historical versions completes in < 50ms.
- **Accuracy:** The algorithm correctly ignores edges or nodes that were deleted or not yet created at the queried `Timestamp`.

## ✅ Acceptance Criteria
- Must expose a temporal pathfinding API (e.g., `db.as_of(time).shortest_path(source, target)`).
- Must support at least A* and Dijkstra algorithms adapted for temporal traversal.
- Must accurately respect the valid time and transaction time of all nodes and edges traversed, projecting the graph to the requested temporal state.
- Must return the ordered sequence of `EntityResult` representing the shortest path, or `None` if no path existed at that time.
- Must leverage existing temporal snapshot caching (Warm Tier) to avoid re-reading disk blocks for repeatedly accessed historical paths.

## 🚫 Out of Scope
- **Time-Dependent Shortest Path (TDSP):** Algorithms where the *travel time* along an edge changes dynamically during the traversal itself (e.g., traffic routing where entering edge A takes 5 mins, so edge B is evaluated at `T + 5`). Phase 1 strictly focuses on static "Snapshot" algorithms (the whole graph is evaluated at a fixed `Time T`).
- Distributed execution of algorithms across sharded clusters (MVP is single-node).
- Algorithms other than Pathfinding (e.g., historical PageRank or Community Detection are deferred to Phase 2).
