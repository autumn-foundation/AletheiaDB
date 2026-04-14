# ADR-0044: Concrete Storage Coupling

**Status:** Accepted
**Date:** 2026-03-24
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, storage, performance

## Context

ADR-0027 proposed decoupling the storage logic from the core domain by defining `StorageEngine` traits in `src/core` and implementing them in `src/storage`. The intent was to allow pluggable storage backends (e.g., in-memory vs Redb) and avoid circular dependencies.

While the module-level decoupling was successfully implemented (core types do not depend on storage implementation), the original plan to have the `AletheiaDB` struct (in `src/db`) hold generic `Box<dyn StorageEngine>` or heavily genericized types introduced significant complexity:

1.  **Dynamic Dispatch Overhead:** Virtual method calls (vtables) inhibit compiler optimizations like inlining, which are critical for the "Performance First" goal of <1µs traversal times.
2.  **Generic Viral Propagation:** Using static dispatch (generics) would require propagating `<S: StorageEngine>` throughout the entire codebase (query planner, traverser, API), significantly increasing compilation times and code complexity.
3.  **YAGNI (You Aren't Gonna Need It):** The primary use case for AletheiaDB is as an embedded library with a specific, optimized storage engine. The need for runtime-swappable backends is theoretical and currently unused.

## Decision

We will **couple the `AletheiaDB` struct directly to concrete storage types** (`CurrentStorage` and `HistoricalStorage`).

The architecture remains modular:
-   `src/core`: Pure domain logic, no storage dependencies.
-   `src/storage`: Concrete implementations of storage logic, depending on `src/core`.
-   `src/db`: Composition root that wires `AletheiaDB` to specific `src/storage` types.

The `AletheiaDB` struct will look like this:

```rust
pub struct AletheiaDB {
    pub(crate) current: Arc<CurrentStorage>,
    pub(crate) historical: Arc<RwLock<HistoricalStorage>>,
    // ...
}
```

Instead of:

```rust
pub struct AletheiaDB<S: StorageEngine> {
    pub(crate) storage: Arc<S>,
    // ...
}
```

## Consequences

### Positive

-   **Performance:** Static dispatch allows the compiler to fully inline storage access in hot paths, essential for meeting latency targets.
-   **Simplicity:** Removes the need for complex trait bounds and generic type parameters across the entire API surface.
-   **Maintainability:** Easier to read and debug without layers of abstraction indirection.

### Negative

-   **Reduced Flexibility:** Swapping the storage backend requires modifying the `AletheiaDB` struct definition and potentially recompiling, rather than just changing a configuration.
-   **Testing:** We cannot easily mock the entire storage layer for unit tests of `AletheiaDB`. However, individual modules can still be tested in isolation, and integration tests can use the real storage engine (which is designed to be efficient).

### Mitigation

If alternative storage backends (e.g., a purely in-memory transient store for testing, or a distributed backend) are required in the future, we can utilize **compile-time feature flags** (e.g., `#[cfg(feature = "distributed")]`) to swap the concrete types used by `AletheiaDB`, preserving static dispatch benefits while allowing build-time configurability.
