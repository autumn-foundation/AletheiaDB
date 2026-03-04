# Sentry's Journal 🛡️

## Distributed Transaction Durability Gap
**Learning:** The `ShardCoordinator` was using an in-memory-only commit log (`TwoPhaseCommitLog`), despite a persistent implementation (`PersistentCommitLog`) being available in the codebase. This meant distributed transactions were not durable across coordinator restarts—a violation of ACID properties (Atomicity/Durability). Furthermore, the persistent log implementation was missing the `commit_timestamp` field, which is critical for consistent recovery in a system using Hybrid Logical Clocks.

**Action:**
1.  Always verify that "persistent" components are actually wired up to configuration and initialization paths.
2.  When seeing duplicate implementations (in-memory vs persistent structs), check which one is used in production code vs tests.
3.  Wired up `PersistentCommitLog` to `ShardCoordinator` via new `wal_path` config.
4.  Updated `PersistentCommitLog` schema to include `commit_timestamp` (with backward compatibility logic for V1, though V2 enforced for new writes).

## IdentityHasher FNV-1a Fallback Coverage Gap
**Learning:** `IdentityHasher` implements a highly optimized pass-through hash for known primitive integers (length 1, 2, 4, 8, 16), but has a catch-all fallback (`_ =>`) using the FNV-1a algorithm for any other length byte slices. This fallback logic lacked property-based verification to ensure it accurately implements the FNV-1a algorithm for arbitrary slices and correctly chains state when prior writes have occurred. Testing this fallback logic revealed an edge case in empty slice fallback.
**Action:** Always verify "catch-all" match arms using property testing to cover all unexpected shapes and lengths, ensuring manual implementations match reference implementations.
**IdentityHasher Coverage Gap**
**Learning:** `IdentityHasher` provides optimizations for pre-hashed unique integer keys. However, large portions of `Hasher::write` branches, state mutability paths, and trait implementations (like bitwise operations in `update_state`) were not comprehensively tested, leaving them vulnerable to subtle regressions if tampered with (e.g., via mutation testing).
**Action:** Wrote exhaustive tests covering every match arm in `write`, explicitly tested the `else` branch of `update_state` (which involves a XOR mix and multiply), and checked each integer specific method (`write_u8`, `write_u16`, etc.) individually and sequentially to eliminate any remaining `cargo mutants` escapees.

## Import CSR Invariant Unwrapping
**Learning:** `AdjacencyIndex::import_csr` used `.unwrap()` directly on the result of `validate_csr_invariants`. If the persisted graph data was corrupted on disk (e.g., offsets were wrong or lengths mismatched), the database startup would panic and crash the entire process.
**Action:** Changed the function to return a `Result<Self, String>` and use the `?` operator. Propagated this error up through `CurrentIndexes` and `CurrentStorage` so that the `IndexPersistenceManager` could safely catch it during startup. Now, if corrupted data is loaded, it gracefully logs an error and falls back to `compact_adjacency()` to rebuild the index. Verified by converting the existing `#[should_panic]` test to expect an `Err`.
