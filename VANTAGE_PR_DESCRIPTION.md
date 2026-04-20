# 🔭 Vantage: Spec for Chimera Sub-Graph Extraction

👤 **User Story:** "As a Data Scientist using AletheiaDB, I want to extract a sub-graph representing the full lineage and context of a synthesized 'Chimera' node, so that I can independently analyze, visualize, and export the entire context that led to its creation without being distracted by the rest of the massive graph."

✅ **Acceptance Criteria:**
- Must preserve context: the output must include the Chimera node itself, all of its immediate parent nodes (the nodes that were merged to create it), and all edges connecting the parents to the Chimera.
- Must return a lightweight, serializable representation of the sub-graph (e.g., a standard AletheiaDB `Graph` or a specific `SubGraph` struct) that can be exported.
- Must avoid full graph scans, instead leveraging existing index and adjacency list lookups for performance.

🚫 **Out of Scope:**
- Recursive depth tracing (only immediate parents).
- Interactive visualization UI.
- Exporting directly to external graph databases (e.g., Neo4j).
