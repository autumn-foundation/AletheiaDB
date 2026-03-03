//! Operation Reordering Optimization
//!
//! Reorders operations based on estimated costs and selectivity to minimize
//! overall query execution cost.
//!
//! # Optimization Strategy
//!
//! This rule applies three main optimizations:
//!
//! 1. **Filter Reordering**: Push more selective filters deeper into the plan (closer to the data source).
//!    This reduces the number of rows that subsequent operations need to process.
//! 2. **Join Reordering**: Ensure the smaller relation is on the left side of a hash join (build side).
//!    This minimizes the memory footprint of the hash table.
//! 3. **Cost-Based Ordering**: Use cardinality estimates to choose the cheapest operation sequence.
//!
//! # Example: Join Optimization
//!
//! ```text
//! BEFORE: Large Table (Left) JOIN Small Table (Right)
//!         (Requires building hash table on Large Table)
//!
//!         Join
//!        /    \
//!   Large      Small
//!
//! AFTER:  Small Table (Left) JOIN Large Table (Right)
//!         (Builds hash table on Small Table - faster & less memory)
//!
//!         Join
//!        /    \
//!   Small      Large
//! ```

use crate::core::error::Result;
use crate::query::ir::Predicate;
use crate::query::plan::{BinaryOp, LogicalOp, LogicalPlan, ScanOp, UnaryOp};

use super::{OptimizationRule, Statistics};

// Selectivity estimates for different predicate types
// (0.0 = filters everything, 1.0 = filters nothing)
/// Selectivity for `IS NULL` checks (0.1). Assumes nulls are relatively rare.
const NULL_CHECK_SELECTIVITY: f64 = 0.1;
/// Selectivity for existence checks (0.1). Assumes checking for a specific property filters well.
const EXISTENCE_CHECK_SELECTIVITY: f64 = 0.1;
/// Selectivity for `IN` predicates (0.15).
const IN_PREDICATE_SELECTIVITY: f64 = 0.15;
/// Selectivity for string matching (Contains/StartsWith/EndsWith) (0.2).
const STRING_PREDICATE_SELECTIVITY: f64 = 0.2;
/// Selectivity for range predicates (Gt/Lt/Gte/Lte) (0.3).
const RANGE_PREDICATE_SELECTIVITY: f64 = 0.3;
/// Selectivity for not-equals (!=) (0.9). These typically filter very little.
const NOT_EQUALS_SELECTIVITY: f64 = 0.9;
/// Selectivity for `TRUE` (1.0). Filters nothing.
const TRUE_SELECTIVITY: f64 = 1.0;
/// Selectivity for `FALSE` (0.0). Filters everything.
const FALSE_SELECTIVITY: f64 = 0.0;
// Note: AND/OR/NOT selectivity is computed dynamically based on child predicates

