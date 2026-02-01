## Semantic Pathfinding
**Concept:** A pathfinding algorithm (A*) that uses vector similarity as the heuristic cost.
**Fate:** Merged (Experimental)
**Lesson:**
- Connecting graph topology with vector embeddings creates powerful "conceptual navigation".
- Time-travel pathfinding is tricky without a historical adjacency index (deleted edges are lost to `get_outgoing_edges`), highlighting a future architectural need.
- `GallifreyDB`'s modular design made it easy to hook into `get_node` and `get_edge`.

## GraphContext (Temporal Subgraph Export)
**Concept:** A feature to extract a temporal subgraph around a node and serialize it into LLM-friendly formats (Markdown).
**Fate:** Merged (Experimental)
**Lesson:**
- Providing "context" to LLMs requires bridging the gap between graph structure and textual representation.
- The "Hybrid Traversal" approach (current topology + historical properties) is a pragmatic workaround for the lack of a temporal adjacency index, though it misses deleted relationships.
- `InternedString` handling in formatting requires careful resolution via `GLOBAL_INTERNER`.
