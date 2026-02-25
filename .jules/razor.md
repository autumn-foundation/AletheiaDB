## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `StorageObserver` trait and `VectorIndexObserver` implementation (Over-engineered "extensibility" pattern with only one implementation).
**Cut:** Removed the entire observer system (trait, events, implementation, plumbing). Replaced with existing `PreAnchorHook` mechanism which provides stronger consistency and was already doing the same job.
**Saved:** 5 files, ~600 lines of code.
