# Elenchus Journal

**[TimeRange Validation Gap]**
**Module:** `aletheiadb::core::temporal`
**Severity:** 🟡 Suspect
**Finding:** `TimeRange::new` relied on `Timestamp` (HybridTimestamp) validity, but `From<i64>` allowed constructing invalid timestamps via `new_unchecked`, bypassing `MAX_VALID_TIMESTAMP` checks. This allowed creating invalid `TimeRange`s.
**Evidence:** `tests/warden_temporal_safety.rs` demonstrated that `TimeRange::new` accepted timestamps > `MAX_VALID_TIMESTAMP`.
**Recommendation:** Added validation to `TimeRange::new` to strictly enforce `MAX_VALID_TIMESTAMP` for both start and end times. Added `tests/warden_temporal_safety.rs` as a permanent regression test.

**[PropertyMap Safety Gaps]**
**Module:** `aletheiadb::core::property`
**Severity:** 🟡 Suspect
**Finding:** `PropertyMap` lacked explicit tests for capacity limits (`MAX_PROPERTY_MAP_CAPACITY`), correctness of removal operations (builder pattern), and DoS protection against pre-allocation attacks with insufficient buffer size.
**Evidence:** Audit revealed `MAX_PROPERTY_MAP_CAPACITY` was checked in code but never exercised in tests. `PropertyMapBuilder::remove` was only tested for size consistency, not actual removal.
**Recommendation:** Added 4 safety tests in `src/core/property.rs` (`mod sentry_tests`) covering capacity enforcement, removal correctness, trailing bytes handling, and pre-allocation DoS protection.

**[HLC Causality Blind Spot]**
**Module:** `aletheiadb::core::hlc`
**Severity:** 🔴 Critical
**Finding:** The property test `prop_receive_causality` provided false confidence. It generated random timestamps from a large `i64` space, making the probability of generating colliding wallclocks (where the core complexity of HLC lies) effectively zero. The logic for resolving `local.wallclock == msg.wallclock == physical` was untested by the property suite.
**Evidence:** Mutation testing (Mutation 2: ignoring `msg.logical` in collision case) passed the original test suite but failed the new `prop_receive_causality_collision` test.
**Recommendation:** Added `prop_receive_causality_collision` to `src/core/hlc.rs` to specifically target the collision scenario.

**[HLC Assertion Weakness]**
**Module:** `tests/hlc_tests.rs`
**Severity:** 🟡 Suspect
**Finding:** Integration tests relied on weak assertions like `assert!(result.is_err())` and string matching for error messages. This made tests brittle and capable of passing for the wrong reasons (e.g., panics or different errors).
**Evidence:** `test_deserialize_truncated_buffer` only checked `is_err()`. `test_send_logical_overflow` checked `error_msg.contains`.
**Recommendation:** Refactored tests to use `matches!(result, Err(StorageError::CorruptedData(_)))` and `Err(TemporalError::LogicalCounterOverflow { .. })` for robust, type-safe verification. Removed redundant "mirror tests" that duplicated unit test coverage.
