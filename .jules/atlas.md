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

## 2026-01-30 - Splitting HistoricalStorage
**Tangle:** `src/storage/historical.rs` was a 7000-line "Blob" containing mixed storage logic and 4400 lines of tests, making it hard to navigate and maintain.
**Blueprint:** Refactored into `src/storage/historical/` directory. Moved tests to `tests.rs` (~4400 lines), leaving the core logic in `mod.rs` (~2600 lines). This separates concerns and makes the core logic more accessible.

## 2026-02-01 - Splitting CurrentStorage
**Tangle:** `src/storage/current.rs` was a "Blob" module containing storage implementation, iterators, statistics, vector index helpers, and extensive tests, making it difficult to maintain.
**Blueprint:** Refactored into `src/storage/current/` directory. Extracted `iterators.rs`, `stats.rs`, and `vector.rs` to separate concerns. Moved tests to `tests.rs`, leaving `mod.rs` as a clean facade and core implementation.

## 2026-02-01 - Consolidating Versioning
**Tangle:** Versioning logic was split between `core::version` (metadata) and `storage::version` (data), creating a false boundary and confusion. `storage` exported core domain primitives like `NodeVersion`.
**Blueprint:** Consolidated all versioning logic into `src/core/version.rs`. Updated `storage` to re-export `core::version` for backward compatibility. This strengthens `core` as the domain definition and `storage` as the implementation.

## 2024-05-23 - Splitting the God Object in src/db.rs
**Tangle:** `src/db.rs` was a 3500-line "God Object" responsible for everything from configuration and transaction management to vector indexing, temporal queries, and admin operations. This violated the Single Responsibility Principle and made navigation difficult.
**Blueprint:** Refactored `src/db.rs` into a `src/db/` module directory.
1. Kept the core `AletheiaDB` struct definition in `mod.rs`.
2. Extracted implementations into cohesive submodules:
   - `config.rs`: Initialization and configuration.
   - `transaction.rs`: Transaction lifecycle management.
   - `ops.rs`: Basic CRUD and graph operations.
   - `temporal.rs`: Temporal query operations.
   - `vector.rs`: Vector index management and search.
   - `query.rs`: Query builder and executor integration.
   - `admin.rs`: Maintenance, statistics, and persistence.
   - `tests.rs`: Unit tests.

## 2026-02-16 - Splitting WriteTransaction God Struct
**Tangle:** `src/api/transaction/write_tx.rs` was a 5000-line "God Struct" handling validation, conflict detection, WAL logging, storage application, and extensive tests.
**Blueprint:** Refactored into `src/api/transaction/write/` directory.
1. `mod.rs`: Defines `WriteTransaction` struct and public API.
2. `validation.rs`: Extracted validation logic.
3. `conflict.rs`: Extracted MVCC conflict detection.
4. `apply.rs`: Extracted storage application logic.
5. `wal.rs`: Extracted WAL logging logic.
6. `tests.rs`: Moved all tests (~4000 lines) to a separate file.

## 2026-05-23 - Breaking Circular Dependencies
**Tangle:**
1. `storage` <-> `api` cycle: `storage` imported `api::TxId`, while `api` depends on `storage`.
2. `api` <-> `db` cycle: `api` contained `VectorIndexBuilder` which depended on `db::AletheiaDB`, while `db` depends on `api` for transactions.
3. `core` <-> `utils` cycle: `utils` contained `Error` which depended on `core::id`, while `core` depended on `utils::Error`.

**Blueprint:**
1. Updated `storage/index_persistence/operations.rs` to import `TxId` from `core::id` instead of `api`.
2. Moved `VectorIndexBuilder` from `api` to `db`, as it is a concrete helper for `AletheiaDB`.
3. Moved `utils/error.rs` to `core/error.rs` and deleted `utils` module, consolidating core domain types.

## 2026-05-24 - Unwinding API/Core Dependency
**Tangle:** `db` module imported `TxIdGenerator` through `api::transaction`. This caused a layering violation where `db` skipped `core` to rely on an `api` re-export for a domain primitive, creating coupling between `api` and `core`'s ID generation responsibilities.
**Blueprint:** Removed `TxIdGenerator` re-export from `api::transaction::types` and `api::transaction::mod`. Updated `db::mod`, `db::config`, and all transaction tests to explicitly import `TxIdGenerator` directly from `core::id::TxIdGenerator`.

## 2025-03-10 - Breaking VersionMetadata Dependency Cycle
**Tangle:** Both `src/index/temporal.rs` and `src/core/version.rs` contained structs named `VersionMetadata`, causing naming collisions and semantic ambiguity ("The Leak" architectural smell).
**Blueprint:** Renamed `src/index/temporal.rs::VersionMetadata` to `TimelineVersionMetadata` (and the associated index alias) to strictly enforce domain boundaries and clarify that the index-level struct serves temporal timelines, whereas `core` defines the global domain version primitive.

## 2026-03-14 - The Blob in Core Property
**Tangle:** `src/core/property.rs` was a 4,200-line "Blob" module. It contained serialization tags, value definitions, serialization/deserialization logic, the core `PropertyValue` enum, the `PropertyMap` struct, the builder pattern, and 2,500 lines of tests. The file was difficult to navigate and combined too many distinct concerns related to the copy-on-write property system.
**Blueprint:** Refactored `src/core/property.rs` into a `src/core/property/` directory.
1. Extracted serialization constants, vectors, and the `PropertyValue` enum (and its impls) into `src/core/property/value.rs`.
2. Extracted `PropertyKey`, `PropertyMap`, and `PropertyMapBuilder` into `src/core/property/map.rs`.
3. Moved all tests into `src/core/property/tests.rs`.
4. Kept `src/core/property/mod.rs` as a clean facade module re-exporting the public types and constants.

## 2026-03-14 - The Blob in Storage Checkpoint
**Tangle:** `src/storage/checkpoint.rs` was a 3,000-line "Blob" module containing the configuration, recovery result structs, the core manager logic, and large test suites for point-in-time snapshots and crash recovery.
**Blueprint:** Refactored `src/storage/checkpoint.rs` into a `src/storage/checkpoint/` directory.
1. Extracted `CheckpointConfig` into `config.rs`.
2. Extracted `RecoveryResult` and related structs into `recovery.rs`.
3. Extracted `CheckpointManager` and `CheckpointStats` into `manager.rs`.
4. Moved tests to `tests.rs`.
5. Re-exported the public API in `mod.rs`.

## 2026-03-14 - RedbColdStorage Tests Separation
**Tangle:** `src/storage/redb_cold_storage.rs` was a 3,400-line file that included 1,900 lines of tests. The sheer size of the test module obscured the core storage implementation.
**Blueprint:** Created `src/storage/redb_cold_storage/` directory. Split tests into `src/storage/redb_cold_storage/tests.rs` and retained the storage implementation in `mod.rs`.
