# ADR-0053: Decouple Storage from Core (Redux)

**Status:** Accepted
**Date:** 2026-03-24
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, storage, core, modularity
**Supersedes:** [ADR-0027](0027-decouple-storage-from-core.md)

## Context

ADR-0027 proposed decoupling `Core` from `Storage` by introducing abstract `StorageEngine` traits. However, as documented in ADR-0044, fully genericizing the `AletheiaDB` struct (the main entry point) proved to be overly complex and detrimental to performance due to dynamic dispatch or viral generics.

Despite this, the need to physically separate the core domain logic (types, query planning, temporal reasoning) from the concrete storage implementation (WAL, disk I/O, serialization) remains critical to break circular dependencies and improve compilation times.

The previous attempt (ADR-0027) envisioned a trait-based boundary where `Core` defined the interface and `Storage` implemented it. The reality of the implementation (ADR-0044) is that `AletheiaDB` (in `src/db`) acts as the composition root, binding `Core` types to concrete `Storage` implementations.

We need to formalize this architectural pattern: **Vertical Decoupling via Module Separation, Horizontal Coupling via Composition Root.**

## Decision

We reaffirm the separation of `src/core` and `src/storage` as distinct modules with clear responsibilities, but we explicitly decide to **couple them concretely in the `src/db` layer**.

The architectural layers are defined as:

1.  **Core (`src/core`)**:
    *   **Responsibility**: Pure domain logic, data structures (`Node`, `Edge`), query planning, temporal primitives.
    *   **Dependency Rule**: Must NOT depend on `src/storage` or `src/db`.
    *   **Storage Interface**: Defines the *data shape* but not the *access method*.

2.  **Storage (`src/storage`)**:
    *   **Responsibility**: Persistence logic, WAL management, page cache, serialization.
    *   **Dependency Rule**: Depends on `src/core` for data types.
    *   **Implementation**: Provides concrete structs like `CurrentStorage`, `HistoricalStorage`.

3.  **Database (`src/db`)**:
    *   **Responsibility**: Composition root, public API, transaction coordination.
    *   **Dependency Rule**: Depends on both `src/core` and `src/storage`.
    *   **Binding**: Binds `Core` types to `Storage` implementations via the `AletheiaDB` struct.

```rust
// src/db/mod.rs
pub struct AletheiaDB {
    pub(crate) current: Arc<CurrentStorage>, // Concrete type from src/storage
    pub(crate) historical: Arc<RwLock<HistoricalStorage>>, // Concrete type from src/storage
    // ...
}
```

This decision supersedes the strict trait-based decoupling proposed in ADR-0027. We acknowledge that while `Core` and `Storage` are separate modules, the `AletheiaDB` system is an integrated whole that relies on specific, high-performance storage implementations.

## Consequences

### Positive

-   **Performance**: Direct usage of concrete storage types in `src/db` enables static dispatch and aggressive inlining (supporting the <1µs traversal goal).
-   **Simplicity**: Avoids the complexity of `Box<dyn StorageEngine>` or `AletheiaDB<S: StorageEngine>`.
-   **Build Times**: Changes to `src/storage` implementation details do not trigger a rebuild of `src/core`, only `src/db`.
-   **Clear Boundaries**: Enforces a unidirectional dependency graph: `db -> storage -> core`.

### Negative

-   **Coupling**: `AletheiaDB` is tightly coupled to `CurrentStorage` and `HistoricalStorage`. Swapping the storage engine requires code changes in `src/db`.
-   **Testing**: Unit testing `AletheiaDB` logic requires the real storage stack (or complex mocking of concrete types). However, `Core` logic remains purely testable in isolation.

## Compliance

This ADR formalizes the existing codebase structure where `src/core` defines the domain and `src/storage` implements persistence, wired together by `src/db`.