/// Operation reordering optimization rule.
///
/// This rule reorders operations based on cost estimates to minimize
/// total execution time.
///
/// # Examples
///
/// ```text
/// Input Plan:
/// Filter(B) -> Filter(A) -> Scan
/// (Where Filter A is very selective, and Filter B is not)
///
/// Optimized Plan:
/// Filter(A) -> Filter(B) -> Scan
/// (Filter A is applied first, reducing rows for Filter B)
/// ```
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
    ///
    /// # Heuristics
    ///
    /// - **Equality**: Uses statistical histograms if available.
    /// - **Range**: Assumes 30% selectivity (`RANGE_PREDICATE_SELECTIVITY`).
    /// - **String**: Assumes 20% selectivity (`STRING_PREDICATE_SELECTIVITY`).
    /// - **Logic**:
    ///   - `AND`: Product of probabilities (intersection).
    ///   - `OR`: 1 - Product of complement probabilities (union).
    ///   - `NOT`: 1 - Probability (complement).
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
                ScanOp::PropertyScan { .. } => 100, // ~10% selectivity estimate
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

    #[test]
    fn test_selectivity_and_predicate() {
        let rule = OperationReordering;
        let stats = test_stats();

        // AND of two predicates: selectivity should be product
        // Use predicates with fixed selectivity (not dependent on property stats)
        let pred = Predicate::And(vec![
            Predicate::gt("score", 50i64), // RANGE_PREDICATE_SELECTIVITY = 0.3
            Predicate::contains("name", "test"), // STRING_PREDICATE_SELECTIVITY = 0.2
        ]);

        let sel = rule.estimate_filter_selectivity(&pred, &stats);
        // Expected: 0.3 * 0.2 = 0.06
        assert!(
            (sel - 0.06).abs() < 0.001,
            "AND selectivity should be product, got {}",
            sel
        );
    }

    #[test]
    fn test_selectivity_or_predicate() {
        let rule = OperationReordering;
        let stats = test_stats();

        // OR of two predicates: selectivity = 1 - (1-sel1) * (1-sel2)
        // Use predicates with fixed selectivity
        let pred = Predicate::Or(vec![
            Predicate::gt("score", 50i64), // RANGE_PREDICATE_SELECTIVITY = 0.3
            Predicate::contains("name", "test"), // STRING_PREDICATE_SELECTIVITY = 0.2
        ]);

        let sel = rule.estimate_filter_selectivity(&pred, &stats);
        // Expected: 1 - (1-0.3) * (1-0.2) = 1 - 0.7 * 0.8 = 1 - 0.56 = 0.44
        assert!(
            (sel - 0.44).abs() < 0.001,
            "OR selectivity should use union rule, got {}",
            sel
        );
    }

    #[test]
    fn test_selectivity_not_predicate() {
        let rule = OperationReordering;
        let stats = test_stats();

        // NOT of a predicate: selectivity = 1 - inner_sel
        // Use predicate with fixed selectivity
        let pred = Predicate::Not(Box::new(Predicate::gt("score", 50i64))); // RANGE_PREDICATE_SELECTIVITY = 0.3

        let sel = rule.estimate_filter_selectivity(&pred, &stats);
        // Expected: 1 - 0.3 = 0.7
        assert!(
            (sel - 0.7).abs() < 0.001,
            "NOT selectivity should be complement, got {}",
            sel
        );
    }

    #[test]
    fn test_selectivity_empty_and() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Empty AND should have high selectivity (filters nothing)
        let pred = Predicate::And(vec![]);
        let sel = rule.estimate_filter_selectivity(&pred, &stats);
        assert_eq!(sel, TRUE_SELECTIVITY);
    }

    #[test]
    fn test_selectivity_empty_or() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Empty OR should have low selectivity (filters everything)
        let pred = Predicate::Or(vec![]);
        let sel = rule.estimate_filter_selectivity(&pred, &stats);
        assert_eq!(sel, FALSE_SELECTIVITY);
    }

    #[test]
    fn test_selectivity_nested_predicates() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Nested: AND(OR(a, b), c)
        // Use predicates with fixed selectivity
        let pred = Predicate::And(vec![
            Predicate::Or(vec![
                Predicate::gt("score", 50i64),       // RANGE = 0.3
                Predicate::contains("name", "test"), // STRING = 0.2
            ]),
            Predicate::lt("age", 100i64), // RANGE_PREDICATE_SELECTIVITY = 0.3
        ]);

        let sel = rule.estimate_filter_selectivity(&pred, &stats);
        // OR: 1 - (1-0.3) * (1-0.2) = 1 - 0.7 * 0.8 = 0.44
        // AND: 0.44 * 0.3 = 0.132
        assert!(
            (sel - 0.132).abs() < 0.001,
            "Nested predicates should compute correctly, got {}",
            sel
        );
    }

    #[test]
    fn test_selectivity_all_types() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Test all predicate types have defined selectivity
        assert_eq!(
            rule.estimate_filter_selectivity(&Predicate::True, &stats),
            TRUE_SELECTIVITY
        );
        assert_eq!(
            rule.estimate_filter_selectivity(&Predicate::False, &stats),
            FALSE_SELECTIVITY
        );
        assert_eq!(
            rule.estimate_filter_selectivity(&Predicate::gt("x", 1i64), &stats),
            RANGE_PREDICATE_SELECTIVITY
        );
        assert_eq!(
            rule.estimate_filter_selectivity(&Predicate::lt("x", 1i64), &stats),
            RANGE_PREDICATE_SELECTIVITY
        );
        assert_eq!(
            rule.estimate_filter_selectivity(&Predicate::contains("x", "y"), &stats),
            STRING_PREDICATE_SELECTIVITY
        );
        assert_eq!(
            rule.estimate_filter_selectivity(
                &Predicate::StartsWith {
                    key: "x".to_string(),
                    prefix: "y".to_string()
                },
                &stats
            ),
            STRING_PREDICATE_SELECTIVITY
        );
        assert_eq!(
            rule.estimate_filter_selectivity(
                &Predicate::EndsWith {
                    key: "x".to_string(),
                    suffix: "y".to_string()
                },
                &stats
            ),
            STRING_PREDICATE_SELECTIVITY
        );
        assert_eq!(
            rule.estimate_filter_selectivity(&Predicate::ne("x", 1i64), &stats),
            NOT_EQUALS_SELECTIVITY
        );
        assert_eq!(
            rule.estimate_filter_selectivity(
                &Predicate::In {
                    key: "x".to_string(),
                    values: vec![
                        crate::query::ir::PredicateValue::Int(1),
                        crate::query::ir::PredicateValue::Int(2)
                    ]
                },
                &stats
            ),
            IN_PREDICATE_SELECTIVITY
        );
        assert_eq!(
            rule.estimate_filter_selectivity(&Predicate::exists("x"), &stats),
            EXISTENCE_CHECK_SELECTIVITY
        );
        assert_eq!(
            rule.estimate_filter_selectivity(&Predicate::NotExists("x".to_string()), &stats),
            EXISTENCE_CHECK_SELECTIVITY
        );
    }

    #[test]
    fn test_predicates_equal_basic() {
        let rule = OperationReordering;

        // Same predicates should be equal
        let p1 = Predicate::eq("name", "Alice");
        let p2 = Predicate::eq("name", "Alice");
        assert!(rule.predicates_equal(&p1, &p2));

        // Different values should not be equal
        let p3 = Predicate::eq("name", "Bob");
        assert!(!rule.predicates_equal(&p1, &p3));

        // Different keys should not be equal
        let p4 = Predicate::eq("age", "Alice");
        assert!(!rule.predicates_equal(&p1, &p4));
    }

    #[test]
    fn test_predicates_equal_different_types() {
        let rule = OperationReordering;

        // Different predicate types should not be equal
        let p1 = Predicate::eq("name", "Alice");
        let p2 = Predicate::ne("name", "Alice");
        assert!(!rule.predicates_equal(&p1, &p2));

        let p3 = Predicate::gt("age", 30i64);
        let p4 = Predicate::lt("age", 30i64);
        assert!(!rule.predicates_equal(&p3, &p4));
    }

    #[test]
    fn test_predicates_equal_and_or() {
        let rule = OperationReordering;

        // Same AND predicates
        let p1 = Predicate::And(vec![
            Predicate::eq("name", "Alice"),
            Predicate::gt("age", 30i64),
        ]);
        let p2 = Predicate::And(vec![
            Predicate::eq("name", "Alice"),
            Predicate::gt("age", 30i64),
        ]);
        assert!(rule.predicates_equal(&p1, &p2));

        // Different order in AND should not be equal (structural comparison)
        let p3 = Predicate::And(vec![
            Predicate::gt("age", 30i64),
            Predicate::eq("name", "Alice"),
        ]);
        assert!(!rule.predicates_equal(&p1, &p3));

        // Different length AND
        let p4 = Predicate::And(vec![Predicate::eq("name", "Alice")]);
        assert!(!rule.predicates_equal(&p1, &p4));

        // AND vs OR
        let p5 = Predicate::Or(vec![
            Predicate::eq("name", "Alice"),
            Predicate::gt("age", 30i64),
        ]);
        assert!(!rule.predicates_equal(&p1, &p5));
    }

    #[test]
    fn test_predicates_equal_not() {
        let rule = OperationReordering;

        // Same NOT predicates
        let p1 = Predicate::Not(Box::new(Predicate::eq("active", true)));
        let p2 = Predicate::Not(Box::new(Predicate::eq("active", true)));
        assert!(rule.predicates_equal(&p1, &p2));

        // Different inner predicates
        let p3 = Predicate::Not(Box::new(Predicate::eq("active", false)));
        assert!(!rule.predicates_equal(&p1, &p3));
    }

    #[test]
    fn test_predicates_equal_all_variants() {
        let rule = OperationReordering;

        // Test all variants for coverage
        assert!(rule.predicates_equal(&Predicate::True, &Predicate::True));
        assert!(rule.predicates_equal(&Predicate::False, &Predicate::False));
        assert!(!rule.predicates_equal(&Predicate::True, &Predicate::False));

        // String predicates
        let c1 = Predicate::contains("text", "hello");
        let c2 = Predicate::contains("text", "hello");
        assert!(rule.predicates_equal(&c1, &c2));

        let s1 = Predicate::StartsWith {
            key: "text".to_string(),
            prefix: "hello".to_string(),
        };
        let s2 = Predicate::StartsWith {
            key: "text".to_string(),
            prefix: "hello".to_string(),
        };
        assert!(rule.predicates_equal(&s1, &s2));

        let e1 = Predicate::EndsWith {
            key: "text".to_string(),
            suffix: "world".to_string(),
        };
        let e2 = Predicate::EndsWith {
            key: "text".to_string(),
            suffix: "world".to_string(),
        };
        assert!(rule.predicates_equal(&e1, &e2));

        // In predicate
        let i1 = Predicate::In {
            key: "id".to_string(),
            values: vec![
                crate::query::ir::PredicateValue::Int(1),
                crate::query::ir::PredicateValue::Int(2),
            ],
        };
        let i2 = Predicate::In {
            key: "id".to_string(),
            values: vec![
                crate::query::ir::PredicateValue::Int(1),
                crate::query::ir::PredicateValue::Int(2),
            ],
        };
        assert!(rule.predicates_equal(&i1, &i2));

        // Exists predicates
        let ex1 = Predicate::exists("prop");
        let ex2 = Predicate::exists("prop");
        assert!(rule.predicates_equal(&ex1, &ex2));

        let nex1 = Predicate::NotExists("prop".to_string());
        let nex2 = Predicate::NotExists("prop".to_string());
        assert!(rule.predicates_equal(&nex1, &nex2));
    }

    #[test]
    fn test_selectivity_null_check() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Null checks should use NULL_CHECK_SELECTIVITY
        let pred = Predicate::Eq {
            key: "value".to_string(),
            value: crate::query::ir::PredicateValue::Null,
        };

        let sel = rule.estimate_filter_selectivity(&pred, &stats);
        assert_eq!(sel, NULL_CHECK_SELECTIVITY);
    }
}

