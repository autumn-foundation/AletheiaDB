# 🔭 Vantage: Spec for Entanglement Detector

## 👤 User Story
"As an Investigator, I want to identify nodes whose semantic vectors change synchronously over time, so that I can discover hidden correlations or coordinated behavior without relying on explicit edges."

## The "So What?" (Business Value)
Currently, uncovering hidden relationships in large datasets is difficult if direct edges do not exist. Fraud rings, botnets, and highly correlated market assets often act in concert, resulting in synchronized semantic shifts. The Entanglement Detector solves this by treating simultaneous vector deltas as "spooky action at a distance." It allows analysts to detect coordinated campaigns, identify hidden forces acting on multiple nodes, and group entities by behavioral patterns over time, moving beyond simple static similarity.

## ✅ Acceptance Criteria
- Must ingest a list of target `NodeId`s and a specific vector property name.
- Must compute vector deltas between consecutive historical versions for each node.
- Must correctly group and align these deltas by the start of their transaction wallclock time.
- Must calculate the correlation (e.g., cosine similarity) between the normalized delta vectors of node pairs at matching transaction times.
- Must return a list of entangled pairs containing the two nodes and their computed entanglement score, sorted by score in descending order.
- Must ignore nodes or time periods where deltas are zero or missing.
- Must not panic when handling empty histories, missing properties, or missing nodes.

## 📈 Metric Definition
- **Success =** Can compute correlations for 1,000 nodes over 100 historical versions each in `< 500ms`.
- **Accuracy =** Highly synchronized node pairs (same direction of change at same times) must score `> 0.90`.
- **Isolation =** Node updates at different transaction times must not falsely contribute to the entanglement score, ensuring a strict `0.0` contribution for completely asynchronous updates.

## 🔍 Gap Analysis
- **Current State:** AletheiaDB can find similar nodes based on their current vectors (k-NN) or find drift in a single node over time.
- **The Gap:** We cannot find *pairs* of nodes that move together. Existing graph traversals require explicit edges. Standard vector search only looks at static point-in-time similarity, missing dynamic behavioral correlation.

## 🚫 Out of Scope
- Real-time streaming updates (Phase 2).
- Valid time alignment (currently strictly bounded to transaction time to ensure changes happened in the same system-level transaction/tick).
- Automatically scanning the entire database (requires a provided subset of nodes to prevent `O(N^2)` combinatorial explosion).
- Cross-property entanglement (e.g., comparing "embedding" on Node A to "image_vector" on Node B).
