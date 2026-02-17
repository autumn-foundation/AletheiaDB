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
