# Sentry's Journal 🛡️

## Distributed Transaction Durability Gap
**Learning:** The `ShardCoordinator` was using an in-memory-only commit log (`TwoPhaseCommitLog`), despite a persistent implementation (`PersistentCommitLog`) being available in the codebase. This meant distributed transactions were not durable across coordinator restarts—a violation of ACID properties (Atomicity/Durability). Furthermore, the persistent log implementation was missing the `commit_timestamp` field, which is critical for consistent recovery in a system using Hybrid Logical Clocks.

**Action:**
1.  Always verify that "persistent" components are actually wired up to configuration and initialization paths.
2.  When seeing duplicate implementations (in-memory vs persistent structs), check which one is used in production code vs tests.
3.  Wired up `PersistentCommitLog` to `ShardCoordinator` via new `wal_path` config.
4.  Updated `PersistentCommitLog` schema to include `commit_timestamp` (with backward compatibility logic for V1, though V2 enforced for new writes).

**IdentityHasher Coverage Gap**
**Learning:** `IdentityHasher` provides optimizations for pre-hashed unique integer keys. However, large portions of `Hasher::write` branches, state mutability paths, and trait implementations (like bitwise operations in `update_state`) were not comprehensively tested, leaving them vulnerable to subtle regressions if tampered with (e.g., via mutation testing).
**Action:** Wrote exhaustive tests covering every match arm in `write`, explicitly tested the `else` branch of `update_state` (which involves a XOR mix and multiply), and checked each integer specific method (`write_u8`, `write_u16`, etc.) individually and sequentially to eliminate any remaining `cargo mutants` escapees.

## Panic Risks in Query Iterators and Mock Clients
**Learning:** `unwrap()` inside iterator implementations (like `VectorRerankIterator::next`) or trait implementations for mock clients (like `MockVectorNodeClient`) pose a significant availability risk, as a panic can crash the thread handling the query or the entire database process.
**Action:** Always gracefully handle `None` on iterators (returning `None` or propagating errors) and map lock poisoning errors (`PoisonError`) to domain-specific errors (e.g. `VectorError`) to ensure the system degrades gracefully instead of crashing during simulated faults or unexpected states.
