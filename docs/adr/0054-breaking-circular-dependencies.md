# ADR-0054: Breaking Circular Dependencies

**Status:** Proposed
**Date:** 2026-05-23
**Deciders:** Atlas, Codex
**Categories:** architecture, core, refactoring

## Context

AletheiaDB had developed circular dependencies across several core modules that violated strict layering principles:
1. `storage` <-> `api` cycle: `storage` imported `api::TxId`, while `api` depended on `storage`.
2. `api` <-> `db` cycle: `api` contained `VectorIndexBuilder` which depended on `db::AletheiaDB`, while `db` depended on `api` for transactions.
3. `core` <-> `utils` cycle: `utils` contained `Error` which depended on `core::id`, while `core` depended on `utils::Error`.

These circular dependencies complicated build times, testing, and made reasoning about module boundaries difficult. The `utils` module became a catch-all bin, diluting the focus of the `core` domain.

## Decision

We broke the circular dependencies by strict relocation and boundary enforcement:
1.  **API/Storage Unwinding:** We updated `storage/index_persistence/operations.rs` to import `TxId` directly from `core::id` instead of `api`. This breaks the cycle, allowing `storage` to depend solely on `core` primitives.
2.  **VectorIndexBuilder Relocation:** We moved `VectorIndexBuilder` from `api` to `db`. Since it is a concrete helper explicitly designed for configuring an `AletheiaDB` instance, it belongs in the `db` module.
3.  **Consolidating Errors:** We moved `utils/error.rs` to `core/error.rs` and deleted the `utils` module entirely. This consolidates all domain types and error definitions under `core`, eliminating the cycle with `utils`.

## Consequences

### Positive

-   **Clearer Boundaries:** Module dependencies now flow unidirectionally: `db` -> `api` -> `storage` -> `core`.
-   **Stronger Domain:** `core` now fully encapsulates errors alongside other domain primitives.
-   **Simpler Architecture:** Removing the ambiguous `utils` module forces functionality to be placed in the proper domain context.

### Negative

-   **Refactoring Cost:** A large number of files needed updates to fix `Error` and `TxId` imports.

### Neutral

-   `VectorIndexBuilder`'s relocation means users now import it from `db` instead of `api`.
