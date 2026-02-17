# Sentinel's Journal

**[HLC Receive Logic Gap]**
**Module:** `src/core/hlc.rs`
**Summary:** The `receive` function's logic for incrementing the logical counter when `new_wallclock == self.wallclock` was susceptible to an off-by-one mutation (`>` vs `>=`) if `physical_wallclock` exactly matched `self.wallclock` while being greater than `msg.wallclock`.
**Diagnosis:** **Missing Coverage**. Existing tests covered cases where wallclock advanced or was strictly determined by `msg` or `self`, but not the specific collision case where physical clock equals local clock (and prevails over message clock).
**Kill Shot:** Added `test_receive_when_physical_equals_local_and_ahead_of_msg` which specifically targets this condition, ensuring logical counter increments instead of resetting.

**[Temporal Range Boundary Gaps]**
**Module:** `src/core/temporal.rs`
**Summary:**
1. `BiTemporalInterval::close_both` was susceptible to argument swapping (passing `valid_end` as `tx_end` and vice versa) because the existing test `test_bitemporal_close` only asserted that the range was closed, not the specific end timestamps.
2. `TimeRange::contains_range` was susceptible to using `<` instead of `<=` for boundary checks because existing tests only covered ranges strictly inside or clearly outside, missing exact boundary matches.
**Diagnosis:** **Weak Assertion** (close_both) and **Missing Coverage** (contains_range boundaries).
**Kill Shot:**
1. Strengthened `test_bitemporal_close` to assert `closed_both.valid_time().end() == valid_end` and `closed_both.transaction_time().end() == tx_end`.
2. Added `test_time_range_contains_range_boundaries` to specifically test cases where inner range shares exactly the same start or end timestamp as the outer range.
