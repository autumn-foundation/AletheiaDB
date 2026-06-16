//! Filter-Scan Fusion Optimization
//!
//! When a `Filter(Eq { key, value })` sits directly above a `Scan(NodeScan { label: Some(l) })`,
//! fuse them into a single `Scan(PropertyScan { label, key, value })`. This avoids the full
//! scan + per-node predicate evaluation by delegating to `CurrentStorage::find_nodes_by_property`.

use crate::core::error::Result;
use crate::query::ir::Predicate;
use crate::query::plan::{LogicalOp, LogicalPlan, ScanOp, UnaryOp};

use super::{OptimizationRule, Statistics};

/// Fuses `Filter(Eq)` + `NodeScan(label)` into a single `PropertyScan`.
///
/// # Example Transformation
///
/// **Before**:
/// ```text
/// Filter(Eq { key: "name", value: "Alice" })
///   NodeScan { label: Some("Person") }
/// ```
///
/// **After**:
/// ```text
/// PropertyScan { label: "Person", key: "name", value: "Alice" }
/// ```
///
/// Non-Eq predicates and scans without a label are left unchanged.
///
/// ## Examples
///
/// ```rust
/// use aletheiadb::query::planner::rules::{OptimizationRule, FilterScanFusion};
/// use aletheiadb::query::planner::stats::Statistics;
/// use aletheiadb::query::plan::{LogicalPlan, LogicalOp, UnaryOp, ScanOp};
/// use aletheiadb::query::ir::{Predicate, PredicateValue};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // 1. Construct a sub-optimal plan: Filter on Top of NodeScan
/// let scan = LogicalOp::Scan(ScanOp::NodeScan {
///     label: Some("Person".into()),
///     estimated_rows: Some(100),
/// });
/// let filter = LogicalOp::unary(
///     UnaryOp::Filter(Predicate::Eq {
///         key: "name".into(),
///         value: PredicateValue::String("Alice".into())
///     }),
///     scan
/// );
///
/// let plan = LogicalPlan { root: filter, temporal_context: None, hints: Default::default() };
///
/// // 2. Apply the rule
/// let rule = FilterScanFusion;
/// let stats = Statistics::new();
/// let optimized_plan = rule.apply(&plan, &stats)?.unwrap(); // unwraps if `changed == true`
///
/// // 3. The rule fuses them into a single PropertyScan
/// assert!(matches!(
///     optimized_plan.root,
///     LogicalOp::Scan(ScanOp::PropertyScan { .. })
/// ));
/// # Ok(())
/// # }
/// ```
pub struct FilterScanFusion;

impl OptimizationRule for FilterScanFusion {
    fn name(&self) -> &str {
        "filter-scan-fusion"
    }

    fn apply(&self, plan: &LogicalPlan, _stats: &Statistics) -> Result<Option<LogicalPlan>> {
        let (new_root, changed) = self.fuse(&plan.root)?;

        if changed {
            Ok(Some(LogicalPlan {
                root: new_root,
                temporal_context: plan.temporal_context.clone(),
                hints: plan.hints.clone(),
            }))
        } else {
            Ok(None)
        }
    }
}

impl FilterScanFusion {
    fn fuse(&self, op: &LogicalOp) -> Result<(LogicalOp, bool)> {
        match op {
            // Pattern: Filter(Eq { key, value }) over NodeScan { label: Some(l) }
            // Skip pseudo-keys like "_label" which are internal label filters, not properties.
            LogicalOp::Unary {
                op: UnaryOp::Filter(Predicate::Eq { key, value }),
                input,
            } if !key.starts_with('_') => {
                // First, recursively optimize the input
                let (optimized_input, input_changed) = self.fuse(input)?;

                // Check if the (possibly optimized) input is a labeled NodeScan
                if let LogicalOp::Scan(ScanOp::NodeScan {
                    label: Some(label), ..
                }) = &optimized_input
                {
                    // Fuse into PropertyScan
                    return Ok((
                        LogicalOp::Scan(ScanOp::PropertyScan {
                            label: label.clone(),
                            key: key.clone(),
                            value: value.clone(),
                        }),
                        true,
                    ));
                }

                // Can't fuse - keep the filter
                Ok((
                    LogicalOp::unary(
                        UnaryOp::Filter(Predicate::Eq {
                            key: key.clone(),
                            value: value.clone(),
                        }),
                        optimized_input,
                    ),
                    input_changed,
                ))
            }

            // Non-filter unary: recurse
            LogicalOp::Unary { op, input } => {
                let (optimized_input, changed) = self.fuse(input)?;
                Ok((LogicalOp::unary(op.clone(), optimized_input), changed))
            }

            // Binary: recurse both branches
            LogicalOp::Binary { op, left, right } => {
                let (opt_left, left_changed) = self.fuse(left)?;
                let (opt_right, right_changed) = self.fuse(right)?;
                Ok((
                    LogicalOp::binary(op.clone(), opt_left, opt_right),
                    left_changed || right_changed,
                ))
            }

            // Leaf nodes: no change
            LogicalOp::Scan(_) | LogicalOp::Empty => Ok((op.clone(), false)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::ir::Predicate;
    use crate::query::plan::ScanOp;
    use crate::query::planner::stats::Statistics;

    fn test_stats() -> Statistics {
        Statistics::default()
    }

    #[test]
    fn test_fuses_eq_filter_on_labeled_scan() {
        let rule = FilterScanFusion;
        let stats = test_stats();

        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: None,
            }),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some(), "Should fuse Filter+NodeScan");

        let new_plan = result.unwrap();
        match &new_plan.root {
            LogicalOp::Scan(ScanOp::PropertyScan { label, key, .. }) => {
                assert_eq!(label, "Person");
                assert_eq!(key, "name");
            }
            other => panic!("Expected PropertyScan, got {:?}", other),
        }
    }

