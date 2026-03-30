## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `EntityId` duplicate definition (defined in both `core::id` and `query::executor::results`).
**Cut:** Deleted the duplicate `EntityId` enum from `results.rs` and updated imports to use the canonical one from `core::id`.
**Saved:** ~20 lines of redundant enum definition.

## [Reduction]
**Bloat:** `Resonator` trait (Single-implementation abstraction used only by `ActivityDensityResonator`).
**Cut:** Deleted the `Resonator` trait and refactored `EchoChamber` to use the concrete `ActivityDensityResonator` struct directly.
**Saved:** ~10 lines of trait definition + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `FieldHolder` trait (Single-implementation abstraction used only by `Event`).
**Cut:** Deleted the `FieldHolder` trait and refactored its methods directly into the `Event` implementation.
**Saved:** ~20 lines of trait definition and boilerplate testing.
