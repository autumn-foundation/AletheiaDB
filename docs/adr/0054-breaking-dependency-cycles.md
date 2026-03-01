# ADR-0054: Breaking Dependency Cycles

**Status:** Proposed
**Date:** 2026-01-28
**Deciders:** Atlas
**Categories:** Architecture, Dependencies

## Context

The architecture was tangled with circular dependencies and boundary violations that made building difficult and violated the strict architectural dependency direction (DB -> API -> Storage -> Core):

1. `storage` <-> `index` cycle via `StorageObserver` (trait in storage, impl in index, usage in storage).
2. `core` <-> `storage` cycle via `VersionMetadata` (struct in storage, used in core graph, imported back in storage).
3. `storage` -> `api` dependency via `TxId` re-export (storage used api::TxId, api depends on storage).

## Decision

We will restructure the modules and types to break these cycles and enforce layering:

1. Move `StorageObserver` to `core::observer` to break the `storage` <-> `index` cycle.
2. Move `VersionMetadata` to `core::version` to break the `core` <-> `storage` cycle.
3. Update `storage` to use `core::id::TxId` directly instead of `api::TxId` to respect the dependency hierarchy.

## Consequences

### Positive

- Strict architectural direction (DB -> API -> Storage -> Core) is enforced without cycles.
- Improved module cohesion.
- Prevents compilation tangles and build failures due to circular dependencies.

### Negative

- Increased FFI complexity or import updates across crates that previously relied on the broken boundaries.
- Refactoring effort to update paths in dependent modules.

### Neutral

- `TxId` is now strictly a domain primitive residing in `core`.
