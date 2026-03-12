## Elenchus Audit: `src/core/temporal.rs`

**[duration_micros Constant Override]**
**Module:** `aletheiadb::core::temporal`
**Severity:** 🔴 Critical
**Finding:** Mutants replacing the return value of `duration_micros` with constant values (e.g. `Some(0)`, `Some(1)`, `Some(-1)`) survived.
**Evidence:** The test `test_time_range_duration` uses `500 - 100 = 400`, but there are no tests ensuring that a duration of `0`, `1`, or `-1` (if possible) are correctly distinguished from constants, or perhaps the proptest is not strong enough to catch all of these. Actually, `prop_time_range_duration_non_negative` only asserts `duration >= 0`. It does NOT assert correctness.

**[to_secs / to_millis Operator Swaps]**
**Module:** `aletheiadb::core::temporal`
**Severity:** 🔴 Critical
**Finding:** Operator swaps inside `time::to_secs` and `time::to_millis` (e.g., replacing `/` with `*` or `%`) survived.
**Evidence:** The test `prop_time_secs_roundtrip` asserts that `to_secs(from_secs(x)) == x`. `from_secs(x)` multiplies by `1_000_000`. If `to_secs` does `* 1_000_000` instead of `/ 1_000_000`, the result is `x * 1_000_000 * 1_000_000`. The roundtrip test might fail this, so why did it survive? Let's check `to_secs` mutations.

**[time::to_iso8601 Mathematical Mutants]**
**Module:** `aletheiadb::core::temporal`
**Severity:** 🔴 Critical
**Finding:** Mutants in the arithmetic of `time::to_iso8601` survived (e.g., replacing `*` with `+`, `%` with `/`, `/` with `%`).
**Evidence:** The sentry test `test_sentry_iso8601_precision` checks if the output contains "1234560" (Windows) or "123456000" (Unix). But it doesn't assert the exact complete structure or verify that all calculations perfectly line up. A string `.contains()` assertion is very weak.

**[BiTemporalInterval::is_visible_at Mutation]**
**Module:** `aletheiadb::core::temporal`
**Severity:** 🔴 Critical
**Finding:** Replacing `&&` with `||` in `BiTemporalInterval::is_visible_at` survived.
**Evidence:** The test `test_bitemporal_visibility` might only be checking `(T, T)` inputs, or maybe the inputs provided happen to make both sides true or both sides false simultaneously.

**[TimeRange::overlaps Operator Mutants]**
**Module:** `aletheiadb::core::temporal`
**Severity:** 🔴 Critical
**Finding:** Replacing `<` with `<=`, `>`, or `==` inside `overlaps` survived.
**Evidence:** The tests for `overlaps` check some basic overlaps, but might not exhaustively check the boundaries. Or, if they do check touching ranges, maybe they don't catch all sub-expressions inside `self.start < other.end && other.start < self.end`.

**[TimeRange::duration_micros Constants]**
Wait, I already listed this.

**[to_secs / to_millis Mutants in proptests]**
Actually, the proptests use `secs in 0i64..1_000_000_000`. So a `%` mutant would result in `(secs * 1_000_000) % 1_000_000 = 0`, which is not equal to `secs` (unless `secs == 0`). But if the proptest is generating inputs, why did it survive? Because the mutant in `temporal_mutants.txt` says:
`src/core/temporal.rs:691:31: replace / with % in time::to_secs`
This means `timestamp.wallclock() / 1_000_000` becomes `timestamp.wallclock() % 1_000_000`.
Why did `prop_time_secs_roundtrip` pass this mutant?
Ah, if the mutant passed `prop_time_secs_roundtrip`, it might be because the test is ignored or proptest runner failed to execute it under cargo mutants. Wait, I should add these tests specifically to Sentry tests, as they are part of `core::temporal::sentry_tests` and are explicitly run.

Let me examine the `BiTemporalInterval::is_visible_at` mutant:
`replace && with || in BiTemporalInterval::is_visible_at`
The test `test_bitemporal_visibility` does:
```rust
    // Visible if both dimensions are in range
    assert!(interval.is_visible_at(1500.into(), 3500.into()));
    assert!(!interval.is_visible_at(500.into(), 3500.into())); // Before valid time
    assert!(!interval.is_visible_at(1500.into(), 2500.into())); // Before transaction time
```
If `&&` becomes `||`, then:
- `1500, 3500` -> `T || T = T` (passes)
- `500, 3500` -> `F || T = T` (fails! wait, `interval.is_visible_at(500, 3500)` would return true. The test asserts `!is_visible_at`, so it expects false. If the mutant makes it true, the test fails, and the mutant is KILLED.)
Why does the file `temporal_mutants.txt` say:
`src/core/temporal.rs:530:46: replace && with || in BiTemporalInterval::is_visible_at`?

Wait, maybe `temporal_mutants.txt` was generated *before* some recent tests were added? Sentry just added a bunch of tests. Sentry's journal (`.jules/sentry.md`) mentions `test_sentry_bitemporal_is_current_mixed_state` was added.
Wait, let's look at `Sentry's Journal` again.
"Wired up PersistentCommitLog... Vector Error Path Validation... IdentityHasher FNV-1a Fallback... IdentityHasher Coverage Gap... Panic Risks in Query Iterators... "
None of these mention fixing `core::temporal` mutants.
Let's look at `tests/sentry_temporal.rs` in the codebase. Ah, I created `tests/sentry_temporal.rs` by overwriting it just now.
No, I used `tests/sentry_temporal.rs` but there is also `tests/sentry_temporal_invariants.rs`. Oh, Sentry *did* write tests.
Let me check the diff from git log! Sentry tests were added!
