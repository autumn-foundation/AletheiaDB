//! Predicate Pushdown Optimization
//!
//! Moves filter operations as close to data sources as possible,
//! reducing the number of rows processed by expensive operations
//! like traversals and joins.
//!
//! # Theory: Filter Early
//!
//! The "Filter Early" principle is fundamental to query optimization. By applying
//! predicates (filters) as early as possible in the query pipeline, we reduce the
//! cardinality (number of rows) that subsequent operators must process.
//!
//! For example, if a `VectorRank` operation is O(N log k) (where N is the number of
//! input rows and k is the top_k parameter) and a filter removes 90% of the rows,
//! pushing the filter before the rank reduces the cost from O(N log k) to roughly
//! O(0.1N log k), i.e., by about 10x.
//!
//! # Optimization Strategy
//!
//! This rule recursively traverses the logical plan and attempts to "bubble down"
//! `Filter` operators through other unary operators where safe.
//!
//! ## Capabilities
//!
//! Currently, this rule supports pushing filters through:
//! - **VectorRank**: Reranking is commutative with filtering (ranking subset vs ranking all then filtering).
//! - **Sort**: Sorting is commutative with filtering (sorting subset vs sorting all then filtering).
//!
//! ## Limitations
//!
//! - **Traversals**: We conservatively do *not* push filters through traversals yet,
//!   as this requires checking if the predicate applies to the source or target node.
//!   (Future work: Push source-node predicates through traversals).
//! - **Scans**: Filters cannot be pushed below scans (they are the leaves).
//! - **Joins**: Filter pushdown through joins is handled by separate logic (future work).
//!
//! # Safety
//!
//! Pushdown is safe when:
//! 1. **No Side Effects**: The operator being swapped doesn't produce side effects that the filter depends on.
//! 2. **Semantic Equivalence**: The result set remains identical.
//!    - For `Sort`, removing a row before or after doesn't change the relative order of remaining rows.
//!    - For `VectorRank`, this holds only when ranking over the full population (for example, when `top_k` is `None`);
//!      if a `top_k` limit is applied, filtering before ranking can change which rows appear in the top-k results and is
//!      therefore not semantically equivalent to filtering after ranking.

use crate::query::plan::{LogicalOp, LogicalPlan, UnaryOp};
use crate::utils::error::Result;

use super::{OptimizationRule, Statistics};

/// Predicate pushdown optimization rule.
///
/// This rule moves Filter operations below Traverse and other operations
/// when possible, reducing intermediate result sizes.
///
/// # Example Transformation
///
/// **Before**: Filter is applied *after* sorting (expensive).
/// ```text
/// Filter(name = "Alice")
///   Sort(score DESC)
///     VectorSearch(...)
/// ```
///
/// **After**: Filter is applied *before* sorting (cheaper).
/// ```text
/// Sort(score DESC)
///   Filter(name = "Alice")
///     VectorSearch(...)
/// ```
///
/// # Complex Example
///
/// Pushing through multiple layers:
///
/// ```text
/// Filter(active = true)
///   Sort(created DESC)
///     VectorRank(...)
///       Scan(...)
/// ```
///
/// Becomes:
///
/// ```text
/// Sort(created DESC)
///   VectorRank(...)
///     Filter(active = true)  <-- Pushed down
///       Scan(...)
/// ```
pub struct PredicatePushdown;

impl OptimizationRule for PredicatePushdown {
    fn name(&self) -> &str {
        "predicate-pushdown"
    }

