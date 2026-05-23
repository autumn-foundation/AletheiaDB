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
**Bloat:** `VectorNodeClient`, `Resonator`, and `GraphView` (Single-implementation traits that created unnecessary generic layers and dynamic dispatch).
**Cut:** Deleted the traits and replaced them with their single concrete implementations (`MockVectorNodeClient` renamed to `VectorNodeClient`, `ActivityDensityResonator` renamed to `Resonator`, and `GraphView` replaced with `AletheiaDB`). Removed the generic `<C: VectorNodeClient>` from `DistributedVectorIndex` and `<G: GraphView>` from semantic query algorithms.
**Saved:** ~100 lines of boilerplate (trait definitions, generic parameters, impl wrappers) + reduced cognitive load of chasing down trait implementations that only exist in one place.
