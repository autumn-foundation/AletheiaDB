## Semantic Pathfinding
**Concept:** A pathfinding algorithm (A*) that uses vector similarity as the heuristic cost.
**Fate:** Merged (Experimental)
**Lesson:**
- Connecting graph topology with vector embeddings creates powerful "conceptual navigation".
- Time-travel pathfinding is tricky without a historical adjacency index (deleted edges are lost to `get_outgoing_edges`), highlighting a future architectural need.
- `GallifreyDB`'s modular design made it easy to hook into `get_node` and `get_edge`.

## Temporal Narrative
**Concept:** A generator that produces natural language logs of a node's history by diffing temporal versions.
**Fate:** Merged (Experimental)
**Lesson:**
- `EntityHistory` and `VersionDiff` primitives made this trivial to implement.
- This creates a bridge between raw bi-temporal data and LLM prompt generation (context injection).
- Timestamp formatting and string interning were the main integration points.

## Associative Retrieval (Fishing)
**Concept:** A search algorithm that "casts" a vector query to find similar nodes, then "spreads a net" to their neighbors, scoring results by a combination of vector similarity, graph proximity, and temporal freshness.
**Fate:** Merged (Experimental)
**Lesson:**
- Combining Vector + Graph + Time creates a rich "associative memory" feel.
- `VersionMetadata` allows for easy "freshness" boosting.
- Handling multiple vector indexes in a generic way requires some heuristics (currently picks the first enabled one).

## GraphContext
**Concept:** An LLM context exporter that combines node properties, temporal evolution (via Temporal Narrative), and neighborhood topology into a dense Markdown format.
**Fate:** Merged (Experimental)
**Lesson:**
- LLMs need "context" more than just "data". Formatting the graph as a narrative document makes it consumable by text-based models.
- Reusing `TemporalNarrative` avoided duplication.
- InternedString resolution is a common friction point in experimental modules; maybe `GraphContext` logic could be generalized into a `Display` trait for Nodes?
