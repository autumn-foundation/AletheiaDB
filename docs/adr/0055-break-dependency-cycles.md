# ADR-0055: Break Dependency Cycles

**Status:** Accepted
**Date:** 2026-05-23
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, dependency

## Context
Circular dependencies were identified that violate layering principles and cause maintenance friction:
1. `storage` <-> `api` cycle: `storage` imported `api::TxId`, while `api` depends on `storage`.
2. `api` <-> `db` cycle: `api` contained `VectorIndexBuilder` which depended on `db::AletheiaDB`, while `db` depends on `api` for transactions.
3. `core` <-> `utils` cycle: `utils` contained `Error` which depended on `core::id`, while `core` depended on `utils::Error`.

## Decision
We refactored the modules to establish a stricter dependency graph:
1. Updated `storage/index_persistence/operations.rs` to import `TxId` from `core::id` instead of `api`.
2. Moved `VectorIndexBuilder` from `api` to `db`, as it is a concrete helper for `AletheiaDB`.
3. Moved `utils/error.rs` to `core/error.rs` and deleted the `utils` module, consolidating core domain types.

## Consequences

### Positive
- A cleaner, decoupled architecture breaking the circular dependency loops.
- Clearer semantic boundaries for domain primitives like `TxId` and `Error`.
- Improved maintainability by co-locating related concepts.

### Negative
- Breaking change for any code relying on `utils::Error` or `api::VectorIndexBuilder`, requiring updates to imports.
