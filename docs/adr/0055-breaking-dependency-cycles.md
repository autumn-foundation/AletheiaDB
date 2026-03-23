# ADR-0055: Breaking Core and Query Dependency Cycles

**Status:** Proposed
**Date:** 2026-03-23
**Deciders:** Atlas, Codex
**Categories:** architecture, core, query, modularity

## Context

As AletheiaDB evolved, two significant circular dependencies emerged, violating the principles of a clean Directed Acyclic Graph (DAG) for module dependencies:

1.  **Core and Utils Cycle:** The `core` module depended on `utils::error::StorageError` and `utils::error::VectorError`, while `utils::error` simultaneously depended on types defined within `core` (e.g., `NodeId`, `VersionId`).
2.  **Query and DB Cycle:** The `query` module (specifically `hybrid.rs` and `semantic_pathfinding.rs`) directly imported and depended on `crate::db::AletheiaDB` for execution. Conversely, `db` depends on `query` for parsing and executing queries.

These structural cycles hampered isolated testing, obscured module boundaries, and increased the risk of tight coupling.

## Decision

We broke both architectural cycles using dependency inversion and domain consolidation.

### Resolving the Core <-> Utils Cycle
We extracted a new module `src/core/error.rs` containing `CoreError`. Core modules were refactored to use `CoreError` instead of reaching into `utils`. `TemporalError` was moved inside `core::temporal`. Finally, `CoreError` was integrated into the broader `utils::error::Error`. This enforces a unidirectional dependency where `utils` can depend on `core`, but `core` is entirely self-contained.

### Resolving the Query <-> DB Cycle
We introduced a `QueryExecutable` trait in `src/query/builder.rs`. This trait abstracts the capabilities required to execute a query (such as fetching nodes, properties, and searching vectors).
The `AletheiaDB` struct (in `src/db/query.rs`) now implements `QueryExecutable`. The `query` module's execution logic operates strictly against this trait instead of the concrete `AletheiaDB` type. We also moved integration-level tests that required full DB setup from the `query` module to the `tests/` directory.

## Consequences

### Positive

-   **Clean DAG:** Strict encapsulation and isolated module dependencies. `core` and `query` are now independently verifiable and testable.
-   **Testability:** Queries can now be tested by providing mock or lightweight implementations of `QueryExecutable` without spinning up the entire database engine.
-   **Architectural Clarity:** The domain logic is decoupled from the concrete database orchestration, preventing future "god object" anti-patterns.

### Negative

-   **Indirection:** Abstracting DB execution via the `QueryExecutable` trait introduces minor virtual dispatch overhead, though typically mitigated by Rust's monomorphization or because query operations are not the tightest inner loop.

### Neutral

-   **Refactoring Effort:** Required extracting error types and moving tests out of the main source tree into the integration `tests/` folder.

## References
- 🗺️ Atlas: [architectural change] Resolve circular dependency between core and utils
- 🗺️ Atlas: [fix query to db cycle]
