## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `StorageObserver` trait (Single-implementation abstraction used only by `VectorIndexObserver`).
**Cut:** Replaced the trait hierarchy with a simple `StorageCallback` closure (`Arc<dyn Fn...>`) and inline filtering.
**Saved:** ~100 lines of code (trait definition, struct boilerplate, file `observer.rs`) + removed unnecessary dependency inversion between core and index.
