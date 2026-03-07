# ADR-0054: Strict Domain Layering and Acyclic Dependencies

**Status:** Proposed
**Date:** 2026-03-07
**Deciders:** Atlas, Codex
**Categories:** architecture, modularity, core, api, storage, db

## Context

AletheiaDB enforces strict domain layering in its architecture: `core` defines primitives and traits, `storage` handles persistence, `index` manages indexing structures, `api` provides external interfaces, and `db` coordinates these layers.

Over time, several "Tangles" (architectural anti-patterns) emerged:

1. **`api` leaking `core` primitives:** The `db` module imported `TxIdGenerator` through a re-export in `api::transaction`. This caused a layering violation where `db` skipped `core` to rely on an `api` re-export for a domain primitive, creating coupling between `api` and `core`'s ID generation responsibilities.
2. **Circular Dependencies:**
   - `storage` <-> `api`: `storage` depended on `api` for `TxId`, while `api` depended on `storage`.
   - `api` <-> `db`: `api` exposed `VectorIndexBuilder` which held a reference to `AletheiaDB` (defined in `db`), while `db` depends on `api` for transaction interfaces.
   - `core` <-> `utils`: `utils` contained `Error` types that depended on `core::id` (e.g., `NodeId`), while `core` modules depended on `utils::Error`.

These circular dependencies and layering violations made the codebase brittle, harder to reason about, and complicated the build process.

## Decision

We will enforce a strictly acyclic module graph and strict domain layering by breaking these circular dependencies and removing improper re-exports.

Specifically, we implemented the following structural changes:
1. **Remove `TxIdGenerator` Re-export:** Removed `TxIdGenerator` re-export from `api::transaction::types` and `api::transaction::mod`. `db::mod`, `db::config`, and all transaction tests now explicitly import `TxIdGenerator` directly from `core::id::TxIdGenerator`.
2. **Canonical `TxId`:** Updated `storage` to import `TxId` from `core::id` instead of `api`, breaking the `storage` <-> `api` cycle.
3. **Relocate `VectorIndexBuilder`:** Moved `VectorIndexBuilder` from `api` to `src/db/vector_builder.rs`, aligning it with the concrete implementation it serves and breaking the `api` <-> `db` cycle.
4. **Consolidate `Error`:** Moved `utils/error.rs` to `core/error.rs` and deleted the `utils` module, making `Error` a first-class citizen of the core domain and eliminating the `core` <-> `utils` cycle.

## Consequences

### Positive

- **Architectural Clarity:** The domain layering is now strictly enforced. Upper layers like `db` must import core primitives directly from `core` rather than through `api` re-exports.
- **Acyclic Dependency Graph:** The module dependency graph is now a Directed Acyclic Graph (DAG), eliminating circular references.
- **Maintainability:** Clearer boundaries reduce cognitive load and prevent unintended side effects when modifying the codebase.

### Negative

- **Refactoring Churn:** Required significant updates to import paths across multiple files, impacting ongoing development branches.

### Neutral

- **Module Structure Reorganization:** The `utils` module is completely eliminated in favor of placing utilities closer to their respective domain boundaries (e.g., `core::error`).

## References

- `.jules/atlas.md` - 2026-05-23 - Breaking Circular Dependencies
- `.jules/atlas.md` - 2026-05-24 - Unwinding API/Core Dependency
