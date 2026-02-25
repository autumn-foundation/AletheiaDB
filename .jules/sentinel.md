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

**[Weak Test Coverage in Semantic Pathfinding]**
**Module:** src/query/semantic_pathfinding.rs
**Summary:** Mutants `replace - with /` in cost calculation and constant-cost mutants (`Ok(1.0)`) survived.
**Diagnosis:** WEAK_TEST - Existing tests were ambiguous (cost ties handled arbitrarily) or did not cover cases where inverted similarity logic (`-` vs `/`) would yield plausible but incorrect results.
**Kill Shot:** Added `tests/sentinel_semantic_pathfinding.rs` with `test_semantic_pathfinding_penalizes_opposite` (kills `/`) and `test_semantic_pathfinding_overcomes_structural_cost` (kills constant cost by forcing a longer but cheaper path).