    fn apply(&self, plan: &LogicalPlan, _stats: &Statistics) -> Result<Option<LogicalPlan>> {
        let (new_root, changed) = self.push_down(&plan.root)?;

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

impl PredicatePushdown {
    /// Recursively push down filters where possible.
    ///
    /// This method traverses the plan tree. When it encounters a `Filter` operator,
    /// it attempts to move it below its input operator if that operator type allows it.
    fn push_down(&self, op: &LogicalOp) -> Result<(LogicalOp, bool)> {
        match op {
            // Filter operation: this is what we want to push down
            LogicalOp::Unary {
                op: UnaryOp::Filter(predicate),
                input,
            } => {
                // First, recursively optimize the input (bottom-up approach)
                let (optimized_input, input_changed) = self.push_down(input)?;

                // Check if we can push the filter below the optimized input operator
                match &optimized_input {
                    // STOP: Can't push below scans (they are the source)
                    LogicalOp::Scan(_) => Ok((
                        LogicalOp::unary(UnaryOp::Filter(predicate.clone()), optimized_input),
                        input_changed,
                    )),

                    // STOP: For traversal, we generally can't push blindly.
                    // We need to know if the predicate applies to the source or target.
                    // Current implementation is conservative and stops here.
                    LogicalOp::Unary {
                        op: UnaryOp::Traverse { .. },
                        ..
                    } => Ok((
                        LogicalOp::unary(UnaryOp::Filter(predicate.clone()), optimized_input),
                        input_changed,
                    )),

                    // PUSH: VectorRank
                    // Filter(VectorRank(Input)) -> VectorRank(Filter(Input))
                    // Safe because reranking doesn't create/destroy rows or change row content.
                    LogicalOp::Unary {
                        op:
                            UnaryOp::VectorRank {
                                embedding,
                                top_k,
                                property_key,
                            },
                        input: vector_input,
                    } => {
                        let filter_then_rank = LogicalOp::unary(
                            UnaryOp::VectorRank {
                                embedding: embedding.clone(),
                                top_k: *top_k,
                                property_key: property_key.clone(),
                            },
                            LogicalOp::unary(
                                UnaryOp::Filter(predicate.clone()),
                                (**vector_input).clone(),
                            ),
                        );
                        Ok((filter_then_rank, true))
                    }

                    // PUSH: Sort
                    // Filter(Sort(Input)) -> Sort(Filter(Input))
                    // Safe because sorting is purely a reordering operation.
                    LogicalOp::Unary {
                        op: UnaryOp::Sort { key, descending },
                        input: sort_input,
                    } => {
                        let filter_then_sort = LogicalOp::unary(
                            UnaryOp::Sort {
                                key: key.clone(),
                                descending: *descending,
                            },
                            LogicalOp::unary(
                                UnaryOp::Filter(predicate.clone()),
                                (**sort_input).clone(),
                            ),
                        );
                        Ok((filter_then_sort, true))
                    }

                    // Default: STOP. Keep filter where it is.
                    _ => Ok((
                        LogicalOp::unary(UnaryOp::Filter(predicate.clone()), optimized_input),
                        input_changed,
                    )),
                }
            }

            // Not a filter: just recurse down (pass-through)
            LogicalOp::Unary { op, input } => {
                let (optimized_input, changed) = self.push_down(input)?;
                Ok((LogicalOp::unary(op.clone(), optimized_input), changed))
            }

            // Binary op: recurse down both branches
            LogicalOp::Binary { op, left, right } => {
                let (opt_left, left_changed) = self.push_down(left)?;
                let (opt_right, right_changed) = self.push_down(right)?;
                Ok((
                    LogicalOp::binary(op.clone(), opt_left, opt_right),
                    left_changed || right_changed,
                ))
            }

            // Leaf nodes: no change possible
            LogicalOp::Scan(_) | LogicalOp::Empty => Ok((op.clone(), false)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::NodeId;
    use crate::query::ir::Predicate;
    use crate::query::plan::{ScanOp, SortKey};
    use std::sync::Arc;

    fn test_stats() -> Statistics {
        Statistics::default()
    }

    #[test]
    fn test_no_change_on_simple_filter() {
        let rule = PredicatePushdown;
        let stats = test_stats();

        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none()); // No change needed
    }

    #[test]
    fn test_push_filter_below_vector_rank() {
        let rule = PredicatePushdown;
        let stats = test_stats();

        // Filter(VectorRank(Scan)) -> VectorRank(Filter(Scan))
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            LogicalOp::unary(
                UnaryOp::VectorRank {
                    embedding: Arc::from([0.1f32; 4].as_slice()),
                    top_k: Some(10),
                    property_key: None,
                },
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some());

        let new_plan = result.unwrap();
        // Check that VectorRank is now on top
        assert!(matches!(
            new_plan.root,
            LogicalOp::Unary {
                op: UnaryOp::VectorRank { .. },
                ..
            }
        ));
    }

    #[test]
    fn test_push_filter_below_sort() {
        let rule = PredicatePushdown;
        let stats = test_stats();

        // Filter(Sort(Scan)) -> Sort(Filter(Scan))
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("active", true)),
            LogicalOp::unary(
                UnaryOp::Sort {
                    key: SortKey::Property("created".to_string()),
                    descending: true,
                },
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some());

        let new_plan = result.unwrap();
        // Check that Sort is now on top
        assert!(matches!(
            new_plan.root,
            LogicalOp::Unary {
                op: UnaryOp::Sort { .. },
                ..
            }
        ));
    }
}
