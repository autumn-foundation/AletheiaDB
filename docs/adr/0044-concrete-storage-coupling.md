# ADR-0044: Concrete Storage Coupling

**Status:** Proposed
**Date:** 2026-02-01
**Deciders:** AletheiaDB Core Team
**Categories:** storage, architecture, simplification

## Context

ADR-0027 ("Decouple Storage from Core") proposed defining architectural boundaries via a `StorageEngine` trait located in the `core` module. This was intended to allow `AletheiaDB` to support pluggable storage backends (e.g., in-memory vs. persistent).

ADR-0034 ("Standardize on Redb for Cold Storage") later removed the `ColdStorage` trait in favor of a concrete `RedbColdStorage` implementation, citing premature abstraction and performance overhead.

The current implementation of the core `AletheiaDB` struct (in `src/db/mod.rs`) depends directly on concrete `CurrentStorage` and `HistoricalStorage` structs:

```rust
pub struct AletheiaDB {
    /// Current state storage (hot path) - Arc-wrapped for sharing across transactions
    pub(crate) current: Arc<CurrentStorage>,
    /// Historical version storage (temporal path) - RwLock-protected for concurrent reads
    pub(crate) historical: Arc<RwLock<HistoricalStorage>>,
    // ...
}
```

The `StorageEngine` trait proposed in ADR-0027 was never fully implemented or adopted.

## Decision

We will **formalize the use of concrete storage types** (`CurrentStorage`, `HistoricalStorage`) within the core `AletheiaDB` struct, superseding the trait-based decoupling proposed in ADR-0027.

Specific decisions:
1.  **No `StorageEngine` Trait:** We will not introduce a generic `StorageEngine` trait at this time.
2.  **Direct Dependencies:** `AletheiaDB` will continue to hold `Arc<CurrentStorage>` and `Arc<RwLock<HistoricalStorage>>` directly.
3.  **Module Separation:** We maintain the physical separation of code (storage logic in `src/storage/`), but the logical coupling remains concrete.

## Consequences

### Positive

-   **Performance:** Eliminates virtual dispatch (vtable) overhead. Calls to storage methods are statically dispatched, enabling compiler optimizations like inlining.
-   **Simplicity:** Reduces codebase complexity by removing unnecessary trait definitions, generic parameters, and associated type plumbing.
-   **YAGNI:** Avoids maintaining an abstraction layer for alternative backends that do not currently exist and are not planned for the immediate future.

### Negative

-   **Tight Coupling:** The core database logic is tightly coupled to the specific storage implementation. Replacing the storage engine would require refactoring `AletheiaDB` rather than just implementing a trait.
-   **Testing:** We cannot easily mock the entire storage engine for unit tests. Instead, we rely on the fact that `CurrentStorage` and `HistoricalStorage` are designed to be efficient enough for use in integration tests (or use in-memory configuration where applicable).

## Compliance

This decision aligns with ADR-0034 and the "Performance First" principle. It acknowledges the reality of the codebase and updates the architectural record to match.
