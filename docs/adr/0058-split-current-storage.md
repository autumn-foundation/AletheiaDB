# ADR-0058: Split CurrentStorage Module

**Status:** Accepted
**Date:** 2026-02-01
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, storage, modularity, current

## Context

The `src/storage/current.rs` file had grown into a "Blob" module. It contained the core storage implementation (DashMap collections, locking logic), iterators for graph traversal, statistics tracking, vector index helpers, and extensive tests. This mixed-responsibility design made the code difficult to navigate, read, and maintain, violating the Single Responsibility Principle and reducing overall codebase transparency.

## Decision

We have decided to split the monolithic `src/storage/current.rs` file into a dedicated module directory `src/storage/current/`.

### Key Changes

The module is decomposed into cohesive, single-responsibility files:
1.  **Extract Iterators:** Iteration logic for scanning nodes and edges was extracted to `src/storage/current/iterators.rs`.
2.  **Extract Statistics:** Storage statistics and metrics were extracted to `src/storage/current/stats.rs`.
3.  **Extract Vector Helpers:** Vector indexing helper logic and configurations were extracted to `src/storage/current/vector.rs`.
4.  **Extract Tests:** All test code was moved to a separate file `src/storage/current/tests.rs`.
5.  **Clean Facade:** The `mod.rs` file now serves as a clean facade, retaining the core storage struct definition (`CurrentStorage`) and basic access methods, and exporting the submodules.

## Consequences

### Positive

-   **Improved Readability:** Separating distinct concerns into dedicated files makes it easier for developers to find and comprehend specific logic, such as iterators or vector index interactions.
-   **Maintainability:** Smaller, focused files are easier to test and modify without causing unintended side effects in unrelated areas.
-   **Test Isolation:** Test failures or modifications are contained within their own file, reducing the risk of accidental changes to the production code during test updates.
-   **Clearer Boundaries:** Enforces structural boundaries between data access logic, traversal mechanisms, metrics, and index integration.

### Negative

-   **Initial Refactoring Overhead:** Moving code across files and updating imports requires a one-time effort.
-   **File Proliferation:** Increases the total number of files in the repository, though they are logically grouped.

### Neutral

-   **No Functional Change:** The core behavior of the current storage layer, including its performance characteristics and memory structures, remains identical.