    #[test]
    fn test_no_fusion_without_label() {
        let rule = FilterScanFusion;
        let stats = test_stats();

        // NodeScan without label - can't fuse
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: None,
                estimated_rows: None,
            }),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none(), "Should not fuse without label");
    }

    #[test]
    fn test_no_fusion_for_non_eq_filter() {
        let rule = FilterScanFusion;
        let stats = test_stats();

        // Gt filter - not an Eq, so no fusion
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::gt("age", 30i64)),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: None,
            }),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none(), "Should not fuse non-Eq filter");
    }

    #[test]
    fn test_preserves_temporal_context() {
        use crate::query::plan::TemporalContext;

        let rule = FilterScanFusion;
        let stats = test_stats();

        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: None,
            }),
        ))
        .with_temporal_context(TemporalContext::default());

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some());
        assert!(result.unwrap().temporal_context.is_some());
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;
    use crate::query::ir::Predicate;
    use crate::query::plan::{BinaryOp, LogicalOp, LogicalPlan, ScanOp, UnaryOp};
    use crate::query::planner::stats::Statistics;

    #[test]
    fn test_filter_scan_fusion_mutants() {
        let rule = FilterScanFusion;
        let stats = Statistics::default();

        // 1. replace match guard !key.starts_with('_') with true in FilterScanFusion::fuse
        // test: Starts with '_' should not fuse
        let filter = LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("_label", "Alice")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".into()),
                estimated_rows: Some(100),
            }),
        );
        let plan = LogicalPlan::new(filter);
        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none(), "Should not fuse pseudo-key");

        // 2. replace || with && in FilterScanFusion::fuse
        // test: binary op partial fusion
        let left = LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".into()),
                estimated_rows: Some(100),
            }),
        );
        let right = LogicalOp::Scan(ScanOp::NodeScan {
            label: Some("Company".into()),
            estimated_rows: Some(100),
        });
        let filter_binary = LogicalOp::binary(BinaryOp::Union, left, right);
        let plan_binary = LogicalPlan::new(filter_binary);
        let result_binary = rule.apply(&plan_binary, &stats).unwrap();
        assert!(result_binary.is_some(), "Should fuse left binary side");

        // 3. Name check
        assert_eq!(rule.name(), "filter-scan-fusion");

        // 4. Default fusion check (tests apply)
        let scan = LogicalOp::Scan(ScanOp::NodeScan {
            label: Some("Person".into()),
            estimated_rows: Some(100),
        });
        let filter = LogicalOp::unary(UnaryOp::Filter(Predicate::eq("name", "Alice")), scan);
        let plan = LogicalPlan::new(filter);
        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some(), "Should fuse");

        // 5. Test recursive UnaryOp that isn't a filter Eq directly
        let inner_filter = LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".into()),
                estimated_rows: Some(100),
            }),
        );
        let outer_limit = LogicalOp::unary(UnaryOp::Limit(10), inner_filter);
        let plan_limit = LogicalPlan::new(outer_limit);
        let result_limit = rule.apply(&plan_limit, &stats).unwrap();
        assert!(result_limit.is_some(), "Should fuse inside limit");
    }
}
