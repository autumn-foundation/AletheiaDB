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
**Bloat:** `Resonator` trait in `src/experimental/echo.rs` (Single-implementation abstraction used only by `ActivityDensityResonator`).
**Cut:** Deleted the `Resonator` trait. Refactored `EchoChamber` to use the concrete `ActivityDensityResonator` struct directly.
**Saved:** ~10 lines of trait definition + dynamic dispatch overhead in experimental module.

## [Reduction]
**Bloat:** `PropagationModel` trait in `src/experimental/sybil.rs` (Single-implementation abstraction used only by `LinearPropagation`).
**Cut:** Deleted the `PropagationModel` trait. Refactored `Sybil::simulate` to use the concrete `LinearPropagation` struct directly.
**Saved:** ~10 lines of trait definition + cognitive load of generic abstractions.
