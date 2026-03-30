## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `StorageSnapshot` trait (Single-implementation abstraction used only by `CurrentStorageSnapshot` internally for checkpointing).
**Cut:** Deleted the `StorageSnapshot` trait. Refactored `CurrentStorageSnapshot` to use concrete types and inherent methods.
**Saved:** ~30 lines of boilerplate (trait definition, impl blocks, associated types) + removed cognitive load of an unnecessary abstraction.