#[cfg(test)]
mod sentry_operation_reordering_tests {
    use super::*;
    use crate::core::NodeId;
    use crate::query::ir::{Predicate, PredicateValue};
    use crate::query::plan::{BinaryOp, LogicalOp, ScanOp, UnaryOp};

    fn test_stats() -> Statistics {
        Statistics::default()
    }

    #[test]
    fn test_sentry_filters_equal_logic() {
        // 🛡️ Sentry Test: Verify strict matching in `filters_equal`
        // Targets mutants changing `&&` to `||` when combining predicates_equal and filters_equal.
        let rule = OperationReordering;

        let base_scan = LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()]));
        let base_empty = LogicalOp::Empty;

        let filter_a = LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            base_scan.clone(),
        );

        // Same predicate, different input type (Scan vs Empty) so discriminant differs
        let filter_b = LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            base_empty.clone(),
        );

        // Different predicate, same input
        let filter_c = LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Bob")),
            base_scan.clone(),
        );

        // Same predicate, same input
        let filter_d = LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            base_scan.clone(),
        );

        assert!(
            !rule.filters_equal(&filter_a, &filter_b),
            "Should be false when inputs differ in discriminant"
        );
        assert!(
            !rule.filters_equal(&filter_a, &filter_c),
            "Should be false when predicates differ"
        );
        assert!(
            rule.filters_equal(&filter_a, &filter_d),
            "Should be true when both match"
        );
    }

    #[test]
    fn test_sentry_estimate_cardinality_binary_op_formula() {
        // 🛡️ Sentry Test: Verify exact cardinality estimation formula for BinaryOp (Join)
        // Targets mutants changing * to / or + or omitting the 0.1 factor.
        let rule = OperationReordering;

        // Left card = 1000
        let left = LogicalOp::Scan(ScanOp::NodeScan {
            label: None,
            estimated_rows: Some(1000),
        });

        // Right card = 200
        let right = LogicalOp::Scan(ScanOp::NodeScan {
            label: None,
            estimated_rows: Some(200),
        });

        let binary_op = LogicalOp::binary(
            BinaryOp::Join {
                left_key: "k1".to_string(),
                right_key: "k2".to_string(),
            },
            left,
            right,
        );

        // Formula: (left_card * right_card * 0.1) as usize
        // (1000 * 200 * 0.1) = 200000 * 0.1 = 20000
        assert_eq!(rule.estimate_cardinality(&binary_op), 20000);
    }

    #[test]
    fn test_sentry_predicates_equal_variants() {
        // 🛡️ Sentry Test: Verify all Predicate variants are correctly matched in predicates_equal.
        // Targets mutants that delete match arms or replace `&&` with `||` / `==` with `!=` in the comparisons.
        let rule = OperationReordering;

        let eq1 = Predicate::Eq {
            key: "k".to_string(),
            value: PredicateValue::Int(1),
        };
        let eq2 = Predicate::Eq {
            key: "k".to_string(),
            value: PredicateValue::Int(2),
        };
        assert!(rule.predicates_equal(&eq1, &eq1));
        assert!(!rule.predicates_equal(&eq1, &eq2));

        let ne1 = Predicate::Ne {
            key: "k".to_string(),
            value: PredicateValue::Int(1),
        };
        let ne2 = Predicate::Ne {
            key: "k".to_string(),
            value: PredicateValue::Int(2),
        };
        assert!(rule.predicates_equal(&ne1, &ne1));
        assert!(!rule.predicates_equal(&ne1, &ne2));

        let gt1 = Predicate::Gt {
            key: "k".to_string(),
            value: PredicateValue::Int(1),
        };
        let gt2 = Predicate::Gt {
            key: "k".to_string(),
            value: PredicateValue::Int(2),
        };
        assert!(rule.predicates_equal(&gt1, &gt1));
        assert!(!rule.predicates_equal(&gt1, &gt2));

        let gte1 = Predicate::Gte {
            key: "k".to_string(),
            value: PredicateValue::Int(1),
        };
        let gte2 = Predicate::Gte {
            key: "k".to_string(),
            value: PredicateValue::Int(2),
        };
        assert!(rule.predicates_equal(&gte1, &gte1));
        assert!(!rule.predicates_equal(&gte1, &gte2));

        let lt1 = Predicate::Lt {
            key: "k".to_string(),
            value: PredicateValue::Int(1),
        };
        let lt2 = Predicate::Lt {
            key: "k".to_string(),
            value: PredicateValue::Int(2),
        };
        assert!(rule.predicates_equal(&lt1, &lt1));
        assert!(!rule.predicates_equal(&lt1, &lt2));

        let lte1 = Predicate::Lte {
            key: "k".to_string(),
            value: PredicateValue::Int(1),
        };
        let lte2 = Predicate::Lte {
            key: "k".to_string(),
            value: PredicateValue::Int(2),
        };
        assert!(rule.predicates_equal(&lte1, &lte1));
        assert!(!rule.predicates_equal(&lte1, &lte2));

        let in1 = Predicate::In {
            key: "k".to_string(),
            values: vec![PredicateValue::Int(1)],
        };
        let in2 = Predicate::In {
            key: "k".to_string(),
            values: vec![PredicateValue::Int(2)],
        };
        assert!(rule.predicates_equal(&in1, &in1));
        assert!(!rule.predicates_equal(&in1, &in2));

        let c1 = Predicate::Contains {
            key: "k".to_string(),
            substring: "a".to_string(),
        };
        let c2 = Predicate::Contains {
            key: "k".to_string(),
            substring: "b".to_string(),
        };
        assert!(rule.predicates_equal(&c1, &c1));
        assert!(!rule.predicates_equal(&c1, &c2));

        let s1 = Predicate::StartsWith {
            key: "k".to_string(),
            prefix: "a".to_string(),
        };
        let s2 = Predicate::StartsWith {
            key: "k".to_string(),
            prefix: "b".to_string(),
        };
        assert!(rule.predicates_equal(&s1, &s1));
        assert!(!rule.predicates_equal(&s1, &s2));

        let e1 = Predicate::EndsWith {
            key: "k".to_string(),
            suffix: "a".to_string(),
        };
        let e2 = Predicate::EndsWith {
            key: "k".to_string(),
            suffix: "b".to_string(),
        };
        assert!(rule.predicates_equal(&e1, &e1));
        assert!(!rule.predicates_equal(&e1, &e2));

        let ex1 = Predicate::Exists("k1".to_string());
        let ex2 = Predicate::Exists("k2".to_string());
        assert!(rule.predicates_equal(&ex1, &ex1));
        assert!(!rule.predicates_equal(&ex1, &ex2));

        let nex1 = Predicate::NotExists("k1".to_string());
        let nex2 = Predicate::NotExists("k2".to_string());
        assert!(rule.predicates_equal(&nex1, &nex1));
        assert!(!rule.predicates_equal(&nex1, &nex2));

        let and1 = Predicate::And(vec![Predicate::True, Predicate::False]);
        let and2 = Predicate::And(vec![Predicate::True, Predicate::True]);
        assert!(rule.predicates_equal(&and1, &and1));
        assert!(!rule.predicates_equal(&and1, &and2));

        let or1 = Predicate::Or(vec![Predicate::True, Predicate::False]);
        let or2 = Predicate::Or(vec![Predicate::True, Predicate::True]);
        assert!(rule.predicates_equal(&or1, &or1));
        assert!(!rule.predicates_equal(&or1, &or2));

        let not1 = Predicate::Not(Box::new(Predicate::True));
        let not2 = Predicate::Not(Box::new(Predicate::False));
        assert!(rule.predicates_equal(&not1, &not1));
        assert!(!rule.predicates_equal(&not1, &not2));
    }

    #[test]
    fn test_sentry_reorder_filters_exact_selectivity() {
        // 🛡️ Sentry Test: Verify exact selectivity values inside `estimate_filter_selectivity`
        let rule = OperationReordering;
        let stats = test_stats();

        let p_gt = Predicate::Gt {
            key: "k".to_string(),
            value: PredicateValue::Int(1),
        };
        assert_eq!(
            rule.estimate_filter_selectivity(&p_gt, &stats),
            RANGE_PREDICATE_SELECTIVITY
        );

        let p_contains = Predicate::Contains {
            key: "k".to_string(),
            substring: "a".to_string(),
        };
        assert_eq!(
            rule.estimate_filter_selectivity(&p_contains, &stats),
            STRING_PREDICATE_SELECTIVITY
        );

        let p_ne = Predicate::Ne {
            key: "k".to_string(),
            value: PredicateValue::Int(1),
        };
        assert_eq!(
            rule.estimate_filter_selectivity(&p_ne, &stats),
            NOT_EQUALS_SELECTIVITY
        );

        let p_in = Predicate::In {
            key: "k".to_string(),
            values: vec![],
        };
        assert_eq!(
            rule.estimate_filter_selectivity(&p_in, &stats),
            IN_PREDICATE_SELECTIVITY
        );

        let p_exists = Predicate::Exists("k".to_string());
        assert_eq!(
            rule.estimate_filter_selectivity(&p_exists, &stats),
            EXISTENCE_CHECK_SELECTIVITY
        );
    }
}
