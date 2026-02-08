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
