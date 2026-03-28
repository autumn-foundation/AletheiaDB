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
**Bloat:** `PropagationModel` and `Resonator` traits (Single-implementation abstractions in experimental features).
**Cut:** Deleted the single-implementation traits. Refactored `Sybil` and `EchoChamber` to use concrete structs `LinearPropagation` and `ActivityDensityResonator` directly.
**Saved:** ~30 lines of boilerplate + cognitive load of unnecessary abstractions.

## [Reduction]
**Bloat:** `SemanticRule` trait and `Box<dyn SemanticRule>` in `Sentinel` (Overkill dynamic dispatch for two variants).
**Cut:** Replaced `SemanticRule` trait with a concrete `enum SemanticRule` containing `VectorBan` and `NumericRange` variants. Updated `Sentinel` to store `Vec<SemanticRule>`.
**Saved:** Dynamic dispatch overhead, heap allocations, and simplified validation loop logic.
