1. **Explore the codebase:** We've inspected `src/query/parser.rs` and its mutant list. Although the `cargo mutants` suite times out, its initial analysis output listed mutants that hinted at missing or weak bounds testing. For instance, the `< 0` check in `validate_non_negative` lacked tests at the exact boundary of `0` (e.g. `MATCH (a)-[:KNOWS*0]->(b)`).
2. **Review `src/query/parser.rs` tests:**
   - Sentry already added tests for `LIMIT 0` and `SKIP 0`.
   - But there are no tests for `*0` (exact depth 0) and `*0..2` (range starting with 0), which would leave the `>= 0` condition vulnerable to `<` vs `<=` mutants.
   - The implicit logic around `is_relationship_start` means `MATCH (a) (b)` would fail eventually at `is_at_end()` but we lack an explicit test ensuring this syntax correctly triggers an error, preventing it from being silently accepted if the loop changes.
3. **Action:** Act as the Elenchus persona.
   - Add targeted tests: `test_parse_zero_depth_path` and `test_parse_zero_depth_range` to `src/query/parser.rs`.
   - Add `test_parse_error_missing_relationship` to test the case of trailing tokens that aren't commas or relationships.
   - Update `.jules/elenchus.md` to document the verification of Sentry's tests for spaced floats and limits, and report the new bounds tests.
4. **Pre-commit and submit.** Ensure proper testing, verification, review, and reflection are done by calling the `pre_commit_instructions` tool and addressing the feedback.
