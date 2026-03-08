# ADR-0055: The Blob in Temporal Vector Index

**Status:** Proposed
**Date:** 2026-01-29
**Deciders:** Atlas
**Categories:** Architecture, Refactoring, Index

## Context

The `src/index/vector/temporal.rs` file had grown into a "Blob" anti-pattern. Spanning over 1400 lines of code, the file had accumulated mixed responsibilities, handling configuration, snapshot logic, statistics tracking, the `StorageObserver` implementation, and the core temporal vector index implementation.

This mixing of concerns within a single monolithic file made the module difficult to read, maintain, and navigate. The sheer size of the file violated the Single Responsibility Principle and hindered collaboration. A refactoring effort was necessary to extract these concerns into separate, cohesive sub-modules.

## Decision

We will refactor `src/index/vector/temporal.rs` by splitting its mixed responsibilities into a cohesive `src/index/vector/temporal/` directory structure.

1. **Extract Configuration**: We will extract the `TemporalIndexConfig` and related configuration structs to a dedicated `config.rs` module.
2. **Extract Snapshot Logic**: We will move the internal `Snapshot` and `VectorSnapshot` structures and management to an internal `snapshot.rs` module.
3. **Extract Statistics**: We will extract `IndexStats` and tracking logic to a `stats.rs` module.
4. **Extract Observer**: We will move the `StorageObserver` trait implementation to an `observer.rs` module.
5. **Core Logic in `mod.rs`**: We will keep the core indexing algorithms and test cases in `mod.rs` for now, significantly reducing its noise and surface area.

## Consequences

### Positive

- **Maintainability**: Smaller, focused files are easier to navigate and maintain.
- **Separation of Concerns**: Each file now has a single responsibility (e.g., config, stats, core implementation), adhering to SOLID principles.
- **Readability**: By moving supporting structures and implementations out of the main file, the core temporal index logic in `mod.rs` is much easier to digest.

### Negative

- **Module Fragmentation**: The code is spread across more files, which requires a slight cognitive shift for developers used to the single-file layout.

### Neutral

- Tests remain in `mod.rs` for now, but may be extracted in a future refactoring step (e.g., `tests.rs`).

## Implementation Notes

These refactorings were identified and executed by the Atlas persona to fix the "Blob" structural pattern and improve the index module's cohesion.
