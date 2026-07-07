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

/// Default cardinality estimate for node scans without statistics.
const DEFAULT_NODE_SCAN_CARDINALITY: usize = 1000;

/// Default cardinality estimate for edge scans without statistics.
const DEFAULT_EDGE_SCAN_CARDINALITY: usize = 1000;

/// Default cardinality estimate for property-indexed scans (~10% of full scan).
const DEFAULT_PROPERTY_SCAN_CARDINALITY: usize = 100;

/// Default selectivity estimate for filter predicates (10%).
const DEFAULT_FILTER_SELECTIVITY: f64 = 0.1;

/// Default selectivity for join operations (10% of cross product).
const DEFAULT_JOIN_SELECTIVITY: f64 = 0.1;

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
///
/// ## Examples
///
/// ```rust
/// use aletheiadb::query::planner::rules::{OptimizationRule, OperationReordering};
/// use aletheiadb::query::planner::stats::Statistics;
/// use aletheiadb::query::plan::{LogicalPlan, LogicalOp, UnaryOp, ScanOp};
/// use aletheiadb::query::ir::{Predicate, PredicateValue};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // 1. Construct a sub-optimal plan: A very selective filter (Eq) is ABOVE a less selective one (NotEq)
/// let scan = LogicalOp::Scan(ScanOp::NodeScan {
///     label: Some("Person".into()),
///     estimated_rows: Some(100),
/// });
/// let filter_bad = LogicalOp::unary(
///     UnaryOp::Filter(Predicate::Ne {
///         key: "status".into(),
///         value: PredicateValue::String("deleted".into())
///     }),
///     scan
/// );
/// let filter_good = LogicalOp::unary(
///     UnaryOp::Filter(Predicate::Eq {
///         key: "id".into(),
///         value: PredicateValue::Int(42)
///     }),
///     filter_bad
/// );
///
/// let plan = LogicalPlan { root: filter_good, temporal_context: None, hints: Default::default() };
///
/// // 2. Apply the rule
/// let rule = OperationReordering;
/// let stats = Statistics::new();
/// let optimized_plan = rule.apply(&plan, &stats)?.unwrap(); // unwraps if `changed == true`
///
/// // 3. The rule reorders them so the more selective Filter(Eq) is applied first (deeper in the tree)
/// if let LogicalOp::Unary { op: UnaryOp::Filter(Predicate::Ne { .. }), input } = optimized_plan.root {
///     assert!(matches!(
///         *input,
///         LogicalOp::Unary { op: UnaryOp::Filter(Predicate::Eq { .. }), .. }
///     ));
/// } else {
///     panic!("Expected Ne filter at root after reordering");
/// }
/// # Ok(())
/// # }
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
        let mut filters = Vec::with_capacity(4); // ⚡ Bolt Optimization: Pre-allocate capacity for query filter lists to prevent multiple reallocations during logical plan extraction.
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
                ScanOp::NodeScan { estimated_rows, .. } => {
                    estimated_rows.unwrap_or(DEFAULT_NODE_SCAN_CARDINALITY)
                }
                ScanOp::VectorSearch { k, .. } => *k,
                ScanOp::TemporalNodeLookup { node_ids, .. } => node_ids.len(),
                ScanOp::TemporalVectorSearch { k, .. } => *k,
                ScanOp::SimilarToNode { k, .. } => *k,
                ScanOp::PropertyScan { .. } => DEFAULT_PROPERTY_SCAN_CARDINALITY,
                ScanOp::EdgeScan { estimated_rows, .. } => {
                    estimated_rows.unwrap_or(DEFAULT_EDGE_SCAN_CARDINALITY)
                }
            },
            LogicalOp::Unary {
                op: UnaryOp::Filter(_),
                input,
            } => (self.estimate_cardinality(input) as f64 * DEFAULT_FILTER_SELECTIVITY) as usize,
            LogicalOp::Unary {
                op: UnaryOp::Limit(n),
                ..
            } => *n,
            LogicalOp::Unary { input, .. } => self.estimate_cardinality(input),
            LogicalOp::Binary { left, right, .. } => {
                let left_card = self.estimate_cardinality(left);
                let right_card = self.estimate_cardinality(right);
                (left_card as f64 * right_card as f64 * DEFAULT_JOIN_SELECTIVITY) as usize
            }
            LogicalOp::Empty => 0,
        }
    }

    /// Check if two filter chains are equal (same filters in same order).
    ///
    /// `LogicalOp` and `Predicate` both derive `PartialEq`, so a direct `==`
    /// comparison is sufficient.
    fn filters_equal(&self, a: &LogicalOp, b: &LogicalOp) -> bool {
        a == b
    }
}

#[cfg(test)]
mod tests;
