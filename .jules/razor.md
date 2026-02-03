## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `StorageObserver` trait, `StorageEvent` enum, and `VectorIndexObserver` (Over-engineered "One-Time Trait" pattern).
**Cut:** Removed the entire Observer infrastructure. Used existing `PreAnchorHook` closures for the only actual use case (vector index synchronization).
**Saved:** ~300 lines of code + removed redundant double-invocation of snapshot logic + simplified HistoricalStorage architecture.
