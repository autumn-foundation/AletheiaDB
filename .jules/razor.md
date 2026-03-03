## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `MigrationCallback` trait (Single-implementation abstraction used only by `DefaultMigrationCallback` outside of tests).
**Cut:** Removed the trait and replaced it with a concrete `MigrationCallback` struct containing `Option<Arc<dyn Fn...>>` hooks.
**Saved:** Reduced boilerplate, flattened abstractions, and simplified usage in `MigrationService`.

## [Reduction]
**Bloat:** `Resonator` trait (Single-implementation abstraction used only by `ActivityDensityResonator`).
**Cut:** Deleted the `Resonator` trait and updated `EchoChamber` to directly take and use `ActivityDensityResonator`.
**Saved:** Removed unnecessary generic indirection (`Box<dyn Resonator>`) and boilerplate.

## [Reduction]
**Bloat:** `PropagationModel` trait (Single-implementation abstraction used only by `LinearPropagation`).
**Cut:** Deleted the `PropagationModel` trait and updated `Sybil` simulate method to directly accept `LinearPropagation`.
**Saved:** Removed trait definition, eliminated `M: PropagationModel` generic bounds, simplified code.
