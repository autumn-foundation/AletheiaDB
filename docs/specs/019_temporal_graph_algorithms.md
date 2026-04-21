# 🔭 Vantage: Spec for Temporal Graph Algorithms

👤 **User Story:**
As a Data Scientist analyzing financial networks, I want to run pathfinding algorithms (like Shortest Path) that respect the temporal validity of edges, so that I can discover routes of influence or asset transfers that actually existed in chronological order, preventing me from finding paths that are historically impossible (e.g., traversing an edge that was created after the next edge in the path).

✅ **Acceptance Criteria:**
- Must provide an algorithm (e.g., Temporal BFS) that ensures edge `e_{i+1}` is valid at or after the timestamp of edge `e_i`.
- Must expose a Rust API for temporal shortest path operations.
- Pathfinding latency on a 1M node / 10M edge graph should complete in `< 50ms` for paths up to depth 5.

🚫 **Out of Scope:**
- Distributed execution across a sharded cluster.
- Complex centrality algorithms (PageRank, Betweenness) over time.
