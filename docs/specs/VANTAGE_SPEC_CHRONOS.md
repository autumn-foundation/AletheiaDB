# 🔭 Vantage: Spec for Chronos (Temporal Pathfinding)

## 👤 User Story
**As a** Cybersecurity Analyst investigating a breach or an Epidemiologist tracking a contagion,
**I want** to find the shortest path between two nodes that respects the strict chronological order of events,
**so that** I can accurately trace how information, funds, or infections spread through a network over time, rather than finding mathematically impossible paths based on static network topology.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Standard graph pathfinding algorithms (like Dijkstra or BFS) find the shortest path in a static network topology. However, in reality, relationships and events occur at specific times. A static path from A -> B -> C might exist structurally, but if the edge A->B occurred *after* the edge B->C, nothing could have actually flowed from A to C. Chronos solves the "Impossible Path" problem. By enforcing chronological validity (time-respecting paths), it prevents false positives in investigations, drastically reducing the time analysts spend chasing dead ends and improving the accuracy of root-cause analysis in forensic scenarios.

**Success Metric Definition:**
- **Path Accuracy:** Chronos guarantees 100% chronological validity (edge $E_i$ must have a valid time $\le$ edge $E_{i+1}$) for all returned paths.
- **Query Latency:** Finding the shortest time-respecting path up to 5 hops deep in a graph of 10 million temporal edges completes in <100ms.

## ✅ Acceptance Criteria
- Must define an API endpoint or query language extension for Chronological Pathfinding (e.g., `find_temporal_path(start_node, end_node, start_time, end_time)`).
- Must traverse the graph ensuring that each subsequent hop along a path occurs sequentially in Valid Time.
- Must return the path (nodes and edges) along with the temporal metadata that validates the sequence.
- Must handle cases where no time-respecting path exists gracefully, returning an empty result rather than an error.

## 🚫 Out of Scope
- Predicting future paths based on historical trends (Phase 2).
- Complex path constraints beyond strictly increasing time (e.g., must flow through a specific intermediary type within a 2-day window—this is closer to Sherlock's domain).
