# ADR-0056: Refactor Write-Ahead Log (WAL) Module

**Status:** Accepted
**Date:** 2026-01-29
**Deciders:** AletheiaDB Core Team
**Categories:** storage, architecture, modularity, wal

## Context

The `src/storage/wal.rs` module was becoming a "Blob" module. It contained data definitions (`WalEntry`, `WalOperation`), serialization logic, and module re-exports. Additionally, it duplicated property serialization sizing logic that logically belongs to the core domain (`core::property`).

This design violated the Single Responsibility Principle and caused unnecessary coupling between the storage layer's implementation details and the core property structures.

## Decision

We have decided to refactor the `src/storage/wal.rs` module into a structured directory `src/storage/wal/` and separate concerns.

### Key Changes:
1.  **Extract Data Types:** `WalEntry` and related types have been extracted to `src/storage/wal/entry.rs`.
2.  **Extract Serialization Logic:** The serialization and deserialization implementations for WAL entries have been moved to `src/storage/wal/serialization.rs`.
3.  **Relocate Property Sizing:** Property serialization sizing logic was removed from `storage` and moved directly to `core::property::PropertyMap::serialized_size`.
4.  **Facade Pattern:** `src/storage/wal.rs` has been converted into a clean facade module that exposes only the necessary public API.

## Consequences

### Positive

-   **Enhanced Cohesion:** Moving property sizing to `core` ensures that logic directly tied to domain primitives is encapsulated within the domain layer.
-   **Improved Maintainability:** Separating serialization logic from data definitions makes the WAL implementation easier to navigate and modify.
-   **Clearer Boundaries:** The facade module (`wal.rs`) hides internal implementation details, providing a cleaner public API to the rest of the storage subsystem.

### Negative

-   **File Overhead:** The refactor introduces multiple new files and internal modules, requiring developers to navigate across files when reasoning about the whole WAL system.

### Neutral

-   **No Functional Change:** The core behavior of the WAL, including its performance characteristics and on-disk format, remains identical.
