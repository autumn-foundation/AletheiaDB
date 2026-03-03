# Sentry's Journal 🛡️

## Distributed Transaction Durability Gap
**Learning:** The `ShardCoordinator` was using an in-memory-only commit log (`TwoPhaseCommitLog`), despite a persistent implementation (`PersistentCommitLog`) being available in the codebase. This meant distributed transactions were not durable across coordinator restarts—a violation of ACID properties (Atomicity/Durability). Furthermore, the persistent log implementation was missing the `commit_timestamp` field, which is critical for consistent recovery in a system using Hybrid Logical Clocks.

**Action:**
1.  Always verify that "persistent" components are actually wired up to configuration and initialization paths.
2.  When seeing duplicate implementations (in-memory vs persistent structs), check which one is used in production code vs tests.
3.  Wired up `PersistentCommitLog` to `ShardCoordinator` via new `wal_path` config.
4.  Updated `PersistentCommitLog` schema to include `commit_timestamp` (with backward compatibility logic for V1, though V2 enforced for new writes).

## `unwrap_or_else` Fallbacks on Debug Implementations
**Learning:** `std::fmt::Debug` implementations for entities using interned strings (`Node`, `Edge`) contained `unwrap_or_else(|| format!("{:?}", self.label))` blocks that were completely untested. Because the default behavior relies on `GLOBAL_INTERNER.resolve_with(...)` which almost always succeeds during normal execution, the fallback logic was invisible to the test suite and could silently mask initialization or interner corruption bugs.
**Action:** Always create test cases using artificially injected, non-existent raw IDs (e.g., `InternedString::from_raw(u32::MAX - 10)`) when testing interner resolution fallbacks to ensure formatting and error handling behave correctly.

## Vector Boundary Validation Fallbacks
**Learning:** The public, panicking `PropertyValue::vector` wraps `PropertyValue::try_vector(...).unwrap_or_else(|e| panic!(...))`. While the panicking behavior was generally expected, the fallible `try_vector` component lacked direct coverage ensuring it correctly yielded a generic result over the dimension limits without panicking prematurely.
**Action:** When APIs provide both panicking and fallible construction methods, ensure the fallible method receives explicit `Result::is_err()` boundary testing to prevent regressions that might turn it into a panicking method unintentionally.
