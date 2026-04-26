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
**Bloat:** Single-implementation traits (`Resonator` in `echo.rs` and `SemanticRule` in `sentinel.rs`) wrapping concrete types in `Box<dyn Trait>` unnecessarily, creating dynamic dispatch overhead and complex lifetimes.
**Cut:** Removed the `Resonator` trait entirely, calling methods directly on `ActivityDensityResonator`. Replaced the `SemanticRule` trait with a concrete enum containing its two implementations.
**Saved:** Multiple lines of trait boilerplate, two heap allocations per instance (from Box), dynamic dispatch overhead, and cognitive load for the reader.
