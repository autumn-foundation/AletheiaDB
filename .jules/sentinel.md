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

**[Weak Test Coverage in ID Format and Boundaries]**
**Module:** `src/core/id.rs`
**Summary:** Mutants modifying formatting (`Display` output logic) for `NodeId`, `EdgeId`, `VersionId`, `TxId`, and `EntityId`, as well as constants boundary logic in `IdGenerator` (like `with_start`, `current_approximate`) and conversions (`is_node`, `is_edge`, `as_node`, `as_edge`) survived.
**Diagnosis:** WEAK_TEST - The tests likely checked properties but lacked explicit assertions covering expected display outputs, boundary edge-cases for initial offset ID generation, and exact positive/negative checks for polymorphic entity conversions.
**Kill Shot:** Added `tests/sentinel_id_tests.rs` containing targeted kill-shots for each missed property.
**[Weak Test Coverage in HTTP API Handlers]**
**Module:** `src/http/handlers.rs`
**Summary:** Mutants survived regarding JSON payload type conversions (`json_to_predicate_value`), boundary condition checks for DoS protection (`> 10000` mutated to `>=` or `==`), and error code categorization logic (`||` mutated to `&&`).
**Diagnosis:** MISSING_TEST / WEAK_TEST - The test suite didn't comprehensively cover the individual match arms of `json_to_predicate_value`, the edge cases of deep pagination limits (`offset + limit == 10000`), or explicit distinction between "syntax" and "parse" error handling branches. Also, an underlying `test_cors_headers_present` test failure masked overall mutant evaluation for `http_server` tasks.
**Kill Shot:** Fixed the CORS test by supplying a `peer_addr`, and added `test_json_to_predicate_value`, `test_execute_query_parse_error`, and exact boundary condition payloads (9900 + 100 vs 9901 + 100) inside `test_warden_find_node_deep_pagination` and `test_warden_find_neighbors_overflow`.

**[Weak Test Coverage in Temporal Types Duration & Methods]**
**Module:** src/core/temporal.rs
**Summary:** Mutants returning arbitrary Option values for `TimeRange::duration_micros` survived. The test suite also didn't explicitly assert exact return values for methods like `BiTemporalInterval::is_currently_valid` / `is_current` across all variants of open/closed intervals.
**Diagnosis:** WEAK_TEST - The tests likely asserted presence (`is_some`) or checked properties implicitly rather than verifying the exact boundary output.
**Kill Shot:** Added `test_timerange_duration_micros_exact`, `test_bitemporal_methods_exact`, and related exact boolean assertions to `tests/sentry_temporal.rs` catching specific returns like `Some(0)` or `None`.
**[Weak Test Coverage in ID Generator Boundaries & EntityId Casting]**
**Module:** `src/core/id.rs`
**Summary:** Mutants returning default values or mutating exact operations survived in `IdGenerator` logic (`ensure_at_least` > changed to <, ==, etc., `current_approximate` returned default constant), `EntityId` casting (`as_node`, `as_edge` returned defaults instead of `Option` logic), and `TxIdGenerator::next` (boundary tests against `== u64::MAX` not strictly forced to pass/fail cleanly on simple operators + / -, and `TxId` default returns allowed).
**Diagnosis:** MISSING_TEST / WEAK_TEST - The previous Sentinel run added `tests/sentinel_id_tests.rs` covering basic display and structural assertions but lacked targeted logic checks (killing operator modifications `+` to `-` or `*`) inside `TxIdGenerator` and `IdGenerator` (which are also internal/`pub(crate)` scoped). `EntityId::as_node` defaults slipped past tests that only invoked `.is_node()` rather than directly asserting exact `.as_node()` value match against zero constants.
**Kill Shot:** Added exhaustive logic tests `sentinel_id_generator_tests` covering exactly `ensure_at_least`, `current_approximate`, and `TxIdGenerator::next` within `src/core/id.rs` directly, alongside appending exact exhaustive checks to `tests/sentinel_id_tests.rs` to stop `as_node`/`as_edge` default `None` and `Some(Default::default())` mutants.

