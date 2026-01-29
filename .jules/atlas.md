## 2024-05-24 - Breaking Dependency Cycles
**Tangle:**
1. `storage` <-> `index` cycle via `StorageObserver` (trait in storage, impl in index, usage in storage).
2. `core` <-> `storage` cycle via `VersionMetadata` (struct in storage, used in core graph, imported back in storage).
3. `storage` -> `api` dependency via `TxId` re-export (storage used api::TxId, api depends on storage).

**Blueprint:**
1. Moved `StorageObserver` to `core::observer` to break `storage` <-> `index`.
2. Moved `VersionMetadata` to `core::version` to break `core` <-> `storage`.
3. Updated `storage` to use `core::id::TxId` directly instead of `api::TxId` to respect layering.

## 2024-05-25 - Refactoring WAL Module
**Tangle:** `src/storage/wal.rs` was becoming a "Blob" module, containing data definitions (`WalEntry`, `WalOperation`), serialization logic, and module re-exports. It also duplicated property serialization sizing logic from `core`.
**Blueprint:**
1. Extracted `WalEntry` and related types to `src/storage/wal/entry.rs`.
2. Extracted serialization logic to `src/storage/wal/serialization.rs`.
3. Moved property serialization sizing logic to `core::property::PropertyMap::serialized_size` to improve cohesion.
4. Converted `src/storage/wal.rs` into a clean facade module.
