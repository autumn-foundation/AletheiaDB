# Sentinel's Journal

**[HLC Receive Logic Gap]**
**Module:** `src/core/hlc.rs`
**Summary:** The `receive` function's logic for incrementing the logical counter when `new_wallclock == self.wallclock` was susceptible to an off-by-one mutation (`>` vs `>=`) if `physical_wallclock` exactly matched `self.wallclock` while being greater than `msg.wallclock`.
**Diagnosis:** **Missing Coverage**. Existing tests covered cases where wallclock advanced or was strictly determined by `msg` or `self`, but not the specific collision case where physical clock equals local clock (and prevails over message clock).
**Kill Shot:** Added `test_receive_when_physical_equals_local_and_ahead_of_msg` which specifically targets this condition, ensuring logical counter increments instead of resetting.

**[TimeRange Validation Gap]**
**Module:** `src/core/temporal.rs`
**Summary:** `TimeRange::new` checks if timestamps exceed `MAX_VALID_TIMESTAMP`, but no test explicitly verifies that invalid timestamps are rejected.
**Diagnosis:** **Missing Coverage**. Existing tests check `start > end` rejection and `MAX_VALID_TIMESTAMP` acceptance, but not `MAX_VALID_TIMESTAMP + 1` rejection. A mutant disabling this check would survive.
**Kill Shot:** Added `test_time_range_rejects_timestamps_exceeding_max_valid` to assert `TimeRange::new` returns `InvalidTimestamp` error for out-of-bounds values.

**[TimeRange Containment Boundaries]**
**Module:** `src/core/temporal.rs`
**Summary:** `contains_range` checks strict containment, but existing tests don't explicitly cover cases where start or end timestamps match exactly.
**Diagnosis:** **Weak Test**. While likely covered by property tests implicitly, explicit boundary tests ensure off-by-one errors (like `<` vs `<=`) in containment logic are deterministically caught.
**Kill Shot:** Added `test_time_range_contains_range_boundaries` to check `[100, 300)` contains `[100, 200)` and `[200, 300)`.

**[WAL Entry Size Boundary Check]**
**Module:** `src/storage/wal/concurrent.rs`
**Summary:** `serialize_entry` checks `estimated_capacity > MAX_WAL_ENTRY_SIZE`, but no test verified behavior at exactly `MAX_WAL_ENTRY_SIZE` or just above it.
**Diagnosis:** **Missing Coverage**. A mutant changing `>` to `>=` (rejecting valid max-size entries) or removing the check entirely (allowing DoS) would survive.
**Kill Shot:** Added `test_append_entry_exactly_max_size_succeeds` and `test_append_entry_exceeding_max_size_fails` in `sentry_tests` module to enforce strict boundary compliance.

**[PropertyDelta Oversized Vector Ignored]**
**Module:** `src/core/version.rs`
**Summary:** `PropertyDelta::from_diff` silently ignored changes to vectors exceeding `MAX_VECTOR_DIMENSIONS` because `VectorDelta::from_diff` returned `None` (correctly), but the fallback logic assumed `None` meant "no change" without checking sizes or content.
**Diagnosis:** **Suspected Code Bug**. The implementation failed to handle the edge case where `VectorDelta` refuses to process a vector due to size limits, leading to silent data loss/inconsistency.
**Kill Shot:** Added `test_property_delta_handles_oversized_vectors` to verify that oversized vector changes trigger a full replacement. Fixed implementation to fallback to full replacement when `len > MAX` and content differs.

**[VectorDelta Equality Order Sensitivity]**
**Module:** `src/core/version.rs`
**Summary:** `VectorDelta::eq` compares sparse changes position-wise, making it sensitive to index order.
**Diagnosis:** **Weakness/Implementation Detail**. While `from_diff` guarantees sorted order, manual construction does not. Documented this behavior to prevent future regression or assumptions.
**Kill Shot:** Added `test_vector_delta_partial_eq_order_sensitivity` to enforce/document this constraint.
