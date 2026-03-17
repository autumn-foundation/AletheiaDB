# ADR-0057: Split HistoricalStorage Module

**Status:** Accepted
**Date:** 2026-01-30
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, storage, historical, modularity

## Context

The `src/storage/historical.rs` file had grown into a massive 7000-line "Blob" module. It contained both the complex core logic for managing historical data in the storage layer (caching, tiering, reconstructions) and approximately 4400 lines of mixed tests. This made the file incredibly difficult to navigate, edit, and maintain, severely impacting developer experience and architectural transparency.

## Decision

We have decided to split the monolithic `src/storage/historical.rs` file into a dedicated module directory `src/storage/historical/`.

### Key Changes

1.  **Extract Tests:** All test code (~4400 lines) was moved into a separate file `src/storage/historical/tests.rs`.
2.  **Retain Core Logic:** The primary storage logic (~2600 lines) remains in `src/storage/historical/mod.rs`, which acts as the module's core implementation and facade.

## Consequences

### Positive

-   **Enhanced Readability:** Developers can now focus entirely on the core business logic of the historical storage layer without scrolling past thousands of lines of tests.
-   **Improved Maintainability:** Test failures or modifications are contained within their own file, reducing the risk of accidental changes to the production code during test updates.
-   **Clearer Structure:** The separation of concerns clarifies the boundary between implementation and verification.

### Negative

-   **Test Isolation Limitations:** Tests in `tests.rs` may require explicit visibility modifiers (`pub(crate)`) on internal functions that were previously accessible when tests were in the same module file.

### Neutral

-   **Core Logic Still Large:** While significantly reduced, the `mod.rs` file still contains ~2600 lines of complex storage logic, which may require further decomposition in the future (e.g., separating cache management, reconstruction logic, and tiering interactions).
