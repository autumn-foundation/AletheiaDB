**[AletheiaDB]
**Module:** core::temporal::time
**Summary:** `time::to_iso8601` panicked when provided with large positive/negative timestamp values via `std::time::Duration::new` arithmetic against `UNIX_EPOCH`.
**Diagnosis:** Raw cast of potentially negative seconds `secs as u64` resulted in an overflow or underflow crash inside `Duration` parsing or `SystemTime::add`.
**Kill Shot:** Use `.abs()` to get absolute bounds for seconds, and switch to using `checked_add`/`checked_sub` against `UNIX_EPOCH` to safely return stringified durations when values surpass valid `SystemTime` ranges.
