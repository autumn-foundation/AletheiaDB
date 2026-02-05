# ADR-0034: Standardize on Redb for Cold Storage

**Status:** Accepted
**Date:** 2026-01-28
**Deciders:** GallifreyDB Core Team
**Categories:** storage, architecture, simplification

## Context

ADR-0027 ("Decouple Storage from Core") proposed defining architectural boundaries via storage traits (e.g., `StorageEngine`) to allow for pluggable backends. Following this, a `ColdStorage` trait was introduced to abstract the cold storage layer, with `RedbColdStorage` and `FileColdStorage` as implementations.

However, as development progressed, several issues emerged with this abstraction:

1.  **Premature Abstraction:** `Redb` (Rust Embedded Database) has proven to be the only viable embedded backend that meets our performance and ACID requirements. The `FileColdStorage` implementation was limited and rarely used.
2.  **Performance Overhead:** The trait required dynamic dispatch (`Arc<dyn ColdStorage>`), preventing compiler optimizations like inlining and specialization, which are critical for the "Performance First" principle (<1µs targets).
3.  **Complexity:** `TieredStorage` and `MigrationService` had to deal with trait objects, complicating the type system and error handling.
4.  **Leakiness:** The `ColdStorage` trait had to expose `redb`-specific types or concepts (like LSNs) to support the WAL architecture, making the abstraction leaky.

## Decision

We will **remove the `ColdStorage` trait** and standardize on `RedbColdStorage` as the sole concrete implementation for cold storage.

Specific changes:
1.  **Remove Trait:** Delete the `ColdStorage` trait definition.
2.  **Concrete Types:** Update `TieredStorage`, `MigrationService`, and `HistoricalStorage` to own `Arc<RedbColdStorage>` directly instead of `Arc<dyn ColdStorage>`.
3.  **Remove Dead Code:** Delete `FileColdStorage` and any other unused implementations.

This decision refines ADR-0027 by favoring **concrete modularity** (separate modules/crates but concrete types) over **abstract modularity** (traits) for the storage layer.

## Consequences

### Positive

-   **Performance:** Removes virtual dispatch overhead. Calls to storage are now static dispatch, allowing inlining.
-   **Simplicity:** Codebase is easier to read and navigate. `TieredStorage` logic is more straightforward.
-   **Robustness:** `RedbColdStorage` provides ACID guarantees that ad-hoc file implementations lacked.
-   **Maintainability:** We focus all optimization efforts on a single, high-quality storage backend.

### Negative

-   **Reduced Pluggability:** Swapping the storage engine now requires code changes rather than just a configuration switch. However, given GallifreyDB's embedded nature, supporting multiple distinct storage engines simultaneously is a non-goal (YAGNI).
-   **Tight Coupling:** The system is now explicitly coupled to `redb`. If `redb` becomes unmaintained, replacing it will be harder (though still manageable due to module boundaries).

## Compliance

This ADR aligns with the "Razor" persona's directive to "Enforce KISS/YAGNI" and "prefer concrete types over single-impl traits".
