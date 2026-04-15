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
**Bloat:** `GraphView` trait.
**Cut:** Deleted single-implementation trait `GraphView` (implemented only by `AletheiaDB`). Refactored all query algorithms (like `traverse_and_rank`, `find_similar_as_of`, `SemanticPathfinder`) to accept concrete `AletheiaDB` reference.
**Saved:** ~150 lines of boilerplate (trait definition, AletheiaDB delegation implementations, unused imports) + cognitive overhead.
