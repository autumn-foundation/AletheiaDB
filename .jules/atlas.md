# Atlas Journal

## [Title]
**Tangle:** The `core` module and the `utils` module had a circular dependency. `core` relied on `utils::error::StorageError` and `utils::error::VectorError`, while `utils::error` relied on types from `core` (e.g., `NodeId`, `VersionId`, `HybridTimestamp`).
**Blueprint:** Extracted a dedicated `CoreError` into `src/core/error.rs` and moved `TemporalError` to `src/core/temporal.rs` so that `core` only depends on errors it defines itself. `utils::error::Error` now wraps `CoreError` using an `Error::Core(CoreError)` variant, decoupling `core` from `utils` while maintaining top-level error integration.
