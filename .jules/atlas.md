## 2024-05-24 - Breaking Dependency Cycles
**Tangle:**
1. `storage` <-> `index` cycle via `StorageObserver` (trait in storage, impl in index, usage in storage).
2. `core` <-> `storage` cycle via `VersionMetadata` (struct in storage, used in core graph, imported back in storage).
3. `storage` -> `api` dependency via `TxId` re-export (storage used api::TxId, api depends on storage).

**Blueprint:**
1. Moved `StorageObserver` to `core::observer` to break `storage` <-> `index`.
2. Moved `VersionMetadata` to `core::version` to break `core` <-> `storage`.
3. Updated `storage` to use `core::id::TxId` directly instead of `api::TxId` to respect layering.
