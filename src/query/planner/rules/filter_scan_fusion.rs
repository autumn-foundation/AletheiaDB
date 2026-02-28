//! Filter-Scan Fusion Optimization
//!
//! The "Filter-Scan Fusion" is an essential query planner rule that dramatically accelerates
//! query execution. When a logical plan requires finding a node by a specific label and a
//! specific property value, the naïve approach is to scan *all* nodes of that label, and
//! apply a filter to each one in memory.
//!
//! Instead, this rule intercepts a `Filter(Eq { key, value })` operation sitting directly
//! above a `Scan(NodeScan { label: Some(l) })` and fuses them into a single, highly-optimized
//! `Scan(PropertyScan { label, key, value })` operation.
//!
//! By delegating this combined operation to the storage layer
//! (`CurrentStorage::find_nodes_by_property`), AletheiaDB can utilize internal property
//! indices or optimized storage layouts, turning an $O(N)$ full table scan into an $O(1)$
//! or $O(\log N)$ targeted lookup.
//!
//! # Example Transformation
//!
//! **Before (Naïve Plan)**:
//! ```text
//! Filter(Eq { key: "name", value: "Alice" })
//!   └─ NodeScan { label: Some("Person") }
//! ```
//!
//! **After (Optimized Plan)**:
//! ```text
//! PropertyScan { label: "Person", key: "name", value: "Alice" }
//! ```

use crate::core::error::Result;
use crate::query::ir::Predicate;
use crate::query::plan::{LogicalOp, LogicalPlan, ScanOp, UnaryOp};

use super::{OptimizationRule, Statistics};

/// Fuses a `Filter(Eq)` operation and a `NodeScan(label)` operation into a single `PropertyScan`.
///
/// This optimizer rule traverses the logical query plan tree looking for opportunities to
/// push down equality predicates directly into the storage scan phase.
///
/// # Examples
///
/// ```rust
/// use aletheiadb::query::plan::{LogicalPlan, LogicalOp, ScanOp, UnaryOp};
/// use aletheiadb::query::ir::Predicate;
/// use aletheiadb::query::planner::rules::{OptimizationRule, FilterScanFusion, Statistics};
///
/// // Create a naïve query plan: "Scan all Persons, then filter for name = Alice"
/// let naive_plan = LogicalPlan::new(LogicalOp::unary(
///     UnaryOp::Filter(Predicate::eq("name", "Alice")),
///     LogicalOp::Scan(ScanOp::NodeScan {
///         label: Some("Person".to_string()),
///         estimated_rows: None,
///     }),
/// ));
///
/// // Apply the FilterScanFusion rule
/// let rule = FilterScanFusion;
/// let stats = Statistics::default();
/// let optimized_plan = rule.apply(&naive_plan, &stats).unwrap().unwrap();
///
/// // The plan is now fused into a single PropertyScan
/// match &optimized_plan.root {
///     LogicalOp::Scan(ScanOp::PropertyScan { label, key, .. }) => {
///         assert_eq!(label, "Person");
///         assert_eq!(key, "name");
///     }
///     _ => panic!("Plan was not fused!"),
/// }
/// ```
///
/// ## Panics
/// This rule itself does not panic under normal execution.
///
/// ## Limits & Exclusions
/// - Non-Eq predicates (e.g., `Gt`, `Lt`, `Like`) are not currently fused and are left unchanged.
/// - Scans without a label (e.g., scan all nodes in the database) are not fused, as property
///   scans require a label context for optimal performance.
/// - Internal system keys (starting with `_`, like `_label`) are ignored.
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
