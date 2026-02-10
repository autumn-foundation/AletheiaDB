# Elenchus Journal ⚔️

## Verdicts & Patterns

This journal records the results of test quality audits.

### Verdict: Weak Test Assertions in HNSW Module
**Module:** `src/index/vector/hnsw.rs`
**Severity:** 🟡 Suspect
**Finding:** Tests in `hnsw.rs` often use weak assertions or partial checks, leaving gaps in coverage for critical logic like metric conversion and search result accuracy.
**Evidence:**
- `test_distance_to_similarity_conversion` only tests `DistanceMetric::Cosine`. It ignores `Euclidean`, `DotProduct`, `Haversine`, `Hamming`, and `Tanimoto`.
- `test_hnsw_basic` asserts only the ID of the first result, ignoring the score and subsequent results.
- `test_hnsw_search_with_filter` similarly checks only the ID.
**Recommendation:**
- Refactor `test_distance_to_similarity_conversion` to be a parameterized test (or iterate through all metrics) and verify exact expected scores.
- Strengthen `test_hnsw_basic` to assert on the full result set (IDs and scores) with tolerance.

### Verdict: Bug Found via Test Strengthening
**Module:** `src/index/vector/hnsw.rs`
**Severity:** 🔴 Critical
**Finding:** The `DotProduct` similarity conversion was incorrect. `usearch` returns `1 - dot_product` for the IP metric, but the wrapper was converting it as `-distance`. This resulted in similarity scores being off by 1.0 (e.g., actual dot product 11 returned as 10).
**Evidence:** The strengthened `test_distance_to_similarity_conversion` failed for `DotProduct` with the message "DotProduct n2 should be 11.0, got 10".
**Resolution:** Updated the conversion logic for `DotProduct` to be `1.0 - distance` instead of `-distance`.
