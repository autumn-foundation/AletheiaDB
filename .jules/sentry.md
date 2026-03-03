# Sentry's Journal 🛡️

## Distributed Transaction Durability Gap
**Learning:** The `ShardCoordinator` was using an in-memory-only commit log (`TwoPhaseCommitLog`), despite a persistent implementation (`PersistentCommitLog`) being available in the codebase. This meant distributed transactions were not durable across coordinator restarts—a violation of ACID properties (Atomicity/Durability). Furthermore, the persistent log implementation was missing the `commit_timestamp` field, which is critical for consistent recovery in a system using Hybrid Logical Clocks.

**Action:**
1.  Always verify that "persistent" components are actually wired up to configuration and initialization paths.
2.  When seeing duplicate implementations (in-memory vs persistent structs), check which one is used in production code vs tests.
3.  Wired up `PersistentCommitLog` to `ShardCoordinator` via new `wal_path` config.
4.  Updated `PersistentCommitLog` schema to include `commit_timestamp` (with backward compatibility logic for V1, though V2 enforced for new writes).

**[Title] PropertyMapBuilder and SparseVec Panic Protection**
**Learning:** `PropertyMapBuilder::remove` could hit a panic due to `try_remove_by_key` returning an error on deeply nested values during serialization size calculation. `SparseVec::new` could potentially cause panics if not adequately validating out-of-bounds, duplicates, or non-finite values before internal operations.
**Action:** Always test internal limits (like `MAX_RECURSION_DEPTH`) in mutation methods (`insert`, `remove`), not just at construction or deserialization. Add comprehensive validation for array indices to prevent panics in specialized structures like `SparseVec`.
