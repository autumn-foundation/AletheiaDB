# ADR-0054: Breaking Dependency Cycles

**Status:** Proposed
**Date:** 2026-01-28
**Deciders:** Atlas
**Categories:** Architecture, Refactoring, Core, Storage, Index

## Context

AletheiaDB enforces strict domain layering in its architecture. However, several circular dependencies had developed over time, causing tight coupling between crates, build complications, and violations of domain boundaries.

Specifically, the following cycles were identified:
1. `storage` <-> `index` cycle via `StorageObserver` (trait defined in `storage`, implemented in `index`, but used by `storage`).
2. `core` <-> `storage` cycle via `VersionMetadata` (struct defined in `storage`, but used by the core graph logic, which is then imported back into `storage`).
3. `storage` -> `api` dependency via `TxId` re-export (`storage` was using `api::TxId`, while `api` inherently depends on `storage`).

These entanglements made the system harder to maintain, test in isolation, and navigate. A clean architectural separation was necessary to restore the intended domain layers.

## Decision

We will systematically untangle these dependency cycles by migrating shared primitives and abstractions to the `core` layer, acting as the foundation for both `storage` and `index`.

1. **Move `StorageObserver` to `core::observer`**: This breaks the `storage` <-> `index` cycle. Both `storage` and `index` can now depend on `core` without depending on each other.
2. **Move `VersionMetadata` to `core::version`**: This breaks the `core` <-> `storage` cycle. `VersionMetadata` is a fundamental domain primitive, so it rightfully belongs in `core`, allowing `storage` to persist it without causing a cycle.
3. **Update `storage` to use `core::id::TxId`**: This fixes the layering violation where `storage` depended on `api`. `storage` will now import `TxId` directly from `core::id`, respecting the strict domain boundaries.

## Consequences

### Positive

- **Architectural Clarity**: Strict domain layering is restored (`core` <- `storage`, `core` <- `index`, `core` <- `api`).
- **Build Times**: Removing circular dependencies improves Cargo build times and allows for better parallel compilation.
- **Maintainability**: Modules are less coupled, making it easier to reason about changes in isolation.

### Negative

- **Refactoring Churn**: Significant file movements and import updates across multiple crates.
- **FFI/API Boundary Changes**: Minor internal API adjustments required to route types correctly through `core`.

### Neutral

- `core` grows slightly larger as it takes on more shared domain primitives (`StorageObserver`, `VersionMetadata`).

## Implementation Notes

These changes were executed by the Atlas persona to ensure architectural integrity. The C4 diagrams and class diagrams should be updated to remove these cyclic arrows and reflect the clean unidirectional dependencies onto `core`.
