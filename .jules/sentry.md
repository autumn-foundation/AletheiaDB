# Sentry's Journal 🛡️

## Distributed Transaction Durability Gap
**Learning:** The `ShardCoordinator` was using an in-memory-only commit log (`TwoPhaseCommitLog`), despite a persistent implementation (`PersistentCommitLog`) being available in the codebase. This meant distributed transactions were not durable across coordinator restarts—a violation of ACID properties (Atomicity/Durability). Furthermore, the persistent log implementation was missing the `commit_timestamp` field, which is critical for consistent recovery in a system using Hybrid Logical Clocks.

**Action:**
1.  Always verify that "persistent" components are actually wired up to configuration and initialization paths.
2.  When seeing duplicate implementations (in-memory vs persistent structs), check which one is used in production code vs tests.
3.  Wired up `PersistentCommitLog` to `ShardCoordinator` via new `wal_path` config.
4.  Updated `PersistentCommitLog` schema to include `commit_timestamp` (with backward compatibility logic for V1, though V2 enforced for new writes).

## Race Condition in Lock-Free Snapshotting
**Learning:** `StringInterner::get_all_strings` (a snapshot operation) assumed that `next_id` increment and map insertion were atomic. However, in a lock-free structure using `DashMap` and `AtomicU32`, there is a window where `next_id` is incremented but the item is not yet visible in the map. This caused `get_all_strings` to return initialized empty strings ("holes") for valid IDs.

**Action:** When snapshotting concurrent data structures, always account for "pending" items. Use `Option<T>` to distinguish "not present" from "default value", and implement a retry/backoff mechanism to wait for the item to become visible (eventual consistency) before returning the snapshot.
