**[Weak Test Coverage in DeduplicationPolicy]**
**Module:** src/index/temporal.rs
**Summary:** The mutant `delete !` in `EntityTimeline::insert_batch` (DeduplicationPolicy::Reject) survived because existing tests only checked that `Reject` works for duplicates (failure case), but never verified it works for valid data (success case).
**Diagnosis:** WEAK_TEST - Missing positive test case for `DeduplicationPolicy::Reject`.
**Kill Shot:** Added `test_batch_insert_reject_policy_valid_batch` which asserts that `insert_batch` with `Reject` policy succeeds for a batch with unique items. Verified manually that this test fails if the mutant is applied.

**[Missing Coverage for Negative Property Values]**
**Module:** src/query/parser.rs
**Summary:** The mutant `delete -` in `Parser::parse_value` (specifically in the `Token::Dash` branch) survived because existing tests only used positive property values or unspaced negative literals handled by the Lexer directly.
**Diagnosis:** MISSING_COVERAGE - No test covered the parsing of spaced negative numeric literals (e.g., `- 5`) which are handled via the `Dash` token branch in the parser.
**Kill Shot:** Added `tests/sentinel_parser.rs` with `test_parse_negative_property_value_spaced` and `test_parse_negative_float_property_value_spaced` to verify that spaced negative numbers are parsed correctly as negative values.
