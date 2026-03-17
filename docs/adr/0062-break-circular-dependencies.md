# ADR-0062: Breaking Circular Dependencies

**Status:** Accepted
**Date:** 2026-05-23
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, modularity, dependencies

## Context

The system suffered from severe dependency tangles and circular references across multiple core modules, violating layering principles and increasing compilation times and architectural complexity:

1.  **`storage` <-> `api` cycle:** `storage` imported `api::TxId` for transaction identification, while `api` fundamentally depends on `storage` to execute transactions. This created a circular logical dependency where the implementation layer relied on its consumer.
2.  **`api` <-> `db` cycle:** `api` contained the `VectorIndexBuilder` which depended on `db::AletheiaDB` for configuration and state, while `db` depends on `api` for high-level transaction interfaces. This mixed orchestration logic with low-level API concerns.
3.  **`core` <-> `utils` cycle:** `utils` contained `Error` types which depended heavily on `core::id` types for context (e.g., `NodeId`, `EdgeId`), while almost every module in `core` depended on `utils::Error` for error handling.

## Decision

We have decided to aggressively break these circular dependencies to restore a strict, unidirectional dependency graph (`db` -> `api` -> `storage` -> `core` -> `utils`).

### Key Changes:

1.  **`storage` Layering:** `storage/index_persistence/operations.rs` now explicitly imports `TxId` from `core::id` instead of routing through `api`. This ensures `storage` remains independent of its consumer (`api`).
2.  **`VectorIndexBuilder` Relocation:** The `VectorIndexBuilder` has been moved from `api` to `db`. This correctly positions it as an orchestration component that utilizes the `AletheiaDB` engine, resolving the circularity and separating API transaction primitives from higher-level builder patterns.
3.  **Error Consolidation:** `utils/error.rs` was moved to `core/error.rs` and the `utils` module was deleted. This consolidates domain-specific error types alongside the domain primitives they reference, eliminating the `core` <-> `utils` cycle entirely.

## Consequences

### Positive

-   **Unidirectional Dependencies:** The architecture now strictly enforces layering boundaries, preventing logically invalid circular imports.
-   **Cohesion:** Domain errors reside within the `core` domain, and high-level builders reside within the top-level orchestration layer (`db`).
-   **Maintainability:** Reduced coupling between layers significantly decreases the risk of modifying one component accidentally breaking another.

### Negative

-   **Widespread Refactoring:** Relocating core error types required extensive import updates and visibility changes across nearly every file in the project.

### Neutral

-   **No Functional Change:** The system's behavior, performance, and correctness remain identical; the changes are purely structural and architectural.
