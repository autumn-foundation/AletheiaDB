# Elenchus Journal

**[TimeRange Validation Gap]**
**Module:** `aletheiadb::core::temporal`
**Severity:** 🟡 Suspect
**Finding:** `TimeRange::new` relied on `Timestamp` (HybridTimestamp) validity, but `From<i64>` allowed constructing invalid timestamps via `new_unchecked`, bypassing `MAX_VALID_TIMESTAMP` checks. This allowed creating invalid `TimeRange`s.
**Evidence:** `tests/warden_temporal_safety.rs` demonstrated that `TimeRange::new` accepted timestamps > `MAX_VALID_TIMESTAMP`.
**Recommendation:** Added validation to `TimeRange::new` to strictly enforce `MAX_VALID_TIMESTAMP` for both start and end times. Added `tests/warden_temporal_safety.rs` as a permanent regression test.
