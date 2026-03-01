# Sentinel Journal 🛡️

**[Suspected Bug: Version Snapshot IDs]**
**Module:** `aletheiadb::core::version`
**Summary:** Mutation testing revealed that `VersionData::get_vector_snapshot_id` could be replaced with `None` or hardcoded values without failing tests. Additionally, the match arm extracting the ID from `Anchor` variants could be deleted.
**Diagnosis:** WEAK_TEST - Existing tests did not verify that the vector snapshot ID set on an anchor was correctly retrieved, or that it defaulted correctly for other variants.
**Kill Shot:** Added `test_version_data_get_vector_snapshot_id_mutants` in `tests/sentry_version.rs`.

**[Logic Gap: PropertyDelta::from_diff]**
**Module:** `aletheiadb::core::version`
**Summary:** Several logical branches in `PropertyDelta::from_diff` were not covered:
1. `match guard old_value.semantically_equal(new_value)` could be replaced with `true` or `false`.
2. The optimization path for `VectorDelta` (match arm `(Some(old_vec), Some(new_vec))`) could be removed.
3. The dimension mismatch check (`!=`) could be inverted.
4. The removal check (`!new.contains_interned_key`) could be negated.
**Diagnosis:** MISSING_COVERAGE - Edge cases for semantic equality, vector optimization fallbacks, and removals were not explicitly exercised.
**Kill Shot:** Added specific tests in `tests/sentry_version.rs`: `test_property_delta_semantically_equal_guard`, `test_vector_delta_match_arm_removal`, `test_property_delta_dimension_mismatch_logic`, `test_property_delta_removed_logic`.

**[Trait Default Implementation Weakness]**
**Module:** `aletheiadb::core::version`
**Summary:** Methods in the `EntityVersion` trait implementation for `NodeVersion` and `EdgeVersion` (like `is_anchor`, `version_id`) could be replaced with default return values.
**Diagnosis:** WEAK_ASSERTION - Tests checked these properties implicitly or structurally but didn't strictly enforce that the trait methods returned the *correct* values from the struct fields.
**Kill Shot:** Added `test_entity_version_methods_not_default` to `tests/sentry_version.rs`.
**[Weak Test Coverage in VectorDelta Equality]**
**Module:** src/core/version.rs
**Summary:** The mutant `replace || with &&` in `PartialEq::eq` for `VectorDelta` survived. This means existing tests don't adequately check for inequality when only one component (e.g., index or value) differs in sparse vectors.
**Diagnosis:** WEAK_TEST - Missing negative test cases that explicitly vary only one field (index or value) in sparse vector comparison.
**Kill Shot:** Added `test_vector_delta_partial_eq_semantics` which tests sparse vectors with mismatched indices and values separately.

**[Weak Test Coverage in Transaction Time Closure]**
**Module:** src/core/version.rs
**Summary:** The mutant `replace TemporalVersion::close_transaction_time -> Result<()> with Ok(())` survived. This means tests call this method but don't verify the side effect (transaction time actually closing).
**Diagnosis:** WEAK_TEST - Tests likely check return value is `Ok` but don't inspect the `temporal` object afterwards to confirm the end time was updated.
**Kill Shot:** Added `test_temporal_version_close_transaction_time_updates_tx_dimension` which calls the method and asserts the transaction end time matches the input.

**[Weak Test Coverage in PropertyDelta Empty Check]**
**Module:** src/core/version.rs
**Summary:** Mutants involving `PropertyDelta::is_empty` survived (replacing `&&` with `||`). This implies tests don't cover mixed states where some collections are empty and others are not.
**Diagnosis:** WEAK_TEST - Tests likely check completely empty or completely full deltas, but not "partially empty" ones (e.g., only removed items).
**Kill Shot:** Added `test_property_delta_is_empty_only_when_all_collections_empty` which creates deltas with only one of `changed`, `vector_deltas`, or `removed` populated and asserts `!is_empty()`.

