# ADR-0054: Enforce Strict Domain Layering for TxIdGenerator

**Status:** Accepted
**Date:** 2026-05-24
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, core, api, db, modularity

## Context

AletheiaDB's architecture is designed with clear domain layering: `core` defines the primitives and traits, `storage` implements persistence, `api` provides external interfaces, and `db` coordinates these layers.

A dependency tangle was identified where the `db` module imported `TxIdGenerator` through a re-export in `api::transaction`. This caused a layering violation because the top-level `db` module skipped `core` to rely on the `api` module for a fundamental domain primitive (transaction ID generation). This created unnecessary coupling, where the `api` layer became a conduit for `core` types to reach `db`, blurring the lines between the API interface and the core domain logic.

## Decision

We will remove the `TxIdGenerator` re-export from `api::transaction::types` and `api::transaction::mod`. We will update `db::mod`, `db::config`, and all transaction tests to explicitly import `TxIdGenerator` directly from `core::id::TxIdGenerator`.

## Consequences

### Positive

- **Cleaner Boundaries:** Enforces strict domain layering. The `api` boundary is cleaner as it no longer leaks internal implementation details of core ID generators.
- **Reduced Coupling:** `db` depends directly on `core` for primitives, reducing indirect coupling through `api`.
- **Architectural Clarity:** Aligns with the "The Bloat" and "Tangle" resolutions, maintaining an acyclic and logically layered module graph.

### Negative

- **Refactoring Overhead:** Required updates to numerous `use` statements across the `db` and test modules.

### Neutral

- **API Interface Change:** The `api::transaction` module no longer provides a one-stop-shop for all transaction-related types, requiring consumers to know about `core::id` if they need to generate IDs directly.

## Alternatives Considered

### Alternative 1: Keep the re-export for convenience

We could have kept the re-export in `api::transaction` to make it easier for external users to access all transaction types from one place.

*   **Why not:** This convenience comes at the cost of architectural purity. `TxIdGenerator` is an internal mechanism for creating IDs, not a part of the public transaction API intended for end-users. Exposing it via `api` encouraged its misuse and perpetuated the layering violation within the codebase itself.
