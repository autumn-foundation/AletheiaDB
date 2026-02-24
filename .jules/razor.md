## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** Fake Honeycomb tracing integration (`observability/backends/honeycomb.rs` and related config).
**Cut:** Deleted `src/observability/backends/honeycomb.rs`, removed `observability-honeycomb` feature, and cleaned up `observability/mod.rs`.
**Saved:** ~200 lines of misleading code that implemented a no-op layer + removed a confusing feature flag.
