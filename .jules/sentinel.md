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
