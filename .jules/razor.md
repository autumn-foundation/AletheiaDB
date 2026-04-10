## [Reduction]
**Bloat:** `GraphView` trait (Single-implementation abstraction used only by `AletheiaDB`).
**Cut:** Deleted the `GraphView` trait and `graph_view.rs` adapter. Refactored all consumers (`SemanticPathfinder`, `traverse_and_rank`, `find_similar_as_of`) to use the concrete `AletheiaDB` struct directly.
**Saved:** ~100 lines of boilerplate (trait definitions, adapter implementation) + removed dynamic dispatch and generic trait bounds complexity.
