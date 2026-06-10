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
**Bloat:** `PropagationModel` trait (Single-implementation abstraction used only by `LinearPropagation`).
**Cut:** Deleted the `PropagationModel` trait and used `LinearPropagation` concretely in `Sybil::simulate`.
**Saved:** ~20 lines of trait definition + cognitive load of unnecessary generic abstraction.

## [Reduction]
**Bloat:** `Resonator` trait (Single-implementation abstraction used only by `ActivityDensityResonator`).
**Cut:** Deleted the `Resonator` trait and used `ActivityDensityResonator` concretely in `EchoChamber`.
**Saved:** ~10 lines of trait definition + cognitive load and `Box<dyn>` overhead.

## [Reduction]
**Bloat:** `GraphView` trait (Single-implementation abstraction used only by `AletheiaDB`).
**Cut:** Deleted the `GraphView` trait and `src/db/graph_view.rs` file. Replaced `<G: GraphView + ?Sized>` and `&dyn GraphView` references with concrete `&AletheiaDB` in `hybrid.rs` and `semantic_pathfinding.rs`.
**Saved:** ~150 lines of boilerplate (trait definitions, duplicate implementations) + cognitive load of unnecessary indirection.
