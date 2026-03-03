# Sentry's Journal 🛡️

## Distributed Transaction Durability Gap
**Learning:** The `ShardCoordinator` was using an in-memory-only commit log (`TwoPhaseCommitLog`), despite a persistent implementation (`PersistentCommitLog`) being available in the codebase. This meant distributed transactions were not durable across coordinator restarts—a violation of ACID properties (Atomicity/Durability). Furthermore, the persistent log implementation was missing the `commit_timestamp` field, which is critical for consistent recovery in a system using Hybrid Logical Clocks.

**Action:**
1.  Always verify that "persistent" components are actually wired up to configuration and initialization paths.
2.  When seeing duplicate implementations (in-memory vs persistent structs), check which one is used in production code vs tests.
3.  Wired up `PersistentCommitLog` to `ShardCoordinator` via new `wal_path` config.
4.  Updated `PersistentCommitLog` schema to include `commit_timestamp` (with backward compatibility logic for V1, though V2 enforced for new writes).

## Testing Serialization Panic Paths
**Learning:** Defensive checks within low-level serialization/deserialization routines (e.g. `PropertyValue` and `PropertyMap` in `core/property.rs`) often hide untested panic paths where invalid payloads trigger out-of-bounds reads. When loops or deep array parsing are involved, ensuring boundaries are checked *before* capacity allocations avoids hidden DoS vectors.
**Action:** When auditing `from_bytes` or `deserialize` code blocks, specifically write tests targeting exact buffer length deficiencies (e.g. providing $N-1$ bytes), invalid UTF-8 boundaries, and max-capacity constraint violations to ensure the parser returns an `Err` instead of crashing.
