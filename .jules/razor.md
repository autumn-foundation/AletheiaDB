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
**Bloat:** `KeyRotationManager` and related types (`RotationState`, `KeyVersion`), and their audit logs.
**Cut:** Deleted the `rotation.rs` module, removed it from `mod.rs`, and removed rotation-related events from `audit.rs`.
**Saved:** ~300 lines of code + cognitive load of maintaining an unused key rotation system.

## [Reduction]
**Bloat:** `KeyFormat` enum.
**Cut:** Removed the `KeyFormat` enum from `key_provider.rs` and `mod.rs` as it's unused.
**Saved:** ~20 lines of code.

## [Reduction]
**Bloat:** `KeyProvider` trait.
**Cut:** Deleted the `KeyProvider` trait. Replaced dynamic dispatch (`Box<dyn KeyProvider>`) with direct concrete usage in `EncryptionManager` by matching on `KeyProviderConfig`.
**Saved:** Trait abstraction overhead and dynamic dispatch.
