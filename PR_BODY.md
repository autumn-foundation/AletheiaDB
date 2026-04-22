### Summary
Overall mutation-readiness assessment of `src/core/error.rs` module formatting functions.

### Verdict table
| Function / Area | Verdict | Reason |
| :--- | :--- | :--- |
| `format_index_not_found` | 🔴 Condemned | Completely lacked tests, allowing mutations returning empty/default strings to survive. |
| `format_clock_skew` | 🟡 Suspect | Tested negative and zero drift, but missed positive drift/forward branch, allowing logic mutations like `<` to `==` to survive. |

### Priority fixes
1. `format_index_not_found`: Added `test_query_error_index_not_found_display` to explicitly check the output formatting of `QueryError::IndexNotFound` with and without a hint.
2. `format_clock_skew`: Added `test_clock_skew_display_positive_drift_is_forward` to assert that the "forward" branch is taken and properly formats the result for positive drift values.

### Missing coverage
- Formatting of `QueryError::IndexNotFound` was completely absent.
- Positive drift bounds checking in `format_clock_skew`.
