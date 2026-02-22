## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** Unused experimental modules (`ConceptAlgebra`, `Dissonance`, `Kaleidoscope`, `Sybil`, `Sentinel`, `Sherlock`, `Thermos`, `Wormhole`).
**Cut:** Deleted 8 experimental source files and removed them from `src/experimental/mod.rs`.
**Saved:** ~1500 lines of dead code / speculative generality.
