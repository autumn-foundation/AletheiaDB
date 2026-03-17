# ADR-0061: Splitting WriteTransaction God Struct

**Status:** Accepted
**Date:** 2026-02-16
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, api, transaction

## Context

The `src/api/transaction/write_tx.rs` file had grown into a 5000-line "God Struct" module. It was responsible for:
1.  **Validation:** Input property validation and normalization.
2.  **Conflict Detection:** MVCC snapshot conflict detection logic.
3.  **WAL Logging:** Constructing, formatting, and appending Write-Ahead Log entries.
4.  **Storage Application:** Applying changes directly to the in-memory graph via the `CurrentStorage` component.
5.  **Extensive Tests:** Approximately 4000 lines of complex transaction behavior, boundary, and error tests.

This design severely violated the Single Responsibility Principle (SRP). The file was incredibly difficult to navigate, edit, and maintain. Any change to a specific feature required modifying this central bottleneck, increasing the risk of merge conflicts and reducing overall codebase transparency.

## Decision

We have decided to split the monolithic `src/api/transaction/write_tx.rs` file into a dedicated module directory `src/api/transaction/write/`.

### Key Changes

The `WriteTransaction` struct definition remains the central entry point for mutative operations, but its implementations have been extracted into cohesive, single-responsibility submodules:

1.  **`mod.rs`**: Retains the core `WriteTransaction` struct definition, implements the public `WriteOps` API, and serves as the facade.
2.  **`validation.rs`**: Extracts validation logic (e.g., verifying vector property dimensions and types).
3.  **`conflict.rs`**: Extracts MVCC conflict detection, ensuring transactions don't overwrite each other concurrently.
4.  **`apply.rs`**: Extracts storage application logic, interfacing directly with `CurrentStorage` to mutate the graph.
5.  **`wal.rs`**: Extracts WAL logging logic, translating high-level operations into durably stored sequences.
6.  **`tests.rs`**: Extracted extensive unit tests for transaction lifecycle management.

## Consequences

### Positive

-   **Enhanced Readability:** Developers can now find and modify specific functionality (e.g., MVCC conflict logic) without wading through thousands of lines of unrelated code (e.g., tests).
-   **Maintainability:** Smaller, focused files are easier to test, modify, and review, significantly reducing the cognitive load on developers.
-   **Reduced Merge Conflicts:** Changes to distinct features no longer require modifications to the same 5000-line file.
-   **Architectural Clarity:** The internal processes of a write transaction (validation -> wal -> apply) are explicitly modeled via submodule organization.

### Negative

-   **Initial Refactoring Overhead:** Moving methods and tests, updating imports, and resolving visibility modifiers across files requires significant one-time effort.

### Neutral

-   **No API Change:** The public `WriteOps` API for `WriteTransaction` remains identical; the changes are strictly internal organization.
