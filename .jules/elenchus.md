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

**[Silent Vector Delta Failure]**
**Module:** `src/core/version.rs`
**Severity:** 🟡 Suspect
**Finding:** `PropertyDelta::apply` silently ignores `VectorDelta::Sparse` updates if the base property is missing or has the wrong type. This "fail open" behavior preserves the original state but leads to silent data loss regarding the intended update.
**Evidence:** `test_property_delta_apply_sparse_ignored_on_missing_base` confirmed that applying a sparse delta to a map missing the key results in no change and no error.
**Recommendation:** Added 2 permanent regression tests in `src/core/version.rs` to document this behavior. Future refactoring should consider returning `Result` from `apply` to enable "fail closed" behavior.

**[HNSW Deadlock Prevention]**
**Module:** `src/index/vector/hnsw.rs`
**Severity:** ⭐ Commended
**Finding:** The module explicitly handles complex concurrency hazards, including:
1.  **FFI Safety:** Strict alignment and null checks for callback pointers.
2.  **Deadlock Prevention:** `IN_FILTER_CALLBACK` thread-local guard prevents re-entrant modifications during searches, blocking a known RwLock deadlock vector.
3.  **Lock Ordering:** Consistent `DashMap` -> `RwLock` (or sequential) ordering logic prevents lock inversion deadlocks.
**Evidence:** Code analysis of `save_internal`, `add`, and `search_with_filter` confirms correct lock discipline and re-entrancy guards.
**Recommendation:** None. The implementation serves as a model for other concurrent modules.

**[DotProduct Metric Conversion Bug]**
**Module:** `src/index/vector/hnsw.rs`
**Severity:** 🔴 Critical
**Finding:** The `DotProduct` similarity conversion was incorrect. `usearch` returns `1 - dot_product` for the IP metric, but the wrapper was converting it as `-distance`. This resulted in similarity scores being off by 1.0 (e.g., actual dot product 11 returned as 10).
**Evidence:** The strengthened `test_distance_to_similarity_conversion` failed for `DotProduct` with the message "DotProduct n2 should be 11.0, got 10".
**Resolution:** Updated the conversion logic for `DotProduct` to be `1.0 - distance` instead of `-distance`.

**[Critical: Silent Vector Update Loss]**
**Module:** `src/core/version.rs`
**Severity:** 🔴 Critical
**Finding:** `PropertyDelta::from_diff` silently ignored vector updates if `VectorDelta::from_diff` returned `None` (e.g., due to dimension mismatch). This resulted in data loss where the new vector value was discarded and the old value preserved.
**Evidence:** Created reproduction test `test_property_delta_silently_ignores_dimension_change` which confirmed that changing a vector's dimension resulted in no change being recorded in the delta.
**Resolution:** Modified `PropertyDelta::from_diff` to strictly fall back to a full value replacement in `delta.changed` when `VectorDelta` cannot be computed (e.g. dimension mismatch), while still respecting epsilon-equality for identical vectors. Added regression test to `sentry_tests`.

**[HNSW Compilation Repair]**
**Module:** `src/index/vector/hnsw.rs`
**Severity:** 🔴 Critical
**Finding:** The file contained a syntax error (unclosed delimiter) in `FilterCallbackGuard` implementation, preventing compilation.
**Evidence:** `cargo test` failed with "this file contains an unclosed delimiter".
**Resolution:** Repaired the syntax error by closing the `new` function and `impl` block, and implementing `Drop` correctly.