**[Weak Test Coverage in Heap Size Estimation]**
**Module:** src/core/version.rs
**Summary:** Mutants altering arithmetic in `estimated_heap_size` survived. This is expected as heap size estimation is often approximate and not strictly asserted.
**Diagnosis:** WEAK_TEST/ACCEPTABLE - Exact heap size assertions are brittle. However, we can assert lower bounds or relative ordering (sparse < full).
**Kill Shot:** Added `test_vector_delta_sparse_estimated_heap_size_matches_formula` to strictly verify the calculation for known inputs.

**[Weak Test Coverage in EntityVersion Trait Implementation]**
**Module:** src/core/version.rs
**Summary:** Default implementations (returning `None` or `false`) for `EntityVersion` methods like `is_anchor`, `prev_version`, etc., survived. This suggests generic tests for this trait are missing or not exercising concrete implementations.
**Diagnosis:** WEAK_TEST - No test iterates through the version chain using the trait methods on `NodeVersion` / `EdgeVersion`.
**Kill Shot:** Added `test_entity_version_trait_round_trip_links_for_node_and_edge` to verify trait methods correctly proxy to struct fields.

**[Weak Test Coverage in Property Deserialization Boundaries]**
**Module:** src/core/property.rs
**Summary:** Mutants in `deserialize_recursive` modifying buffer length checks (`<` to `<=`) and recursion depth checks (`>` to `>=` or `==`) survived or timed out.
**Diagnosis:** WEAK_TEST - Existing tests covered general DOS scenarios but lacked exact boundary checks (e.g., `len - 1` vs `len`, `MAX_DEPTH` vs `MAX_DEPTH + 1`).
**Kill Shot:** Added `test_deserialize_recursion_exact_boundary` and `test_deserialize_buffer_boundary_conditions` in `src/core/property.rs` (sentry_tests). Manual verification confirmed `test_deserialize_recursion_exact_boundary` kills the `>` -> `==` mutant.

**[Weak Test Coverage in PropertyMap Duplicate Keys]**
**Module:** src/core/property.rs
**Summary:** Potential for mutants to ignore duplicate key errors in `PropertyMap::deserialize`.
**Diagnosis:** WEAK_TEST - No test explicitly constructed a serialized map with duplicate keys to verify rejection.
**Kill Shot:** Added `test_property_map_duplicate_key_rejection` in `src/core/property.rs`.

**[Weak Test Coverage in PropertyValue Equality]**
**Module:** src/core/property.rs
**Summary:** Potential regression in floating point equality semantics (NaN).
**Diagnosis:** WEAK_TEST - Explicit verification of `NaN != NaN` (PartialEq) vs `NaN == NaN` (semantically_equal) was needed to prevent regressions.
**Kill Shot:** Added `test_property_value_partial_eq_nan_semantics` in `src/core/property.rs`.

**[Weak Test Coverage in Predicate Pushdown Boundaries]**
**Module:** `src/query/planner/rules/predicate_pushdown.rs`
**Summary:** The `cargo-mutants` tool indicated that removing the `Traverse` and `Scan` match arms inside `push_down` survived mutation testing. This implies there were no tests ensuring that a filter would correctly stop pushing down at a graph traversal.
**Diagnosis:** MISSING_TEST - The tests did not explicitly verify the negative condition: that `Filter` is properly halted by a `Traverse` operator. The match arms were functionally equivalent to the default case in terms of output, but they carried semantic meaning that was not checked.
**Kill Shot:** Added `test_stop_filter_at_traverse` to ensure a `Filter` is not incorrectly pushed underneath a `Traverse` operation.

**[Weak Test Coverage in Logical Equality Logic]**
**Module:** `src/query/planner/rules/operation_reordering.rs`
**Summary:** Numerous mutants survived in `predicates_equal`, replacing boolean operators `&&` with `||` and equality operators `==` with `!=`.
**Diagnosis:** WEAK_TEST - The existing tests only checked equality matching entirely (where both terms were exactly the same) or entirely differently (different variants). It lacked "partial mismatch" checks (same variant, different internal value).
**Kill Shot:** Added `test_predicates_equal_exhaustive_mismatches` to explicitly check every `Predicate` variant with slightly mismatched keys or values to force the strict `&&` evaluations.
