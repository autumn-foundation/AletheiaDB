# ADR-0059: Consolidating Versioning Logic

**Status:** Accepted
**Date:** 2026-02-01
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, core, storage, versioning

## Context

The system's versioning logic, the fundamental data structures representing bi-temporal data, was split between two locations:
1.  `src/core/version.rs`: Contained version metadata and identifiers.
2.  `src/storage/version.rs`: Contained the actual data definitions for versions (e.g., `NodeVersion`, `EdgeVersion`).

This division created a false architectural boundary and significant confusion. `NodeVersion` and `EdgeVersion` are core domain primitives, not storage-specific implementation details. The `storage` module, intended to be an implementation detail (ADR-0027), was leaking core domain types back into the rest of the system. This architectural smell ("The Leak") made it difficult to reason about where domain logic belonged versus storage logic.

## Decision

We have decided to **consolidate all versioning logic into `src/core/version.rs`**.

### Key Changes:
1.  **Move Domain Primitives:** All data structures representing versions (e.g., `NodeVersion`, `EdgeVersion`) and their associated logic have been moved from `src/storage/version.rs` to `src/core/version.rs`.
2.  **Delete Storage Version Module:** The `src/storage/version.rs` file has been completely removed.
3.  **Backward Compatibility Re-export:** To prevent immediate breakages across the codebase and allow a gradual migration path, `src/storage/mod.rs` temporarily re-exports the version types from `core::version`.

## Consequences

### Positive

-   **Strengthened Boundaries:** This change correctly positions `core` as the sole owner of domain definitions (the "what") and `storage` strictly as the implementation layer (the "how").
-   **Elimination of Architectural Leakage:** Core domain primitives are no longer defined in or exported from the implementation layer, resolving the architectural confusion.
-   **Cohesion:** All versioning-related metadata and data structures reside in a single, cohesive location (`core::version`), simplifying navigation and reasoning.

### Negative

-   **Temporary Re-exports:** The `storage` module still re-exports `core::version` types for backward compatibility, which could prolong the transition period before all modules correctly import from `core`.

### Neutral

-   **Refactoring Effort:** Required significant changes to imports and visibility modifiers across the codebase to resolve compiler errors.
