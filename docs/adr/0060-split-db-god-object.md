# ADR-0060: Splitting the AletheiaDB God Object

**Status:** Accepted
**Date:** 2026-05-23
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, db, modularity

## Context

The `src/db.rs` file had grown into a massive 3500-line "God Object". It was responsible for virtually every top-level operation in the system:
1.  Initialization and configuration parsing.
2.  Transaction lifecycle management (read/write).
3.  Basic CRUD and graph operations (nodes, edges, properties).
4.  Temporal query execution.
5.  Vector index management and search (HNSW).
6.  Query builder and query execution integration.
7.  Admin operations (maintenance, persistence, statistics).
8.  Extensive unit tests.

This design severely violated the Single Responsibility Principle (SRP). The file was incredibly difficult to navigate, edit, and maintain. Any change to a specific feature required modifying this central bottleneck, increasing the risk of merge conflicts and reducing overall codebase transparency.

## Decision

We have decided to split the monolithic `src/db.rs` file into a dedicated module directory `src/db/`.

### Key Changes

The `AletheiaDB` struct definition remains the central entry point, but its implementations have been extracted into cohesive, single-responsibility submodules:

1.  **`mod.rs`**: Retains the core `AletheiaDB` struct definition and serves as the facade, exporting the required APIs.
2.  **`config.rs`**: Extracts initialization, configuration loading, and teardown logic.
3.  **`transaction.rs`**: Extracts transaction lifecycle management (e.g., `db.read()`, `db.write()`).
4.  **`ops.rs`**: Extracts basic graph CRUD operations.
5.  **`temporal.rs`**: Extracts temporal-specific querying logic.
6.  **`vector.rs`**: Extracts vector index management and search logic.
7.  **`query.rs`**: Extracts integration with the query builder and executor.
8.  **`admin.rs`**: Extracts administrative logic (maintenance tasks, statistics, explicit persistence).
9.  **`tests.rs`**: Extracted unit tests for the db functionality.

## Consequences

### Positive

-   **Enhanced Readability:** Developers can now find and modify specific functionality (e.g., vector search) without wading through thousands of lines of unrelated code (e.g., admin operations).
-   **Maintainability:** Smaller, focused files are easier to test, modify, and review, significantly reducing the cognitive load on developers.
-   **Reduced Merge Conflicts:** Changes to distinct features (like temporal queries vs. vector indexing) no longer require modifications to the same file.
-   **Architectural Clarity:** The `AletheiaDB` struct's responsibilities are explicitly modeled via submodule organization.

### Negative

-   **Initial Refactoring Overhead:** Moving methods and tests, updating imports, and resolving visibility modifiers across files requires significant one-time effort.
-   **File Proliferation:** Increases the total number of files in the repository.

### Neutral

-   **No API Change:** The public API of `AletheiaDB` remains identical; the changes are strictly internal organization.
