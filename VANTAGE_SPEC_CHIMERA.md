# 🔭 Vantage: Spec for Chimera Sub-Graph Extraction

👤 **User Story:** "As a Data Scientist using AletheiaDB, I want to extract a sub-graph representing the full lineage and context of a synthesized 'Chimera' node, so that I can independently analyze, visualize, and export the entire context that led to its creation without being distracted by the rest of the massive graph."

✅ **Acceptance Criteria:**
- **Context Preservation:** Must extract the Chimera node itself, all of its immediate parent nodes (the nodes that were merged to create it), and all edges connecting the parents to the Chimera.
- **Recursive Depth (Optional/Phase 2?):** *Out of scope for this initial spec, keep it to immediate parents/edges for simplicity and performance.*
- **Output Format:** Must return a lightweight, serializable representation of the sub-graph (e.g., a standard AletheiaDB `Graph` or a specific `SubGraph` struct) that can be easily exported to JSON, CSV, or passed to visualization tools.
- **Performance:** Sub-graph extraction must be fast, avoiding full graph scans. It should leverage existing index and adjacency list lookups.

🚫 **Out of Scope:**
- Interactive visualization UI.
- Exporting to external graph databases (e.g., Neo4j).
