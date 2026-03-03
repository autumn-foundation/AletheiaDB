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

**[Weak Test Coverage in IdentityHasher]**
**Module:** src/core/hasher.rs
**Summary:** The mutant `replace ^= with =` (overwrite instead of mix) in `update_state` survived because existing tests only wrote single values or checked byte-wise writes without asserting mixing behavior for composite keys.
**Diagnosis:** WEAK_TEST - No test verified that multiple `write_u64` calls mix their inputs.
**Kill Shot:** Added `test_identity_hasher_composite_writes` which writes two values and asserts the result is not equal to the last written value (overwrite).

**[Weak Test Coverage in PropertyValue Deserialization]**
**Module:** src/core/property.rs
**Summary:** The mutant `replace != with ==` (strict check) in boolean deserialization survived because existing tests only checked canonical values (0 and 1).
**Diagnosis:** WEAK_TEST - Missing test case for non-canonical true values (e.g. byte 2).
**Kill Shot:** Added `test_deserialize_bool_non_canonical_true` which deserializes byte `2` and asserts it becomes `Bool(true)`.

**[Suspected Bug in IdentityHasher Collision]**
**Module:** src/core/hasher.rs
**Summary:** `IdentityHasher` treats an initial state of `0` as "uninitialized", meaning `write(0)` as the first operation is effectively ignored. This causes a collision where `Hash(0, 42)` equals `Hash(42)`.
**Diagnosis:** SUSPECTED_BUG - The zero-check logic fails to distinguish "default uninitialized state" from "initialized with valid 0".
**Kill Shot:** None (bug reported but not fixed as per Sentinel protocol).

**[Weak Test Coverage in OperationReordering]**
**Module:** src/query/planner/rules/operation_reordering.rs
**Summary:** Mutants modifying constants (`0.1`, `0.2`) in `estimate_filter_selectivity` and structural comparison logic in `predicates_equal` survived because existing tests only verified if some reordering occurred, not the precise values driving the optimization.
**Diagnosis:** WEAK_TEST - Missing exact value checks for heuristics and exhaustive matching in equality tests.
**Kill Shot:** Added `test_exact_selectivity_constants` to enforce specific heuristic values, `test_predicates_equal_structural_integrity` to ensure cross-variant inequality, and `test_cardinality_math` to test the internal arithmetic formulas.
