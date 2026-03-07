# ADR-0055: Core Modularization and Consolidation

**Status:** Proposed
**Date:** 2026-03-07
**Deciders:** Atlas, Codex
**Categories:** architecture, modularity, core, index, vector, hnsw, version

## Context

As the project evolved, several key components grew into "Blob" and "God Object" anti-patterns. These monolithic files mixed configuration, statistics, persistence, and core logic, creating significant maintenance overhead and making it difficult to understand the code structure.

1.  **The Bloat in Vector Module:** `src/core/vector.rs` had swelled to over 5,000 lines of code. It mixed types, metric functions, sparse vector operations, SIMD optimizations, operations logic, validation, and thousands of lines of tests.
2.  **Fragmented Versioning:** Versioning logic was awkwardly split between `core::version` (metadata) and `storage::version` (data implementation), creating a false boundary. This setup caused `storage` to improperly define and export core domain primitives like `NodeVersion`.
3.  **HNSW Index Blob:** `src/index/vector/hnsw.rs` became a 2,000+ line module mixing configuration, persistence, statistics, and core indexing logic, while also suffering from a circular dependency between the index struct and its builder.
4.  **Temporal Vector Index Blob:** `src/index/vector/temporal.rs` grew to 1,400+ lines, mixing snapshot logic, configuration, and observer mechanics.

These issues violated the Single Responsibility Principle, hindered code navigation, and contributed to build and test sluggishness.

## Decision

We will systematically modularize these monolithic structures by breaking them into cohesive, directory-based modules and consolidating scattered domain logic into its proper domain layer.

1.  **Vector Modularization:** Refactored `src/core/vector.rs` into a `src/core/vector/` directory containing clean submodules: `types.rs`, `constants.rs`, `metric.rs`, `sparse.rs`, `simd.rs`, `ops.rs`, `validation.rs`, and `tests.rs`. A `mod.rs` was introduced to transparently maintain the original public API.
2.  **Consolidating Versioning in Core:** Merged `src/storage/version.rs` into `src/core/version.rs`, centralizing all versioning logic in the core domain. The `storage` module now correctly re-exports `core::version` to maintain backward compatibility.
3.  **Modularizing HNSW Index:** Refactored `src/index/vector/hnsw.rs` into a `src/index/vector/hnsw/` directory with `config.rs`, `persistence.rs`, `stats.rs`, `tests.rs`, and `mod.rs`. We broke the circular dependency by introducing an internal `HnswIndex::new_internal` method.
4.  **Modularizing Temporal Vector Index:** Split `src/index/vector/temporal.rs` into `src/index/vector/temporal/`, extracting `config.rs`, `snapshot.rs`, `stats.rs`, and `observer.rs`.

## Consequences

### Positive

-   **High Cohesion, Low Coupling:** Components are now grouped by concern (types, ops, validation, etc.), making the architecture easier to understand and extend.
-   **Improved Navigation:** Smaller, single-purpose files dramatically reduce the cognitive load when searching for or modifying code.
-   **Domain Correctness:** Consolidating versioning inside `core` ensures that `storage` relies on domain primitives rather than defining them, strengthening our architectural layering.
-   **No API Breakage:** Using `mod.rs` facades allowed us to refactor internals without breaking external consumers of the modules.

### Negative

-   **Refactoring Cost:** A significant one-time effort was required to extract code, fix import paths, and reorganize test suites.
-   **More Files:** The sheer number of files has increased, though each file is more focused.

### Neutral

-   **Test Relocation:** Massive test blocks (like the 2,800+ lines in `vector/tests.rs`) were relocated but remain intact. Future efforts could further partition these tests.

## References

-   `.jules/atlas.md` - 2026-01-29 - The Blob in Temporal Vector Index
-   `.jules/atlas.md` - 2026-02-01 - Consolidating Versioning
-   `.jules/atlas.md` - Refactor vector module to fix "The Bloat" (#675)
-   `.jules/atlas.md` - Modularize HNSW Index (#1454)
