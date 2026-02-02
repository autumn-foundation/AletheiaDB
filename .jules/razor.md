## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `TieredStorage` (Redundant abstraction layer adding caching, prefetching, and metrics on top of `RedbColdStorage`).
**Cut:** Deleted `TieredStorage` struct and module. Refactored `HistoricalStorage` to use `RedbColdStorage` directly.
**Saved:** ~400 lines of code + removed an unnecessary layer of indirection + simplified storage hierarchy.
