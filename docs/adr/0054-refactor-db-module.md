# ADR-0054: Refactor Database God Object and Break Circular Dependencies

**Status:** Accepted
**Date:** 2026-05-23
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, core, api

## Context

AletheiaDB's `src/db.rs` had grown into a 3500-line "God Object". It was responsible for database configuration, transaction management, vector indexing, temporal queries, and administrative operations. This violated the Single Responsibility Principle, making the file extremely difficult to navigate and maintain.

Simultaneously, the codebase developed several circular dependencies across boundaries:
1. `storage` and `api` had a cyclical dependency where `storage` imported `api::TxId`, but `api` depended on `storage`.
2. `api` and `db` had a cycle where `api`'s `VectorIndexBuilder` depended on `db::AletheiaDB`, while `db` relied on `api` for transactions.
3. `core` and `utils` formed a cycle because `utils` contained `Error` types that depended on `core::id`, while `core` itself depended on `utils::Error`.

These tangles inhibited proper compilation isolation and obscured the true architectural hierarchy.

## Decision

We performed a structural refactor to split the God Object and break all detected circular dependencies:

1. **Splitting the `db` Module:**
   - Refactored `src/db.rs` into a comprehensive `src/db/` directory.
   - Kept the core `AletheiaDB` struct definition in `mod.rs`.
   - Extracted distinct implementations into cohesive submodules:
     - `config.rs` (Initialization/configuration)
     - `transaction.rs` (Transaction lifecycle)
     - `ops.rs` (CRUD and graph logic)
     - `temporal.rs` (Temporal queries)
     - `vector.rs` (Vector indexing and search)
     - `query.rs` (Query builder/executor integration)
     - `admin.rs` (Maintenance and persistence)

2. **Breaking Circular Dependencies:**
   - Re-layered `TxId`: Updated `storage/index_persistence/operations.rs` to import `TxId` directly from `core::id`, removing the upstream dependency on `api`.
   - Relocated `VectorIndexBuilder`: Moved it from `api` to `db` because it serves as a concrete helper specifically for `AletheiaDB`.
   - Consolidated Errors: Moved `utils/error.rs` to `core/error.rs` and deleted the `utils` module entirely, centralizing core domain primitives and eliminating the `core` <-> `utils` loop.

## Consequences

### Positive
- **Maintainability:** Navigating the `AletheiaDB` implementation is drastically easier. Submodules have clear, single responsibilities.
- **Build Performance:** Removing circular dependencies and flattening the `utils` module improves compilation times and rust-analyzer responsiveness.
- **Clean Architecture:** `core` now acts as a true foundation with `error.rs` and `id.rs`, without relying on higher-level modules like `api` or a disjointed `utils`.

### Negative
- **API Churn:** Users and internal modules relying on `utils::Error` or `api::VectorIndexBuilder` required import updates.
- **File Sprawl:** The number of files in the project increased, which may slightly steepen the learning curve for new contributors trying to find specific `AletheiaDB` methods.

### Neutral
- No change to runtime performance or storage mechanics. The refactor is purely structural.

## Alternatives Considered

- **Keep `db.rs` but use `impl` blocks with visual separators:** Rejected because it doesn't solve file-size constraints or IDE navigation sluggishness.
- **Extracting Temporal/Vector logic into standalone crates:** Considered premature. A module-level split inside the same crate provides the necessary separation without the overhead of multi-crate publishing right now.
