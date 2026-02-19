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

**[TimeRange Infinity Boundaries]**
**Module:** `src/core/temporal.rs`
**Summary:** `TimeRange::new` has special handling to allow `TIMESTAMP_MAX` (infinity) as a start or end timestamp, bypassing `MAX_VALID_TIMESTAMP` checks. Removing these exemptions (e.g., `&& start != TIMESTAMP_MAX`) would cause valid infinite ranges to be rejected.
**Diagnosis:** **Missing Coverage**. No existing test explicitly verified that `TimeRange::new` accepts `TIMESTAMP_MAX` as a start or end timestamp. A mutant removing the exemption survived the existing suite.
**Kill Shot:** Added `test_sentry_timerange_new_allows_infinity_start` and `test_sentry_timerange_new_allows_infinity_end` in `sentry_tests` to enforce support for infinite ranges constructed via `new`.
