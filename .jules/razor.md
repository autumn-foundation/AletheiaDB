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
**Bloat:** `Resonator` trait (Single-implementation abstraction used only by `ActivityDensityResonator`).
**Cut:** Deleted the `Resonator` trait and refactored `EchoChamber` to use the concrete `ActivityDensityResonator` struct directly, removing generic type parameters.
**Saved:** ~20 lines of trait definition + cognitive load of unnecessary abstraction layers + generic parameter bloat across the module.

## [Reduction]
**Bloat:** `SemanticRule` trait (Abstraction causing `Box<dyn SemanticRule>` allocations and dynamic dispatch for just two rule types).
**Cut:** Converted `SemanticRule` from a trait into an enum with `VectorBan` and `NumericRange` variants, moving the `validate` implementations to the concrete structs.
**Saved:** Removed dynamic dispatch overhead + simplified `Sentinel` to hold `Vec<SemanticRule>` instead of boxed trait objects.
