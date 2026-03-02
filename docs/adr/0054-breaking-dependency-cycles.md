# ADR-0054: Breaking Dependency Cycles

**Status:** Proposed
**Date:** 2026-01-28
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, modularity, core, storage

## Context

During the evolution of AletheiaDB, several circular dependencies and inappropriate logical couplings emerged between the `core`, `storage`, `index`, and `api` layers:

1.  **Storage ↔ Index Cycle:** `storage` contained the `StorageObserver` trait. `index` implemented this trait, but `storage` then needed to use `index` types or mechanisms, creating a cycle.
2.  **Core ↔ Storage Cycle:** `storage` defined the `VersionMetadata` struct, which was used in `core` graph operations. However, `storage` logically depends on `core` for its types, leading to a circular import.
3.  **Storage → API Dependency:** The `storage` layer imported `TxId` from the `api` layer. The `api` layer, conversely, depends on `storage` for persistence, violating the expected architectural layering.

These tangles made refactoring difficult, increased build times, and broke the strict domain boundaries defined in earlier architectural decisions.

## Decision

We resolved these circular dependencies by realigning the components with their correct architectural layers:

1.  **Relocated `StorageObserver`:** Moved the `StorageObserver` trait from `storage` to `core::observer`. This breaks the cycle, as both `storage` and `index` can now safely depend on `core`.
2.  **Relocated `VersionMetadata`:** Moved `VersionMetadata` from `storage` to `core::version`. This solidifies `core` as the true source of domain concepts and breaks the cycle between `core` and `storage`.
3.  **Updated `TxId` Usage:** Modified `storage` to import `TxId` directly from `core::id::TxId` instead of `api::TxId`. This respects the layering principle where lower layers (storage) should not depend on higher layers (api).

## Consequences

### Positive

-   **Clearer Boundaries:** Enforces the strict rule that `storage` depends on `core`, and `api` depends on `storage`, with no backward references.
-   **Improved Maintainability:** Eliminating circular dependencies makes the codebase significantly easier to navigate, refactor, and understand.
-   **Faster Compilation:** A cleaner dependency graph allows the compiler to parallelize builds more effectively and reduces the blast radius of incremental changes.

### Negative

-   None. This is a purely structural improvement with no runtime overhead.
