**[Weak Test Coverage in DeduplicationPolicy]**
**Module:** src/index/temporal.rs
**Summary:** The mutant `delete !` in `EntityTimeline::insert_batch` (DeduplicationPolicy::Reject) survived because existing tests only checked that `Reject` works for duplicates (failure case), but never verified it works for valid data (success case).
**Diagnosis:** WEAK_TEST - Missing positive test case for `DeduplicationPolicy::Reject`.
**Kill Shot:** Added `test_batch_insert_reject_policy_valid_batch` which asserts that `insert_batch` with `Reject` policy succeeds for a batch with unique items. Verified manually that this test fails if the mutant is applied.

**[Suspected Bug in Vector Threshold]**
**Module:** src/core/vector/constants.rs / src/core/vector/ops.rs
**Summary:** `SQUARED_MAGNITUDE_THRESHOLD` was set to `1e-14`, causing valid small vectors (magnitude < 1e-7) to be treated as zero vectors, failing normalization and similarity checks.
**Diagnosis:** SUSPECTED_BUG - The threshold was too aggressive, preventing operations on valid small-scale vectors (e.g., gradients).
**Kill Shot:** Changed `SQUARED_MAGNITUDE_THRESHOLD` to `1e-25`. Updated `test_normalize_in_place_tiny_vector` to assert correct normalization instead of zeroing out. Added regression tests `test_normalize_small_vector_preserves_direction`, `test_cosine_similarity_small_vectors`, `test_cosine_similarity_mixed_magnitude`.
