## Semantic Pathfinding
**Concept:** A pathfinding algorithm (A*) that uses vector similarity as the heuristic cost.
**Fate:** Merged (Experimental)
**Lesson:**
- Connecting graph topology with vector embeddings creates powerful "conceptual navigation".
- Time-travel pathfinding is tricky without a historical adjacency index (deleted edges are lost to `get_outgoing_edges`), highlighting a future architectural need.
- `GallifreyDB`'s modular design made it easy to hook into `get_node` and `get_edge`.
