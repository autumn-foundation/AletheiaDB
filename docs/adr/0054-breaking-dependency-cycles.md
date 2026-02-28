# ADR-0054: Breaking Dependency Cycles

**Status:** Accepted
**Date:** 2026-01-28
**Deciders:** Atlas, Codex
**Categories:** architecture, core, storage, index, api

## Context

As AletheiaDB has grown, several critical architectural tangles and circular dependencies emerged between modules. These tangles violated the strict module hierarchy, increasing tight coupling and introducing the risk of build complications.

Specifically, the following dependency cycles and improper boundaries were identified:

1. **`storage` <-> `index` Cycle**:
   A circular dependency existed via `StorageObserver`. The trait was defined in `storage`, implemented in `index`, but usage of the implementation was required back in `storage`.
2. **`core` <-> `storage` Cycle**:
   A circular dependency existed via `VersionMetadata`. This struct was defined in `storage`, but was needed by the core graph representations. As a result, `core` depended on `storage` to access it, and `storage` naturally depended on `core`.
3. **`storage` -> `api` Dependency**:
   The `storage` layer improperly depended on the `api` layer by utilizing `api::TxId`. This violated the intended architectural layering where `api` should depend on `storage`, not the reverse.

## Decision

To resolve these tangles and enforce a strict unidirectional dependency graph, we applied the following refactoring blueprint:

1. **Moved `StorageObserver` to `core::observer`**:
   This breaks the `storage` <-> `index` cycle. Both `storage` and `index` now depend on the shared definition in `core`.
2. **Moved `VersionMetadata` to `core::version`**:
   This breaks the `core` <-> `storage` cycle. `VersionMetadata` is now treated as a fundamental domain primitive residing in `core`, eliminating `core`'s dependency on `storage`.
3. **Updated `storage` to use `core::id::TxId`**:
   We removed the `storage` dependency on `api::TxId` and instead used the base `TxId` defined in `core::id`. This respects proper architectural layering.

## Consequences

### Positive

- **Acyclic Dependency Graph**: The module dependencies are now strictly unidirectional (e.g., `API` -> `Storage` -> `Core` and `Index` -> `Storage` -> `Core`).
- **Improved Modularity**: Clearer domain boundaries enforce "The Cartographer" and "Atlas" architectural principles.
- **Better Build Times**: Removing circular dependencies streamlines the compiler's build graph and reduces incremental compilation bottlenecks.

### Negative

- **Refactoring Churn**: The changes required touching numerous files across the `core`, `storage`, `index`, and `api` modules.

## References

- `.jules/atlas.md` (2026-01-28 - Breaking Dependency Cycles)
- `docs/ARCHITECTURE.md`
