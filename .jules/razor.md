## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `StorageObserver` trait (Single-method trait effectively acting as a callback, implemented by `VectorIndexObserver`).
**Cut:** Replaced `StorageObserver` trait with a closure type alias `Arc<dyn Fn(&StorageEvent) -> Result<()>>`. Replaced `VectorIndexObserver` struct with a factory function returning the closure. Deleted `src/core/observer.rs`.
**Saved:** ~150 lines of boilerplate (trait definition, struct wrapper, impl blocks) + simplified testing by using closures directly.