**[Weak Test Coverage in ID Generation & Defaults]**
**Module:** `aletheiadb::core::id`
**Summary:** Mutation testing revealed numerous surviving mutants related to returning defaults (`Default::default()`, `0`, `1`) from constructors (`new_unchecked`, `with_start`), accessors (`as_u64`, `current`, `current_approximate`), and `fmt::Display`. Additional surviving mutants manipulated boundaries (`>`, `>=`, `<`), constant math (`+`, `/` for `MAX_VALID_ID`), and generator state transitions (`reset_to` return type empty).
**Diagnosis:** WEAK_TEST / MISSING_TEST - Tests often asserted positive paths (e.g. `assert_eq!(id, 42)`) but lacked explicit negative bounds preventing implementations from collapsing to zero, 1, or empty structures that happen to not trigger failures down stream. Tests were also missing strict arithmetic boundary testing for constants and `TxIdGenerator` sequencing.
**Kill Shot:** Extensively added direct bound assertions (`assert_ne!(id, 0)`), explicit exhaustiveness to `sentinel_id_tests.rs`, and introduced a `sentinel_id_generator_tests` module in `src/core/id.rs` directly to access `pub(crate)` APIs and kill underlying generator logic mutations.

**[Weak Test Coverage in HLC Time Bounds]**
**Module:** `aletheiadb::core::hlc`
**Summary:** Mutation testing indicated several surviving mutants around exact evaluation of `is_clock_skew_self_heal_enabled` reading configurations, default value handling in formatters (e.g. `ClockSkewDirection::as_str` and `Display` for `HybridTimestamp`), arithmetic operators inside `as_secs` and `as_millis` (which survived replacement to `*` and `%`), and strict evaluations within `HybridTimestamp::receive` boundary `&&` / `||` checks.
**Diagnosis:** WEAK_TEST / MISSING_TEST - Tests often asserted overall flow or simple logical bumps but lacked explicit assertions regarding specific fallback arithmetic and exact boundary conditions when physical, local, and message times perfectly collided in unexpected combinations.
**Kill Shot:** Appended targeted boundary tests `test_is_clock_skew_self_heal_enabled_override`, `test_clock_skew_direction_as_str`, `test_hybrid_timestamp_display`, `test_hybrid_timestamp_as_secs_millis_exact`, and `test_hybrid_timestamp_receive_exact_wallclock_logic` within `src/core/hlc.rs` to close these gaps.
**[Weak Test Coverage in IdentityHasher Boundaries & Logic]**
**Module:** `src/core/hasher.rs`
**Summary:** Mutants returning default values or mutating exact bitwise operations survived in `IdentityHasher` logic (`^=` changed to `|=` or `&=`, `update_state` removed, match arms deleted for various byte lengths inside `write`).
**Diagnosis:** WEAK_TEST / MISSING_TEST - Tests often asserted high-level collision avoidance (e.g. `assert_ne!(h1, h2)`) but lacked explicit exact bounds for `update_state` logic with `FNV_PRIME`, lengths of `write()`, default return overrides, and proper bitwise math chaining.
**Kill Shot:** Extensively added exact bound and behavioral assertions across bitwise operators, lengths, and chaining logic within the `tests` module inside `src/core/hasher.rs`.

**[Weak Test Coverage in Temporal Logic Boundaries and Math]**
**Module:** `src/core/temporal.rs`
**Summary:** Mutants regarding strict bounds on `MAX_VALID_TIMESTAMP` (`>` mutated to `>=` or `==`) within `TimeRange::from` and `TimeRange::at` survived, alongside strict math operators (`*`, `/`, `%`) inside `time::to_secs`, `time::to_millis`, and `time::to_iso8601` methods. Finally, specific bounding checks involving exclusive interval boundaries in `contains` and `overlaps` were weakly asserted allowing `>` to mutate into `>=`.
**Diagnosis:** MISSING_TEST / WEAK_TEST - Existing tests tested positive path boundary logic well, but neglected specific maximum boundary exact values (`MAX_VALID_TIMESTAMP`), exact conversion validation that strictly broke if `/` turned into `%`, and exact exclusion of boundaries inside interval intersections.
**Kill Shot:** Appended explicit exact boundary tests `test_timerange_from_at_exact_boundaries`, `test_time_to_secs_millis_exact_math`, `test_time_to_iso8601_exact_content`, and `test_timerange_contains_exact_boundary` directly to `tests/sentry_temporal.rs`.
**[Weak Test Coverage in HLC Unchecked Constructor]**
**Module:** `aletheiadb::core::hlc`
**Summary:** Mutation testing indicated a surviving mutant in `HybridTimestamp::new_unchecked` replacing its logic with a default return `Default::default()`.
**Diagnosis:** MISSING_TEST - There were no direct exhaustive tests executing `HybridTimestamp::new_unchecked` ensuring internal struct layout is explicitly mapped from input bounds rather than failing softly via tests passing with empty defaults.
**Kill Shot:** Appended targeted boundary test `test_hybrid_timestamp_new_unchecked_exhaustive` within `src/core/hlc.rs` directly explicitly validating field mappings from inputs.
