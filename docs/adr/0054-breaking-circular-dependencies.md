# ADR-0054: Breaking Circular Dependencies

**Status:** Accepted
**Date:** 2026-05-23
**Deciders:** Atlas, Codex
**Categories:** core, api, storage, db

## Context

Circular dependencies between fundamental modules were causing build failures and architectural tangles, making the codebase difficult to maintain and evolve. Specifically, we identified three major cycles:

1.  **`storage` <-> `api` cycle:** `storage` imported `api::TxId` to represent transaction identifiers, while the `api` module intrinsically depends on `storage` to execute those transactions.
2.  **`api` <-> `db` cycle:** `api` contained the `VectorIndexBuilder` which directly depended on `db::AletheiaDB` for graph access, while the `db` module depends on `api` to provide transaction boundaries and builder patterns.
3.  **`core` <-> `utils` cycle:** The `utils` module contained a generic `Error` type which depended on `core::id` for specific error variants, while the `core` module itself depended on `utils::Error` for its fallible operations.

These cycles violated the principle of acyclic dependencies, blurring the boundaries between domains and leading to a "tangled" architecture.

## Decision

We will break these dependency cycles by realigning types with their appropriate domain boundaries:

1.  **Move `TxId`:** We will update `storage/index_persistence/operations.rs` to import `TxId` from `core::id` instead of `api`. This correctly positions the transaction identifier as a core domain primitive rather than an API-level construct.
2.  **Move `VectorIndexBuilder`:** We will move `VectorIndexBuilder` from the `api` module to the `db` module. Since it is a concrete helper specifically designed to work with `AletheiaDB`, it belongs in the `db` domain.
3.  **Consolidate Errors:** We will move `utils/error.rs` to `core/error.rs` and completely delete the `utils` module. This consolidates error handling into the core domain, eliminating the artificial boundary and cycle.

## Consequences

### Positive

-   **Architectural Clarity:** Restores a unidirectional dependency flow (`db` -> `api` -> `storage` -> `core`).
-   **Improved Build Times:** Breaking cycles allows Cargo to compile crates/modules more efficiently in parallel.
-   **Increased Cohesion:** Types are now located in the modules that define their primary domain (e.g., `TxId` in `core::id`).

### Negative

-   **Refactoring Overhead:** Requires widespread updates to import paths across the entire codebase.

## Alternatives Considered

-   **Intermediate Crates:** We considered extracting shared types (like `TxId` and `Error`) into a new `common` or `types` crate. However, this was rejected as it introduces unnecessary complexity and fragmentation; these types naturally belong in `core`.

## References

-   Architecture Blueprints (`.jules/atlas.md` entry for 2026-05-23)
