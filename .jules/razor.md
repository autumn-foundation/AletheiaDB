## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `GraphView` trait (Single-implementation abstraction used only by `AletheiaDB` to decouple the database from query algorithms in the same crate).
**Cut:** Deleted the `GraphView` trait and `graph_view.rs` adapter module. Replaced `<G: GraphView + ?Sized>` generics in `traverse_and_rank`, `find_similar_as_of`, and `SemanticPathfinder` with concrete `&crate::db::AletheiaDB` references.
**Saved:** ~180 lines of boilerplate (trait definitions, mock-like implementations, duplicate imports) + removed generic `<G>` constraints everywhere.
