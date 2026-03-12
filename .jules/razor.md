## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.
## [Reduction]
**Bloat:** Unnecessary traits (`Resonator`, `PropagationModel`) in `src/experimental/` that only had a single implementation (`ActivityDensityResonator`, `LinearPropagation`).
**Cut:** Flattened the traits into concrete structs, replacing dynamic dispatch and generic bounds with direct type usage in `EchoChamber` and `Sybil`.
**Saved:** ~30 lines of code + cognitive load of navigating traits.

## [Reduction]
**Bloat:** Verbose `GraphContextBuilder` in `src/experimental/graph_context.rs` that only acted as a data holder for a few arguments before calling `build()`.
**Cut:** Removed the builder struct and replaced it with a simple `build_graph_context` function.
**Saved:** ~50 lines of boilerplate code + cognitive load of instantiating a builder for a simple function call.
