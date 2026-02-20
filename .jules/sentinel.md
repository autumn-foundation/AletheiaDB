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

**[Time ISO 8601 Precision Loss]**
**Module:** `src/core/temporal.rs`
**Summary:** `time::to_iso8601` calculation of nanoseconds (`(wallclock % 1_000_000) * 1000`) was susceptible to arithmetic mutations (e.g. `*` to `/`).
**Diagnosis:** **Weak Test**. Existing tests only asserted that the seconds component was present in the output string, allowing incorrect fractional parts to pass unnoticed.
**Kill Shot:** Added `test_sentry_iso8601_precision` which inputs a timestamp with microseconds and explicitly asserts that the calculated nanoseconds appear in the output.

**[MAX_VALID_TIMESTAMP Value Integrity]**
**Module:** `src/core/temporal.rs`
**Summary:** `MAX_VALID_TIMESTAMP` constant definition was susceptible to reduction (e.g. `replace - with /`) because existing boundary tests used the constant itself for assertions (tautological tests).
**Diagnosis:** **Weak Test**. Tests checked `TimeRange::new(MAX_VALID_TIMESTAMP, ...)` which would pass even if `MAX_VALID_TIMESTAMP` was small, as long as it was self-consistent.
**Kill Shot:** Added `test_sentry_max_valid_timestamp_value` which asserts that `TimeRange::new` accepts a hardcoded large timestamp (`i64::MAX - 2000`), ensuring the limit isn't drastically reduced.

**[Weakness in Edge::connects Verification]**
**Module:** `src/core/graph.rs`
**Summary:** The `Edge::connects(source, target)` method was vulnerable to a mutation where the `source` check could be ignored (e.g., `self.target == target` instead of `self.source == source && self.target == target`). Existing tests only checked:
1. `(Correct, Correct) -> True`
2. `(Wrong, Wrong) -> False`
3. `(Correct, Wrong) -> False`
They missed the case `(Wrong, Correct) -> False`.
**Diagnosis:** **Weak Test**. The test suite lacked a specific assertion for source mismatch when the target matches.
**Kill Shot:** Added `test_sentry_edge_connects_source_check` which explicitly verifies `!edge.connects(wrong_source, correct_target)`.
