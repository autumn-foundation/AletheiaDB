**[LimitPushdown Audit]**
**Module:** `query::planner::rules::limit_pushdown`
**Severity:** 🔴 Critical
**Finding:** Multiple tests used `assert!(result.is_none())` or `assert!(result.is_some())` rather than verifying the actual propagated `LogicalPlan` structure, leading to incomplete mutation testing coverage and false confidence that partial updates were correctly chained.
**Evidence:** `test_propagate_limit_through_project` returned `is_some()` but didn't verify if `top_k` was applied correctly inside the inner plans.
**Recommendation:** Refactored tests to construct an explicit expected `LogicalPlan` and used `assert_eq!(result, Some(expected_plan))` to guarantee structural correctness.

**[FilterScanFusion Audit]**
**Module:** `query::planner::rules::filter_scan_fusion`
**Severity:** 🟡 Suspect
**Finding:** Tests like `test_fuses_eq_filter_on_labeled_scan` manually deconstructed the logical plan using `match` and weak assertions, lacking full equivalence checks. Additionally, an internal property test `test_no_fusion_internal_property` was missing for the `!key.starts_with('_')` optimization guard.
**Evidence:** Mutation of `!key.starts_with('_')` went undetected due to a missing test case.
**Recommendation:** Replaced manual matches with full `assert_eq!(result, Some(expected_plan))` checks and added `test_no_fusion_internal_property`.

**[OperationReordering Audit]**
**Module:** `query::planner::rules::operation_reordering`
**Severity:** 🔴 Critical
**Finding:** `test_reorder_filters_by_selectivity` and `test_reorder_join_operands_by_size` manually inspected plan nodes instead of validating the complete tree transformation, which was fragile and less readable.
**Evidence:** The test logic was difficult to follow and allowed mutations deep in the tree to escape if they matched partial shapes. `test_reorder_join_operands_by_size` used a shallow match to test the left and right scan labels, completely omitting the join keys and inner structure equality.
**Recommendation:** Refactored tests to use strict `assert_eq!(result, Some(expected_plan))` across all complex reordering tests to harden the suite.

**[PredicatePushdown Audit]**
**Module:** `query::planner::rules::predicate_pushdown`
**Severity:** 🟡 Suspect
**Finding:** `test_apply_changed` checked for `is_some()` without validating whether the predicate was actually pushed down correctly inside the Sort operation.
**Evidence:** The test passed even if the optimization produced a semantically invalid, garbage plan as long as `changed` was true.
**Recommendation:** Replaced weak assertions with strong `assert_eq!(result, Some(expected_plan))` checks.
