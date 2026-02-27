# ADR-0054: Decouple Storage from Core

**Status:** Proposed
**Date:** 2026-04-10
**Deciders:** Codex (Architecture Enforcement)
**Categories:** architecture, storage, core, modularity

## Context

AletheiaDB's initial monolithic architecture placed both core business logic (graph topology, query execution, temporal reasoning) and storage implementation (WAL, page management, serialization) within the same compilation unit.

As the system has grown, several issues have emerged:

1.  **Circular Dependencies:** The `core` logic frequently references `storage` types for persistence, while `storage` needs `core` types (like `Node`, `Edge`) for serialization. This tight coupling makes refactoring difficult and introduces circular dependency risks.
2.  **Build Times:** Any change in the storage layer (e.g., modifying the WAL format) triggers a rebuild of the entire core, slowing down development cycles.
3.  **Testing Complexity:** Unit testing core logic requires mocking complex storage interactions or spinning up temporary files, leading to slower and flaky tests.
4.  **Pluggability:** We want to support multiple storage backends (e.g., in-memory for testing, Redb for embedded, remote for distributed), but the current design assumes a specific storage implementation.

## Decision

We will **decouple the storage logic from the core domain** by moving all persistence-related code into a dedicated `storage` module.

The architectural boundary will be defined by separating pure domain logic into `src/core` and concrete persistence implementations into `src/storage`. `src/db` acts as the composition root that wires the database together. (Note: As refined in ADR-0044, to avoid dynamic dispatch overhead, `AletheiaDB` uses concrete storage types rather than generic trait bounds for its primary engine.)

**Key Changes:**
1.  **Module Restructuring:**
    *   `src/core/` - Pure domain logic, identifiers, temporal primitives, graph elements, and vector utilities.
    *   `src/storage/` - Concrete implementations for persistence (CurrentStorage, HistoricalStorage, WAL, Redb, sharding, and caching).
    *   `src/db.rs` (or equivalent composition point) - Wires `AletheiaDB` to specific `src/storage` types.
2.  **Dependency Inversion:** `Core` no longer depends on concrete storage implementations. `Storage` depends on `Core` for domain types (`Node`, `Edge`, `PropertyValue`).

## Consequences

### Positive

-   **Clearer Boundaries:** Enforces strict separation of concerns. `Core` focuses on "what" (graph logic), `Storage` focuses on "how" (bytes on disk/memory).
-   **Improved Build Times:** Changes to storage internals do not force recompilation of the entire core API layer (where possible).
-   **Testability:** `Core` logic can be tested more easily in isolation without pulling in complex I/O dependencies.

### Negative

-   **FFI Complexity:** If we move to separate dynamic libraries or cross-language boundaries later, the interface complexity increases.
-   **Refactoring Effort:** Required a significant one-time effort to move files and break existing cycles.

### Neutral

-   The decision to use concrete types over traits for the main storage engine (to ensure peak performance via static dispatch) means swapping backends requires changing the `AletheiaDB` struct definition, possibly using feature flags, as documented in ADR-0044.