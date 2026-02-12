# Elenchus's Journal ⚔️

**[Zombie Test]**
**Module:** aletheiadb::core::vector::sentry_tests
**Severity:** 🔴 Critical
**Finding:** Test `test_simd_mismatched_lengths_safety` asserts a value (20.0) from a function that panics. The implementation enforces strict length equality, while the test assumes safe truncation. This contradiction means the test provides false confidence about "safety" features that are actually panic guards.
**Evidence:** The test fails with "SIMD vector length mismatch" panic, contradicting its own assertion logic.
**Recommendation:** Modify the test to expect a panic, aligning it with the implementation and the existing `test_unsafe_simd_dot_and_magnitudes_mismatch_panics`. Mark it as a duplicate or merge coverage.

**[Weak Assertion]**
**Module:** aletheiadb::core::property::tests
**Severity:** 🟡 Suspect
**Finding:** Critical recovery methods `IdGenerator::reset_to` and `ensure_at_least` were `pub(crate)` and completely untested. These are the foundation of crash recovery.
**Evidence:** Code audit revealed these methods had no unit tests in `mod tests` or `mod proptests`.
**Recommendation:** Added `mod sentry_tests` with concurrency tests for `ensure_at_least` and verification for `reset_to`.
**Finding:** `test_estimated_heap_size_nested_array` uses a loose lower bound (`>=`) which might allow significant under-estimation to pass.
**Evidence:** `assert!(size >= expected_min);`
**Recommendation:** Calculate the exact expected size (or a very tight range) based on `std::mem::size_of` and `Vec` capacity rules.
