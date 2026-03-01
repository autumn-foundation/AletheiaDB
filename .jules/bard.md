# Bard's Journal 🎻

## 2024-05-23 - Outdated Vector Index Docs
**Confusion:** The `src/index/vector/mod.rs` documentation contained outdated comments stating "no VectorIndex implementation exists yet" and referencing future phases, despite `HnswIndex` being fully implemented. The examples were also marked `no_run`.
**Clarification:** Updated the documentation to reflect that `HnswIndex` is the concrete implementation. Examples were updated to use `ignore` (as they require setup) and the text was updated to describe the current state of the implementation.

## 2024-05-24 - Transaction Consistency Guarantees
**Confusion:** The `ReadOps` trait methods in `src/api/transaction/mod.rs` did not specify consistency guarantees. Specifically, `node_count()` is *not* snapshot-isolated (returns current count), while `get_node()` *is* snapshot-isolated. This distinction is critical but was undocumented.
**Clarification:** Updated `ReadOps` documentation to explicitly state snapshot isolation behavior, ordering guarantees (none), and performance complexity for all methods. Added examples to `WriteOps` to demonstrate correct usage.

## 2024-05-24 - Historical Version Metadata
**Confusion:** Historical versions retrieved from storage (not currently in memory) may have `created_by_tx` set to `TxId(0)`. This is because the creating transaction ID is not currently preserved in the historical storage format to save space.
**Clarification:** Updated `VersionMetadata` documentation to explicitly state this behavior.

## 2024-05-25 - AletheiaDB Default Configuration
**Confusion:** `AletheiaDB::new()` documentation did not specify whether it creates an in-memory or disk-based database. It defaults to disk-based storage at `./aletheiadb/wal`, which could surprise users expecting an ephemeral in-memory instance.
**Clarification:** Updated `AletheiaDB::new()` documentation to explicitly state the default disk-based configuration and point to `with_unified_config` for customization.

## 2024-05-25 - WAL Entry Binary Format
**Confusion:** The on-disk binary format of `WalEntry` was only documented in code comments within the serialization logic, making it hard to understand the storage format without deep diving into implementation details.
**Clarification:** Added detailed binary layout documentation to the `WalEntry` struct in `src/storage/wal/entry.rs`, including field sizes and ordering.

## 2024-05-26 - HTTP JSON Conversion Limits
**Confusion:** The HTTP API's JSON conversion logic enforces a recursion depth limit (100) to prevent stack overflow attacks, but this behavior was undocumented and could surprise developers working with deeply nested data.
**Clarification:** Added documentation to `src/http/converters.rs` explicitly stating the recursion limit and detailing the type mappings between AletheiaDB types and JSON types.

## 2024-05-27 - Hidden Transaction Time Access
**Confusion:** The `VersionInfo` struct does not expose a `tx_time` field, forcing users to dive into the source code to realize they must access `version.temporal.transaction_time().start()`.
**Clarification:** Added a "Temporal Access" section to `VersionInfo` documentation with a clear code example demonstrating how to retrieve both valid time and transaction time.

## 2024-05-27 - Temporal Logic Pitfalls
**Confusion:** Users were unaware of critical boundaries like `MAX_VALID_TIMESTAMP` (causing runtime errors) and the reflexive nature of `TimeRange::contains_range` (causing off-by-one logic bugs).
**Clarification:** Added a "Gotchas & Corner Cases" section to `src/core/temporal.rs` explicitly listing `MAX_VALID_TIMESTAMP` limits, range containment rules, and visibility logic nuances.

## 2024-05-27 - Experimental Feature Opacity
**Confusion:** The `Chronos` (pathfinding) and `Dreamer` (vector extrapolation) experimental modules lacked explanations of their underlying algorithms, making them opaque "black boxes."
**Clarification:** Added detailed algorithmic explanations to `Chronos` (Snapshot Pathfinding, Path Stability) and clarified `Dreamer`'s dependency on `search_vectors_in`.

## 2024-05-28 - Broken Links in Feature-Gated Modules
**Confusion:** Running `cargo doc` without features enabled resulted in broken intra-doc links to experimental modules (like `sherlock` or `hindsight`), causing warnings and confusion about missing items.
**Clarification:** Documentation generation for experimental features requires enabling the `nova` feature flag (e.g., `cargo doc --features nova`).

## 2024-05-29 - Experimental Feature Leaks
**Confusion:** The `experimental` module documentation claimed that all features were gated behind `feature = "nova"`. However, `metaphor` and `muse` were publicly exposed even without the feature enabled, leading to inconsistent API availability and potential runtime panics (as `metaphor` used internal runtime checks instead of compile-time gating).
**Clarification:** Updated `src/experimental/mod.rs` to explicitly gate `metaphor` and `muse` with `#[cfg(feature = "nova")]`, aligning the code with the documentation and ensuring a consistent compile-time experience.

## 2024-05-30 - The Case of the Fake Statistic
**Confusion:** The `Statistics::avg_delta_chain` field (used for cost-based optimization of temporal queries) was documented as "collected statistics" but was actually hardcoded to `5.0` in `refresh_statistics`, with a TODO comment (Issue #366) hidden in the implementation. This could mislead users debugging query performance on deep historical graphs.
**Clarification:** Refactored `AletheiaDB::refresh_statistics` to calculate the actual average chain length from `HistoricalStorage` metadata (`total_versions / total_anchors`). Updated `Statistics` documentation to explain its lifecycle and role in query planning.

## 2026-02-25 - The Case of the Sequential Scatter
**Confusion:** The architecture documentation described a "Scatter-Gather" query executor and "Distributed Transactions", implying high-concurrency parallelism. However, the implementation actually processes shards sequentially (one by one), meaning latency scales linearly ((N)$) rather than remaining constant.
**Clarification:** Updated `src/storage/sharding/executor.rs`, `coordinator.rs`, and `network.rs` to explicitly document the sequential, blocking nature of the current implementation. Added performance warnings to the `README.md` to set correct expectations for Phase 1 sharding.
## 2025-02-28 - Escaping Square Brackets in Docs
**Confusion:** The rustdoc generator attempts to resolve anything inside square brackets `[Like This]` as an intra-doc link, which causes `rustdoc::broken_intra_doc_links` warnings if the text is just meant to be a literal string (e.g., demonstrating a pattern match like `[Person ~ 'Engineer']`).
**Clarification:** You must explicitly escape square brackets that are not meant to be links using backslashes: `\[Like This\]`.

## 2025-02-28 - Redundant Explicit Link Targets
**Confusion:** Writing `[`GLOBAL_INTERNER`](crate::core::GLOBAL_INTERNER)` causes a `rustdoc::redundant_explicit_links` warning because the path resolves to the same destination as the link text itself.
**Clarification:** Rustdoc can automatically resolve the path if it's imported in scope or if it's a known global. Just use `[`GLOBAL_INTERNER`]` directly without the explicit target to keep the documentation source cleaner and avoid warnings.

## 2025-03-01 - README Getting Started Examples
**Confusion:** Users copying examples from the README encountered compilation errors due to missing `aletheiadb::prelude::*` imports and missing feature flags (like `sharding-rpc`). Furthermore, running multiple examples sequentially in the same directory caused runtime crashes (e.g., `InvalidTimeRange`) due to conflicting leftover state in the default `./aletheiadb` directory. Unused variables also caused compiler warnings.
**Clarification:** Updated README examples to consistently include `use aletheiadb::prelude::*`, explicitly declare required feature flags for sharding, prefix unused variables with `_`, and added a prominent warning about database state persistence and cleanup between example runs.
