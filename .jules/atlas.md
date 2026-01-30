## 2026-01-28 - Breaking Dependency Cycles
**Tangle:**
1. `storage` <-> `index` cycle via `StorageObserver` (trait in storage, impl in index, usage in storage).
2. `core` <-> `storage` cycle via `VersionMetadata` (struct in storage, used in core graph, imported back in storage).
3. `storage` -> `api` dependency via `TxId` re-export (storage used api::TxId, api depends on storage).

**Blueprint:**
1. Moved `StorageObserver` to `core::observer` to break `storage` <-> `index`.
2. Moved `VersionMetadata` to `core::version` to break `core` <-> `storage`.
3. Updated `storage` to use `core::id::TxId` directly instead of `api::TxId` to respect layering.

## 2026-01-29 - The Blob in Temporal Vector Index
**Tangle:** `src/index/vector/temporal.rs` grew to 1400+ lines, mixing configuration, snapshot logic, statistics, and the core index implementation. This made it hard to navigate and maintain.
**Blueprint:** Split into `src/index/vector/temporal/` module. Extracted `config.rs`, `snapshot.rs` (internal), `stats.rs`, and `observer.rs`. Kept core logic and tests in `mod.rs` (for now) but significantly reduced noise.

## 2026-01-29 - Refactoring WAL Module
**Tangle:** `src/storage/wal.rs` was becoming a "Blob" module, containing data definitions (`WalEntry`, `WalOperation`), serialization logic, and module re-exports. It also duplicated property serialization sizing logic from `core`.
**Blueprint:**
1. Extracted `WalEntry` and related types to `src/storage/wal/entry.rs`.
2. Extracted serialization logic to `src/storage/wal/serialization.rs`.
3. Moved property serialization sizing logic to `core::property::PropertyMap::serialized_size` to improve cohesion.
4. Converted `src/storage/wal.rs` into a clean facade module.

## 2026-01-30 - Refactoring Historical Storage Blob
**Tangle:** `src/storage/historical.rs` was a massive 6900+ line file ("The Blob") containing configuration, statistics, core storage logic, and thousands of lines of tests, making navigation and maintenance difficult.
**Blueprint:**
1. Created `src/storage/historical/` directory module.
2. Extracted `RetentionPolicy` and constants to `config.rs`.
3. Extracted `CacheMetrics` and `HistoricalStats` to `stats.rs`.
4. Extracted tests to `tests.rs`.
5. Kept core logic in `mod.rs`, significantly reducing file size and improving cohesion.
