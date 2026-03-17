# ADR-0063: Unwinding API/Core Dependency

**Status:** Accepted
**Date:** 2026-05-24
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, api, dependencies

## Context

The `db` module imported the `TxIdGenerator` domain primitive through an `api::transaction` re-export rather than directly from `core::id`. This created a subtle but significant layering violation: `db` skipped the direct `core` boundary to rely on an `api` module for a fundamental domain type.

This intermediate re-export created unnecessary coupling between the `api` layer and `core`'s ID generation responsibilities. It blurred the architectural boundary where `api` should expose operations on data, while `core` defines the data's identity and creation rules.

## Decision

We have decided to eliminate the `TxIdGenerator` re-export from the `api` module and force direct dependencies on the `core` layer for this domain primitive.

### Key Changes:

1.  **Removed Re-export:** Removed `pub use crate::core::id::TxIdGenerator` from `api::transaction::types` and `api::transaction::mod`.
2.  **Explicit Imports:** Updated `db::mod`, `db::config`, and all related transaction tests to explicitly import `TxIdGenerator` directly from `core::id::TxIdGenerator`.

## Consequences

### Positive

-   **Strict Layering:** Enforces a clean separation between the API layer (transaction orchestration) and the Core domain layer (identity generation). The `db` module now explicitly acknowledges its dependency on both layers independently.
-   **Reduced Coupling:** The `api` module is no longer responsible for vending domain primitives it does not own or operate upon directly, reducing its API surface area.

### Negative

-   **Refactoring Effort:** Required updating import statements across several internal modules and extensive test suites.

### Neutral

-   **No Functional Change:** The underlying implementation and behavior of the `TxIdGenerator` remains identical.
