## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `StorageSnapshot` trait (Single-implementation abstraction used only by `CurrentStorageSnapshot`).
**Cut:** Deleted the `StorageSnapshot` trait entirely and moved its required methods (`lsn`, `node_count`, `edge_count`, `iter_nodes`, `iter_edges`) to be inherent `pub` methods directly on `CurrentStorageSnapshot`.
**Saved:** ~30 lines of boilerplate (trait definition, empty implementation blocks, redundant imports) + removed an unnecessary layer of abstraction.
