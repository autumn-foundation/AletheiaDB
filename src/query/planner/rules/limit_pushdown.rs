//! Limit Pushdown Optimization
//!
//! Propagates LIMIT operations down through the plan tree where safe,
//! enabling early termination and reducing work.

use crate::core::error::Result;
use crate::query::plan::{LogicalOp, LogicalPlan, UnaryOp};

use super::{OptimizationRule, Statistics};

/// Limit pushdown optimization rule.
///
/// This rule propagates LIMIT operations through certain operators
/// to enable early termination. This is particularly useful for
/// vector search and top-k queries.
///
/// # Example Transformation
///
/// Before:
/// ```text
/// Limit(10)
///   Sort(score DESC)
///     Traverse(KNOWS)
///       NodeLookup([1])
/// ```
///
/// After:
/// ```text
/// Limit(10)
///   Sort(score DESC, limit: 10)  // Top-K sort
///     Traverse(KNOWS)
///       NodeLookup([1])
/// ```
pub struct LimitPushdown;

impl OptimizationRule for LimitPushdown {
    fn name(&self) -> &str {
        "limit-pushdown"
    }

    fn apply(&self, plan: &LogicalPlan, _stats: &Statistics) -> Result<Option<LogicalPlan>> {
        let (new_root, changed) = self.push_down(&plan.root, None)?;

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

impl LimitPushdown {
    /// Recursively push down limit information where possible.
    ///
    /// `limit` is Some(n) if there's a LIMIT above that we might propagate.
    fn push_down(&self, op: &LogicalOp, limit: Option<usize>) -> Result<(LogicalOp, bool)> {
        match op {
            // Limit operation: propagate limit value down
            LogicalOp::Unary {
                op: UnaryOp::Limit(n),
                input,
            } => {
                // Use the smaller of the two limits
                let effective_limit = limit.map(|l| l.min(*n)).unwrap_or(*n);
                let (optimized_input, changed) = self.push_down(input, Some(effective_limit))?;

                // If the child is also a Limit, we can combine them
                if let LogicalOp::Unary {
                    op: UnaryOp::Limit(child_limit),
                    input: child_input,
                } = &optimized_input
                {
                    let combined_limit = effective_limit.min(*child_limit);
                    return Ok((
                        LogicalOp::unary(UnaryOp::Limit(combined_limit), (**child_input).clone()),
                        true,
                    ));
                }

                Ok((
                    LogicalOp::unary(UnaryOp::Limit(effective_limit), optimized_input),
                    changed || effective_limit != *n,
                ))
            }

            // VectorRank: can use limit hint for top-k optimization
            LogicalOp::Unary {
                op:
                    UnaryOp::VectorRank {
                        embedding,
                        top_k,
                        property_key,
                    },
                input,
            } => {
                let (optimized_input, input_changed) = self.push_down(input, None)?;

                // If there's a limit and it's smaller than current top_k, use it
                let new_top_k = match (limit, *top_k) {
                    (Some(l), Some(k)) => Some(l.min(k)),
                    (Some(l), None) => Some(l),
                    (None, k) => k,
                };

                let changed = input_changed || new_top_k != *top_k;

                Ok((
                    LogicalOp::unary(
                        UnaryOp::VectorRank {
                            embedding: embedding.clone(),
                            top_k: new_top_k,
                            property_key: property_key.clone(),
                        },
                        optimized_input,
                    ),
                    changed,
                ))
            }

            // Sort: propagate limit for top-k sort optimization
            // (handled at physical plan level, but we track it)
            LogicalOp::Unary {
                op: UnaryOp::Sort { key, descending },
                input,
            } => {
                let (optimized_input, changed) = self.push_down(input, None)?;
                Ok((
                    LogicalOp::unary(
                        UnaryOp::Sort {
                            key: key.clone(),
                            descending: *descending,
                        },
                        optimized_input,
                    ),
                    changed,
                ))
            }

            // Filter: propagate limit through (filtering might reduce result count)
            LogicalOp::Unary {
                op: UnaryOp::Filter(predicate),
                input,
            } => {
                let (optimized_input, changed) = self.push_down(input, limit)?;
                Ok((
                    LogicalOp::unary(UnaryOp::Filter(predicate.clone()), optimized_input),
                    changed,
                ))
            }

            // Project: propagate limit through (projection doesn't change row count)
            LogicalOp::Unary {
                op: UnaryOp::Project(props),
                input,
            } => {
                let (optimized_input, changed) = self.push_down(input, limit)?;
                Ok((
                    LogicalOp::unary(UnaryOp::Project(props.clone()), optimized_input),
                    changed,
                ))
            }

            // Other unary ops: recursively optimize
            LogicalOp::Unary { op, input } => {
                let (optimized_input, changed) = self.push_down(input, None)?;
                Ok((LogicalOp::unary(op.clone(), optimized_input), changed))
            }

            // Binary: recursively optimize both branches (don't propagate limit)
            LogicalOp::Binary { op, left, right } => {
                let (opt_left, left_changed) = self.push_down(left, None)?;
                let (opt_right, right_changed) = self.push_down(right, None)?;
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
    use crate::core::NodeId;
    use crate::query::plan::ScanOp;
    use std::sync::Arc;

    fn test_stats() -> Statistics {
        Statistics::default()
    }

    #[test]
    fn test_combine_consecutive_limits() {
        let rule = LimitPushdown;
        let stats = test_stats();

        // Limit(5, Limit(10, Scan)) -> Limit(5, Scan)
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Limit(5),
            LogicalOp::unary(
                UnaryOp::Limit(10),
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some());

        let new_plan = result.unwrap();
        // Should be Limit(5, Scan) - no nested limit
        match &new_plan.root {
            LogicalOp::Unary {
                op: UnaryOp::Limit(n),
                input,
            } => {
                assert_eq!(*n, 5);
                assert!(matches!(input.as_ref(), LogicalOp::Scan(_)));
            }
            _ => panic!("Expected Limit"),
        }
    }

    #[test]
    fn test_propagate_limit_to_vector_rank() {
        let rule = LimitPushdown;
        let stats = test_stats();

        // Limit(5, VectorRank(top_k=10, Scan)) -> Limit(5, VectorRank(top_k=5, Scan))
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Limit(5),
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
        // VectorRank should have top_k=5 now
        match &new_plan.root {
            LogicalOp::Unary {
                op: UnaryOp::Limit(_),
                input,
            } => match input.as_ref() {
                LogicalOp::Unary {
                    op: UnaryOp::VectorRank { top_k, .. },
                    ..
                } => {
                    assert_eq!(*top_k, Some(5));
                }
                _ => panic!("Expected VectorRank"),
            },
            _ => panic!("Expected Limit"),
        }
    }

    #[test]
    fn test_no_change_for_simple_limit() {
        let rule = LimitPushdown;
        let stats = test_stats();

        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Limit(10),
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none()); // No change needed
    }
}
