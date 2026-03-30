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
**Bloat:** `GraphView` trait
**Cut:** Removed the `GraphView` trait in `src/query/traits.rs` which was only implemented by `AletheiaDB`. Removed `src/db/graph_view.rs` and updated query functions like `SemanticPathfinder` and `traverse_and_rank` to use `AletheiaDB` directly.
**Saved:** ~100 lines of boilerplate + cognitive load of unnecessary abstraction layer.

## [Reduction]
**Bloat:** `Resonator` trait
**Cut:** Removed the single-implementation trait `Resonator` in `src/experimental/echo.rs` and replaced it with its concrete implementation `ActivityDensityResonator`.
**Saved:** ~20 lines of code + cognitive load of unnecessary dynamic dispatch `Box<dyn Resonator>`.
