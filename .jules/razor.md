## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `StorageSnapshot` and `FieldHolder` traits.
**Cut:** Deleted single-implementation traits `StorageSnapshot` (implemented only by `CurrentStorageSnapshot`) and `FieldHolder` (implemented only by `Event`, unused except in tests). Moved methods directly to structs.
**Saved:** ~50 lines of boilerplate + cognitive load of unnecessary abstraction layers.

## [Reduction]
**Bloat:** `GraphView` trait (Single-implementation abstraction used only by `AletheiaDB` for decoupling storage from queries).
**Cut:** Deleted the `GraphView` trait and `src/db/graph_view.rs` implementation. Refactored `SemanticPathfinder` and hybrid query functions (`traverse_and_rank`, `find_similar_as_of`) to accept a concrete `&crate::db::AletheiaDB` reference.
**Saved:** ~150 lines of boilerplate trait definitions and delegation wrappers + cognitive load of an unnecessary abstraction layer.
