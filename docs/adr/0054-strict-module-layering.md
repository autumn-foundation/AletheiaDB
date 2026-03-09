# ADR-0054: Strict Module Layering

**Status:** Proposed
**Date:** 2026-05-24
**Deciders:** Atlas, Codex
**Categories:** architecture, modularity, core

## Context

During a recent architectural review (Atlas 2026-05-23, 2026-05-24), several circular dependencies and layering violations were discovered across the AletheiaDB codebase:

1.  **Storage <-> API Cycle:** The `storage` module imported `api::TxId` for transaction identification, while the `api` module inherently depends on `storage` to persist transactions.
2.  **API <-> DB Cycle:** The `api` module contained the `VectorIndexBuilder`, which took a dependency on the high-level `db::AletheiaDB` facade. At the same time, `db` depends on `api` to orchestrate transactions.
3.  **Core <-> Utils Cycle:** A legacy `utils` module contained `Error` definitions that depended on `core::id` types, while `core` itself depended on `utils::Error`.
4.  **API/Core Layering Violation:** The `db` module was importing the domain primitive `TxIdGenerator` via a re-export in `api::transaction`. This allowed `db` to bypass `core` and rely on an `api` re-export for core ID generation responsibilities, coupling the `api` module to `core`'s ID generation.

These tangled dependencies create fragile boundaries, making the codebase harder to reason about, test, and refactor. They violate the principle of strict unidirectional data flow and logical layering.

## Decision

We will enforce strict, unidirectional module layering in AletheiaDB: `core` <- `storage` <- `api` <- `db`.

To resolve the specific tangles:

1.  **Storage -> Core:** `storage` must only depend on `core`. We updated `storage/index_persistence/operations.rs` to import `TxId` directly from `core::id` instead of `api`.
2.  **API -> Storage -> Core:** The `VectorIndexBuilder` was moved from `api` to `db`, as it is a concrete helper tied to the `AletheiaDB` facade, breaking the `api` -> `db` cycle.
3.  **Consolidate Utils into Core:** The `utils/error.rs` file was moved to `core/error.rs`, and the `utils` module was deleted. Core domain types and errors now reside cohesively in `core`.
4.  **Remove Transitive Re-exports:** We removed the `TxIdGenerator` re-export from `api::transaction`. The `db` module and all tests must now explicitly import `TxIdGenerator` directly from `core::id::TxIdGenerator`, preserving strict layering.

## Consequences

### Positive

-   **Clear Architectural Boundaries:** The system now follows a strict, understandable dependency hierarchy (`db` depends on `api`, which depends on `storage`, which depends on `core`).
-   **No Circular Dependencies:** The build graph is acyclic, improving compilation times and preventing infinite loops in module resolution.
-   **High Cohesion:** Core domain primitives (like Errors and IDs) are now consolidated in the `core` module.

### Negative

-   **Verbose Imports:** Modules at the top of the stack (like `db`) must now explicitly import primitives from the bottom of the stack (like `core::id::TxIdGenerator`) rather than relying on convenient re-exports in intermediate layers.

### Neutral

-   **Refactoring Effort:** Required moving files, renaming imports, and updating tests across the codebase.

## References

-   Atlas Architectural Review: 2026-05-23 (Breaking Circular Dependencies)
-   Atlas Architectural Review: 2026-05-24 (Unwinding API/Core Dependency)
