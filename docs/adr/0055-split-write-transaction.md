# ADR-0055: Splitting WriteTransaction God Struct

**Status:** Accepted
**Date:** 2026-02-16
**Deciders:** AletheiaDB Core Team
**Categories:** transaction, api

## Context

The `WriteTransaction` logic located in `src/api/transaction/write_tx.rs` had grown into a 5000-line "God Struct" over time. It handled validation, MVCC conflict detection, Write-Ahead Log (WAL) logging, storage application, and extensive tests all within a single file. This violated the Single Responsibility Principle and made navigation, comprehension, and maintenance difficult. Any change to a specific part of the transaction logic required modifying this massive, central file.

## Decision

We have refactored the `WriteTransaction` logic by splitting the 5000-line "God Struct" into a cohesive `src/api/transaction/write/` directory.

The responsibilities have been isolated into the following submodules:
1. `mod.rs`: Defines the `WriteTransaction` struct and its public API.
2. `validation.rs`: Extracted validation logic.
3. `conflict.rs`: Extracted MVCC conflict detection.
4. `apply.rs`: Extracted storage application logic.
5. `wal.rs`: Extracted WAL logging logic.
6. `tests.rs`: Moved all tests (~4000 lines) to a separate file.

## Consequences

### Positive

- **Improved Readability and Maintainability:** Each module now has a single, well-defined responsibility.
- **Easier Navigation:** Developers can quickly locate the code related to specific transaction phases (e.g., conflict resolution vs. WAL logging) without scrolling through thousands of lines of unrelated code.
- **Test Isolation:** The ~4000 lines of tests are now separated from the implementation logic, reducing the noise when examining the core algorithms.
- **Reduced Merge Conflicts:** Changes to validation logic are less likely to conflict with changes to WAL logging, as they are now in separate files.

### Negative

- **Internal Visibility:** Some private methods or fields of `WriteTransaction` may need to be exposed as `pub(crate)` or `pub(super)` to be accessible by the new submodules.

### Neutral

- **Module Structure:** The `src/api/transaction/write/` directory now contains several smaller files instead of one large file, requiring developers to familiarize themselves with the new layout.
