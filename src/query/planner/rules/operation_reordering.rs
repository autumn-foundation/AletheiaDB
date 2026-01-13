//! Operation Reordering Optimization
//!
//! Reorders operations based on estimated costs and selectivity to minimize
//! overall query execution cost. This includes:
//! - Reordering filters by selectivity (most selective first)
//! - Reordering join operands (smaller relation as build side)
//! - Choosing optimal operation order based on cardinality estimates

use crate::query::ir::Predicate;
use crate::query::plan::{BinaryOp, LogicalOp, LogicalPlan, ScanOp, UnaryOp};
use crate::utils::error::Result;

use super::{OptimizationRule, Statistics};

// Selectivity estimates for different predicate types
// (0.0 = filters everything, 1.0 = filters nothing)
const NULL_CHECK_SELECTIVITY: f64 = 0.1; // Null checks are typically selective
const EXISTENCE_CHECK_SELECTIVITY: f64 = 0.1; // Property existence checks
const CONJUNCTION_SELECTIVITY: f64 = 0.1; // AND predicates (typically selective)
const IN_PREDICATE_SELECTIVITY: f64 = 0.15; // IN predicates (depends on list size)
const STRING_PREDICATE_SELECTIVITY: f64 = 0.2; // Contains/StartsWith/EndsWith
const RANGE_PREDICATE_SELECTIVITY: f64 = 0.3; // Gt/Lt/Gte/Lte
const DISJUNCTION_SELECTIVITY: f64 = 0.5; // OR predicates (less selective)
const NOT_SELECTIVITY: f64 = 0.5; // NOT predicates (medium selectivity)
const NOT_EQUALS_SELECTIVITY: f64 = 0.9; // Not-equals (typically less selective)
const TRUE_SELECTIVITY: f64 = 1.0; // Filters nothing
const FALSE_SELECTIVITY: f64 = 0.0; // Filters everything

/// Operation reordering optimization rule.
///
/// This rule reorders operations based on cost estimates to minimize
/// total execution time. Key optimizations:
///
/// 1. **Filter reordering**: Apply more selective filters first
/// 2. **Join reordering**: Use smaller relation as hash join build side
/// 3. **Cost-based operation ordering**: Choose cheapest operation order
pub struct OperationReordering;

impl OptimizationRule for OperationReordering {
    fn name(&self) -> &str {
        "operation-reordering"
    }

