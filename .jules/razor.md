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
**Bloat:** `GraphView` trait (Single-implementation abstraction used only by `AletheiaDB`), `Resonator` trait, `PropagationModel` trait
**Cut:** Deleted the `GraphView` trait and `src/db/graph_view.rs` wrapper module. Refactored `SemanticPathfinder` and `traverse_and_rank` to use `&AletheiaDB` directly. Deleted single-implementation traits `Resonator` and `PropagationModel` in the `experimental` module and used concrete structs `ActivityDensityResonator` and `LinearPropagation` instead.
**Saved:** ~100 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed unnecessary abstractions.
