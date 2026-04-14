## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `StorageSnapshot` and `FieldHolder` traits.
**Cut:** Deleted single-implementation traits `StorageSnapshot` (implemented only by `CurrentStorageSnapshot`) and `FieldHolder` (implemented only by `Event`, unused except in tests). Moved methods directly to structs.
**Saved:** ~50 lines of boilerplate + cognitive load of unnecessary abstraction layers.

## [Reduction]
**Bloat:** `ResultExt` trait and `.record_error_metric()` abstraction (Used only on `Result` types but adds unnecessary boilerplate, most implementations should log errors explicitly at the entry/exit bounds instead of implicit trait extension over `Result`).
**Cut:** Deleted `ResultExt` trait from `core/error.rs`, removed its implementations, and removed 49 usages of `.record_error_metric()` along with unused `ResultExt` imports.
**Saved:** ~100 lines of code + cognitive load of checking side effects on `Result` handling.
