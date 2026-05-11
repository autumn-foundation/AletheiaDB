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
**Bloat:** The  trait in `src/query/traits.rs` (and implemented in `src/db/graph_view.rs`). It was a "One-Time" trait implemented only by `AletheiaDB`, adding an unnecessary layer of abstraction.
**Cut:** Deleted the trait definition, its implementation, and the test coverage file that only tested the trait proxying methods. Refactored `SemanticPathfinder` and the `traverse_and_rank`/`find_similar_as_of` hybrid query functions to use the concrete `AletheiaDB` struct directly.
**Saved:** 3 files completely deleted (`src/query/traits.rs`, `src/db/graph_view.rs`, `tests/graph_view_coverage.rs`), totaling over 250 lines of boilerplate code.
## [Reduction]
**Bloat:** The `GraphView` trait in `src/query/traits.rs` (and implemented in `src/db/graph_view.rs`). It was a "One-Time" trait implemented only by `AletheiaDB`, adding an unnecessary layer of abstraction.
**Cut:** Deleted the trait definition, its implementation, and the test coverage file that only tested the trait proxying methods. Refactored `SemanticPathfinder` and the `traverse_and_rank`/`find_similar_as_of` hybrid query functions to use the concrete `AletheiaDB` struct directly.
**Saved:** 3 files completely deleted (`src/query/traits.rs`, `src/db/graph_view.rs`, `tests/graph_view_coverage.rs`), totaling over 250 lines of boilerplate code.
