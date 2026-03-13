## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `GraphView` trait (Single-implementation abstraction used only by `AletheiaDB` with `?Sized` trait bounds leaking into queries).
**Cut:** Deleted the `GraphView` trait and `src/db/graph_view.rs` module. Refactored all query consumers (`hybrid.rs`, `semantic_pathfinding.rs`) to use the concrete `AletheiaDB` reference directly.
**Saved:** ~80 lines of boilerplate (trait definition and implementation) + removed dynamic dispatch capability and `?Sized` bound complexity.

## [Reduction]
**Bloat:** `StorageSnapshot` trait (Single-implementation abstraction used only by `CurrentStorageSnapshot` that unnecessarily decoupled checkpointing from the snapshot data model).
**Cut:** Deleted the `StorageSnapshot` trait and moved its methods directly to the `CurrentStorageSnapshot` concrete type. Removed trait imports from `checkpoint.rs` and tests.
**Saved:** ~30 lines of boilerplate (trait definition and `impl` block) + removed unneeded abstraction layer.
