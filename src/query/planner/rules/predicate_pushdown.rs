//! Predicate Pushdown Optimization
//!
//! Moves filter operations as close to data sources as possible,
//! reducing the number of rows processed by expensive operations
//! like traversals and joins.

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
/// Before:
/// ```text
/// Filter(name = "Alice")
///   Traverse(KNOWS)
///     NodeLookup([1])
/// ```
///
/// After (if filter can apply to traversal targets):
/// ```text
/// Traverse(KNOWS)
///   Filter(name = "Alice")
///     NodeLookup([1])
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
    fn push_down(&self, op: &LogicalOp) -> Result<(LogicalOp, bool)> {
        match op {
            // Filter above Traverse: can push down if filter applies to source nodes
            LogicalOp::Unary {
                op: UnaryOp::Filter(predicate),
                input,
            } => {
                // First, recursively optimize the input
                let (optimized_input, input_changed) = self.push_down(input)?;

                // Check if we can push the filter below the input operation
                match &optimized_input {
                    // Can't push below scans (they are the source)
                    LogicalOp::Scan(_) => Ok((
                        LogicalOp::unary(UnaryOp::Filter(predicate.clone()), optimized_input),
                        input_changed,
                    )),

                    // For traversal, we can push the filter to apply to source nodes
                    // if the predicate applies to the source, not the target
                    // For now, keep filter above (conservative approach)
                    LogicalOp::Unary {
                        op: UnaryOp::Traverse { .. },
                        ..
                    } => Ok((
                        LogicalOp::unary(UnaryOp::Filter(predicate.clone()), optimized_input),
                        input_changed,
                    )),

                    // Push filter below VectorRank (reranking doesn't change what we filter)
                    LogicalOp::Unary {
                        op: UnaryOp::VectorRank { embedding, top_k },
                        input: vector_input,
                    } => {
                        // Push filter below vector rank
                        let filter_then_rank = LogicalOp::unary(
                            UnaryOp::VectorRank {
                                embedding: embedding.clone(),
                                top_k: *top_k,
                            },
                            LogicalOp::unary(
                                UnaryOp::Filter(predicate.clone()),
                                (**vector_input).clone(),
                            ),
                        );
                        Ok((filter_then_rank, true))
                    }

                    // Push filter below Sort (filtering doesn't affect sort order)
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

                    // Default: keep filter where it is
                    _ => Ok((
                        LogicalOp::unary(UnaryOp::Filter(predicate.clone()), optimized_input),
                        input_changed,
                    )),
                }
            }

            // Recursively optimize other unary operations
            LogicalOp::Unary { op, input } => {
                let (optimized_input, changed) = self.push_down(input)?;
                Ok((LogicalOp::unary(op.clone(), optimized_input), changed))
            }

            // Recursively optimize binary operations
            LogicalOp::Binary { op, left, right } => {
                let (opt_left, left_changed) = self.push_down(left)?;
                let (opt_right, right_changed) = self.push_down(right)?;
                Ok((
                    LogicalOp::binary(op.clone(), opt_left, opt_right),
                    left_changed || right_changed,
                ))
            }

            // Leaf nodes don't change
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
