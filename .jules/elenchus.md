# Elenchus Test Quality Audit Journal

## [Tautological Fallback FNV Hashing]
**Module:** `src/core/hasher.rs`
**Severity:** 🔴 Critical
**Finding:** The FNV-1a fallback tests for `IdentityHasher` were tautological. They exactly mirrored the source implementation by reconstructing the FNV-1a multiplication and XOR sequence to generate their `expected` values.
**Evidence:** The original tests explicitly copied the sequence `expected ^= 1; expected = expected.wrapping_mul(FNV_PRIME);` which exactly mirrors the `write` loop. Any mutation altering `FNV_PRIME` or the operation order would survive if the same change was incorrectly made to the test or if it was inherently flawed.
**Recommendation:** Fixed in Sentry's journal earlier, but re-verifying properties.

## [LimitPushdown Weak Assertions]
**Module:** `src/query/planner/rules/limit_pushdown.rs`
**Severity:** 🟡 Suspect
**Finding:** The `LimitPushdown` tests used weak assertions (`assert!(result.is_some())`) which only proved that the rule ran, not that the output was correct.
**Evidence:** `cargo mutants` could easily alter the `top_k` or `limit` logic without failing tests that only check `is_some()`.
**Recommendation:** Refactored tests to explicitly verify the resulting AST bounds.

## [Predicate Pushdown Weak Assertions]
**Module:** `src/query/planner/rules/predicate_pushdown.rs`
**Severity:** 🟡 Suspect
**Finding:** Similar to LimitPushdown, many tests in this module redundantly asserted `is_some()` before checking equality, or only verified the root node type.
**Evidence:** The doc test used `matches!` only on the root node type, allowing internal structure modifications to survive testing.
**Recommendation:** Replaced `assert!(result.is_some())` and `assert_eq!(result.unwrap(), expected_plan)` with a single robust `assert_eq!(result, Some(expected_plan))` check.
