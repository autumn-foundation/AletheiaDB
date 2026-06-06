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
## De-abstract Resonator
**Bloat:** `Resonator` trait with a single implementation `ActivityDensityResonator` used via `Box<dyn Resonator>`.
**Cut:** Removed the trait, made `resonate` a direct method on `ActivityDensityResonator`, and removed the Box/dyn dispatch.
**Saved:** Removed dynamic dispatch overhead, simplified generic signatures in `EchoChamber` API, and deleted ~10 lines of trait definition.
## De-abstract GraphView
**Bloat:** `GraphView` trait used to abstract `AletheiaDB` in query modules, but only `AletheiaDB` implements it. Creates "Generic Soup" like `<G: GraphView + ?Sized>`.
**Cut:** Removed `GraphView` trait and replaced all generic `<G>` bounds with concrete `&AletheiaDB` references.
**Saved:** Deleted entire `graph_view.rs` and `traits.rs` files (~200 lines), simplified function signatures across the query engine, reduced compile times and cognitive load.