    fn apply(&self, plan: &LogicalPlan, stats: &Statistics) -> Result<Option<LogicalPlan>> {
        let (new_root, changed) = self.reorder(&plan.root, stats)?;

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

impl OperationReordering {
    /// Recursively reorder operations in the plan tree.
    fn reorder(&self, op: &LogicalOp, stats: &Statistics) -> Result<(LogicalOp, bool)> {
        match op {
            // Check if this is a sequence of filters that can be reordered
            LogicalOp::Unary {
                op: UnaryOp::Filter(_),
                ..
            } => {
                // Collect all consecutive filters
                let (filters, base) = self.collect_filters(op);

                if filters.len() > 1 {
                    // Reorder filters by selectivity (most selective first = deepest)
                    let reordered = self.reorder_filters(filters, base, stats)?;
                    // Check if order actually changed
                    let changed = !self.filters_equal(op, &reordered);
                    Ok((reordered, changed))
                } else {
                    // Single filter or no filter - just recurse
                    if let LogicalOp::Unary {
                        op: filter_op,
                        input,
                    } = op
                    {
                        let (new_input, changed) = self.reorder(input, stats)?;
                        Ok((LogicalOp::unary(filter_op.clone(), new_input), changed))
                    } else {
                        Ok((op.clone(), false))
                    }
                }
            }

            // Join reordering: put smaller table on left (build side)
            LogicalOp::Binary {
                op:
                    BinaryOp::Join {
                        left_key,
                        right_key,
                    },
                left,
                right,
            } => {
                // First, optimize children
                let (opt_left, left_changed) = self.reorder(left, stats)?;
                let (opt_right, right_changed) = self.reorder(right, stats)?;

                // Estimate cardinalities
                let left_card = self.estimate_cardinality(&opt_left);
                let right_card = self.estimate_cardinality(&opt_right);

                // If right side is smaller, swap them
                let (final_left, final_right, final_left_key, final_right_key, swapped) =
                    if right_card < left_card {
                        (
                            opt_right,
                            opt_left,
                            right_key.clone(),
                            left_key.clone(),
                            true,
                        )
                    } else {
                        (
                            opt_left,
                            opt_right,
                            left_key.clone(),
                            right_key.clone(),
                            false,
                        )
                    };

                Ok((
                    LogicalOp::binary(
                        BinaryOp::Join {
                            left_key: final_left_key,
                            right_key: final_right_key,
                        },
                        final_left,
                        final_right,
                    ),
                    left_changed || right_changed || swapped,
                ))
            }

            // Other unary operations: recursively optimize
            LogicalOp::Unary { op, input } => {
                let (new_input, changed) = self.reorder(input, stats)?;
                Ok((LogicalOp::unary(op.clone(), new_input), changed))
            }

            // Binary operations (non-join): recursively optimize both sides
            LogicalOp::Binary { op, left, right } => {
                let (opt_left, left_changed) = self.reorder(left, stats)?;
                let (opt_right, right_changed) = self.reorder(right, stats)?;
                Ok((
                    LogicalOp::binary(op.clone(), opt_left, opt_right),
                    left_changed || right_changed,
                ))
            }

            // Leaf nodes: no change
            LogicalOp::Scan(_) | LogicalOp::Empty => Ok((op.clone(), false)),
        }
    }

    /// Collect all consecutive filters from a filter chain.
    /// Returns (filters, base) where filters are in top-to-bottom order.
    fn collect_filters(&self, op: &LogicalOp) -> (Vec<Predicate>, LogicalOp) {
        let mut filters = Vec::new();
        let mut current = op;

        while let LogicalOp::Unary {
            op: UnaryOp::Filter(predicate),
            input,
        } = current
        {
            filters.push(predicate.clone());
            current = input;
        }

        (filters, current.clone())
    }

    /// Reorder filters by selectivity (most selective applied first = deepest in tree).
    fn reorder_filters(
        &self,
        mut filters: Vec<Predicate>,
        base: LogicalOp,
        stats: &Statistics,
    ) -> Result<LogicalOp> {
        // Sort filters by selectivity (ascending = most selective first)
        filters.sort_by(|a, b| {
            let sel_a = self.estimate_filter_selectivity(a, stats);
            let sel_b = self.estimate_filter_selectivity(b, stats);
            sel_a
                .partial_cmp(&sel_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Build filter chain with most selective at bottom (applied first)
        let mut result = base;
        for filter in filters {
            result = LogicalOp::unary(UnaryOp::Filter(filter), result);
        }

        Ok(result)
    }

    /// Estimate filter selectivity (0.0 = filters everything, 1.0 = filters nothing).
    fn estimate_filter_selectivity(&self, predicate: &Predicate, stats: &Statistics) -> f64 {
        match predicate {
            Predicate::Eq { key, value } => {
                // Special case: Null checks are typically selective
                if matches!(value, crate::query::ir::PredicateValue::Null) {
                    return NULL_CHECK_SELECTIVITY;
                }

                // Convert value to string for selectivity estimation
                let value_str = match value {
                    crate::query::ir::PredicateValue::String(s) => s.clone(),
                    crate::query::ir::PredicateValue::Int(i) => i.to_string(),
                    crate::query::ir::PredicateValue::Float(f) => f.to_string(),
                    crate::query::ir::PredicateValue::Bool(b) => b.to_string(),
                    crate::query::ir::PredicateValue::Null => unreachable!(), // Already handled above
                };
                stats.estimate_selectivity(key, &value_str)
            }
            Predicate::Gt { .. }
            | Predicate::Lt { .. }
            | Predicate::Gte { .. }
            | Predicate::Lte { .. } => RANGE_PREDICATE_SELECTIVITY,
            Predicate::Contains { .. }
            | Predicate::StartsWith { .. }
            | Predicate::EndsWith { .. } => STRING_PREDICATE_SELECTIVITY,
            Predicate::And(predicates) => {
                // For AND: multiply selectivities (intersection rule)
                // Empty AND defaults to high selectivity (filters nothing)
                if predicates.is_empty() {
                    return TRUE_SELECTIVITY;
                }
                predicates
                    .iter()
                    .map(|p| self.estimate_filter_selectivity(p, stats))
                    .product()
            }
            Predicate::Or(predicates) => {
                // For OR: use union rule: 1 - (1-sel1) * (1-sel2) * ...
                // Empty OR defaults to low selectivity (filters everything)
                if predicates.is_empty() {
                    return FALSE_SELECTIVITY;
                }
                let complement_product: f64 = predicates
                    .iter()
                    .map(|p| 1.0 - self.estimate_filter_selectivity(p, stats))
                    .product();
                1.0 - complement_product
            }
            Predicate::Not(inner) => {
                // For NOT: complement of inner selectivity
                1.0 - self.estimate_filter_selectivity(inner, stats)
            }
            Predicate::Ne { .. } => NOT_EQUALS_SELECTIVITY,
            Predicate::In { .. } => IN_PREDICATE_SELECTIVITY,
            Predicate::Exists(_) | Predicate::NotExists(_) => EXISTENCE_CHECK_SELECTIVITY,
            Predicate::True => TRUE_SELECTIVITY,
            Predicate::False => FALSE_SELECTIVITY,
        }
    }

    /// Estimate cardinality of a logical operation's output.
    fn estimate_cardinality(&self, op: &LogicalOp) -> usize {
        match op {
            LogicalOp::Scan(scan) => match scan {
                ScanOp::NodeLookup(ids) => ids.len(),
                ScanOp::NodeScan { estimated_rows, .. } => estimated_rows.unwrap_or(1000),
                ScanOp::VectorSearch { k, .. } => *k,
                ScanOp::TemporalNodeLookup { node_ids, .. } => node_ids.len(),
                ScanOp::TemporalVectorSearch { k, .. } => *k,
                ScanOp::SimilarToNode { k, .. } => *k,
            },
            LogicalOp::Unary {
                op: UnaryOp::Filter(_),
                input,
            } => {
                // Assume 10% selectivity by default
                (self.estimate_cardinality(input) as f64 * 0.1) as usize
            }
            LogicalOp::Unary {
                op: UnaryOp::Limit(n),
                ..
            } => *n,
            LogicalOp::Unary { input, .. } => self.estimate_cardinality(input),
            LogicalOp::Binary { left, right, .. } => {
                // For joins, assume 10% of cross product
                let left_card = self.estimate_cardinality(left);
                let right_card = self.estimate_cardinality(right);
                (left_card as f64 * right_card as f64 * 0.1) as usize
            }
            LogicalOp::Empty => 0,
        }
    }

    /// Check if two filter chains are equal (same filters in same order).
    fn filters_equal(&self, a: &LogicalOp, b: &LogicalOp) -> bool {
        match (a, b) {
            (
                LogicalOp::Unary {
                    op: UnaryOp::Filter(pred_a),
                    input: input_a,
                },
                LogicalOp::Unary {
                    op: UnaryOp::Filter(pred_b),
                    input: input_b,
                },
            ) => {
                // Check if predicates are equal (simple comparison)
                self.predicates_equal(pred_a, pred_b) && self.filters_equal(input_a, input_b)
            }
            // If not both filters, just check if they're the same type
            _ => std::mem::discriminant(a) == std::mem::discriminant(b),
        }
    }

    /// Structural predicate equality check.
    ///
    /// Performs deep structural comparison of predicates to determine if they
    /// are semantically equivalent. This is used to detect if filter reordering
    /// actually changed the plan structure.
    fn predicates_equal(&self, a: &Predicate, b: &Predicate) -> bool {
        match (a, b) {
            (Predicate::Eq { key: k1, value: v1 }, Predicate::Eq { key: k2, value: v2 }) => {
                k1 == k2 && v1 == v2
            }
            (Predicate::Ne { key: k1, value: v1 }, Predicate::Ne { key: k2, value: v2 }) => {
                k1 == k2 && v1 == v2
            }
            (Predicate::Gt { key: k1, value: v1 }, Predicate::Gt { key: k2, value: v2 }) => {
                k1 == k2 && v1 == v2
            }
            (Predicate::Gte { key: k1, value: v1 }, Predicate::Gte { key: k2, value: v2 }) => {
                k1 == k2 && v1 == v2
            }
            (Predicate::Lt { key: k1, value: v1 }, Predicate::Lt { key: k2, value: v2 }) => {
                k1 == k2 && v1 == v2
            }
            (Predicate::Lte { key: k1, value: v1 }, Predicate::Lte { key: k2, value: v2 }) => {
                k1 == k2 && v1 == v2
            }
            (
                Predicate::In {
                    key: k1,
                    values: vs1,
                },
                Predicate::In {
                    key: k2,
                    values: vs2,
                },
            ) => k1 == k2 && vs1 == vs2,
            (
                Predicate::Contains {
                    key: k1,
                    substring: s1,
                },
                Predicate::Contains {
                    key: k2,
                    substring: s2,
                },
            ) => k1 == k2 && s1 == s2,
            (
                Predicate::StartsWith {
                    key: k1,
                    prefix: p1,
                },
                Predicate::StartsWith {
                    key: k2,
                    prefix: p2,
                },
            ) => k1 == k2 && p1 == p2,
            (
                Predicate::EndsWith {
                    key: k1,
                    suffix: s1,
                },
                Predicate::EndsWith {
                    key: k2,
                    suffix: s2,
                },
            ) => k1 == k2 && s1 == s2,
            (Predicate::Exists(k1), Predicate::Exists(k2)) => k1 == k2,
            (Predicate::NotExists(k1), Predicate::NotExists(k2)) => k1 == k2,
            (Predicate::And(v1), Predicate::And(v2)) | (Predicate::Or(v1), Predicate::Or(v2)) => {
                v1.len() == v2.len()
                    && v1
                        .iter()
                        .zip(v2.iter())
                        .all(|(p1, p2)| self.predicates_equal(p1, p2))
            }
            (Predicate::Not(p1), Predicate::Not(p2)) => self.predicates_equal(p1, p2),
            (Predicate::True, Predicate::True) => true,
            (Predicate::False, Predicate::False) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::NodeId;
    use crate::query::ir::Predicate;
    use crate::query::plan::{BinaryOp, ScanOp, UnaryOp};

    fn test_stats() -> Statistics {
        let stats = Statistics::default();
        // Set up some statistics for testing
        stats.refresh(1000, 5000, 100, vec![], 5.0);

        // Set up property statistics for selectivity estimation
        stats.update_property_stats("rare_property", 10); // Very selective (1/10)
        stats.update_property_stats("common_property", 500); // Less selective (1/500)

        stats
    }

    // ==================== Filter Reordering Tests ====================

    #[test]
    fn test_reorder_filters_by_selectivity() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Original: Filter(common) -> Filter(rare) -> Scan
        // common_property: 500 distinct values → 0.2% selectivity (MORE selective)
        // rare_property: 10 distinct values → 10% selectivity (LESS selective)
        // Should be reordered to: Filter(rare) -> Filter(common) -> Scan
        // (most selective at bottom/applied first)
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("common_property", "value")),
            LogicalOp::unary(
                UnaryOp::Filter(Predicate::eq("rare_property", "value")),
                LogicalOp::Scan(ScanOp::NodeScan {
                    label: None,
                    estimated_rows: Some(1000),
                }),
            ),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some(), "Should reorder filters by selectivity");

        let optimized = result.unwrap();
        // The outermost filter should be the LESS selective one (rare_property)
        // The innermost/deepest filter should be the MORE selective one (common_property)
        match &optimized.root {
            LogicalOp::Unary {
                op: UnaryOp::Filter(predicate),
                input,
            } => {
                // Root filter should be rare (less selective, applied last)
                assert!(matches!(
                    predicate,
                    Predicate::Eq { key, .. } if key == "rare_property"
                ));

                // Inner filter should be common (more selective, applied first)
                match input.as_ref() {
                    LogicalOp::Unary {
                        op: UnaryOp::Filter(inner_pred),
                        ..
                    } => {
                        assert!(matches!(
                            inner_pred,
                            Predicate::Eq { key, .. } if key == "common_property"
                        ));
                    }
                    _ => panic!("Expected inner filter"),
                }
            }
            _ => panic!("Expected Filter at root"),
        }
    }

    #[test]
    fn test_no_reorder_when_filters_already_optimal() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Filter(rare - less selective) -> Filter(common - more selective) -> Scan
        // Already optimal: most selective at bottom
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("rare_property", "value")),
            LogicalOp::unary(
                UnaryOp::Filter(Predicate::eq("common_property", "value")),
                LogicalOp::Scan(ScanOp::NodeScan {
                    label: None,
                    estimated_rows: Some(1000),
                }),
            ),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        // Should return None since already optimal
        assert!(
            result.is_none(),
            "Should not reorder already optimal filters"
        );
    }

    #[test]
    fn test_reorder_three_filters() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Add a third property with medium selectivity
        stats.update_property_stats("medium_property", 100); // Medium (1/100)

        // Filter(common) -> Filter(medium) -> Filter(rare) -> Scan
        // Should become: Filter(rare) -> Filter(medium) -> Filter(common) -> Scan
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("common_property", "value")),
            LogicalOp::unary(
                UnaryOp::Filter(Predicate::eq("medium_property", "value")),
                LogicalOp::unary(
                    UnaryOp::Filter(Predicate::eq("rare_property", "value")),
                    LogicalOp::Scan(ScanOp::NodeScan {
                        label: None,
                        estimated_rows: Some(1000),
                    }),
                ),
            ),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some(), "Should reorder three filters");
    }

    // ==================== Join Reordering Tests ====================

    #[test]
    fn test_reorder_join_operands_by_size() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Join with large left side and small right side
        // Should be reordered to put small side first (for hash join build)
        let large_scan = LogicalOp::Scan(ScanOp::NodeScan {
            label: Some("LargeTable".to_string()),
            estimated_rows: Some(10000),
        });

        let small_scan = LogicalOp::Scan(ScanOp::NodeScan {
            label: Some("SmallTable".to_string()),
            estimated_rows: Some(100),
        });

        let plan = LogicalPlan::new(LogicalOp::binary(
            BinaryOp::Join {
                left_key: "id".to_string(),
                right_key: "ref_id".to_string(),
            },
            large_scan,
            small_scan,
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some(), "Should reorder join operands");

        let optimized = result.unwrap();
        // Small table should be on the left (build side)
        match &optimized.root {
            LogicalOp::Binary {
                op: BinaryOp::Join { .. },
                left,
                ..
            } => {
                if let LogicalOp::Scan(ScanOp::NodeScan {
                    estimated_rows: Some(rows),
                    ..
                }) = left.as_ref()
                {
                    assert_eq!(*rows, 100, "Smaller table should be build side");
                } else {
                    panic!("Expected NodeScan on left");
                }
            }
            _ => panic!("Expected Join at root"),
        }
    }

    #[test]
    fn test_no_reorder_when_join_already_optimal() {
        let rule = OperationReordering;
        let stats = test_stats();

        let small_scan = LogicalOp::Scan(ScanOp::NodeScan {
            label: Some("SmallTable".to_string()),
            estimated_rows: Some(100),
        });

        let large_scan = LogicalOp::Scan(ScanOp::NodeScan {
            label: Some("LargeTable".to_string()),
            estimated_rows: Some(10000),
        });

        // Small already on left - optimal
        let plan = LogicalPlan::new(LogicalOp::binary(
            BinaryOp::Join {
                left_key: "id".to_string(),
                right_key: "ref_id".to_string(),
            },
            small_scan,
            large_scan,
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none(), "Should not reorder optimal join");
    }

    // ==================== Complex Query Reordering Tests ====================

    #[test]
    fn test_reorder_complex_query() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Complex query with multiple opportunities for reordering:
        // Join(large, small) with filters in non-optimal order
        let plan = LogicalPlan::new(LogicalOp::binary(
            BinaryOp::Join {
                left_key: "id".to_string(),
                right_key: "ref_id".to_string(),
            },
            // Left side: large scan with non-optimal filter order
            LogicalOp::unary(
                UnaryOp::Filter(Predicate::eq("common_property", "value")),
                LogicalOp::Scan(ScanOp::NodeScan {
                    label: Some("LargeTable".to_string()),
                    estimated_rows: Some(10000),
                }),
            ),
            // Right side: small scan
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("SmallTable".to_string()),
                estimated_rows: Some(100),
            }),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some(), "Should optimize complex query");
    }

    // ==================== Edge Cases ====================

    #[test]
    fn test_no_change_for_simple_scan() {
        let rule = OperationReordering;
        let stats = test_stats();

        let plan = LogicalPlan::new(LogicalOp::Scan(ScanOp::NodeLookup(vec![
            NodeId::new(1).unwrap(),
        ])));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none(), "Should not change simple scan");
    }

    #[test]
    fn test_single_filter_no_reorder() {
        let rule = OperationReordering;
        let stats = test_stats();

        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: None,
                estimated_rows: Some(1000),
            }),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none(), "Single filter cannot be reordered");
    }
}
