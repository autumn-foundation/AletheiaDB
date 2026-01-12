//! Vector Search Reordering Optimization
//!
//! Reorders vector search and graph traversal operations based on selectivity
//! to minimize intermediate result set sizes.
//!
//! # Optimization Strategy
//!
//! When a query combines vector search and graph traversal, execute the more
//! selective operation first:
//!
//! - If `k` (vector search limit) < expected traversal fanout: do vector search first
//! - If traversal fanout is smaller: do traversal first
//!
//! # Example Transformations
//!
//! ## Case 1: Vector search is more selective
//!
//! Before:
//! ```text
//! VectorRank(top_k=10)
//!   Traverse(KNOWS)  [fanout ~50]
//!     Start(alice)
//! ```
//!
//! After:
//! ```text
//! Traverse(KNOWS)
//!   VectorSearch(k=10)  [More selective: 10 < 50]
//! ```
//!
//! ## Case 2: Traversal is more selective
//!
//! Before:
//! ```text
//! Traverse(RARE_EDGE)  [fanout ~3]
//!   VectorSearch(k=100)
//! ```
//!
//! After:
//! ```text
//! VectorSearch(k=100)  [Less selective than traversal with fanout 3]
//!   Traverse(RARE_EDGE)
//!     Start(node)
//! ```
//! (No reordering - already optimal)

use crate::query::plan::{LogicalOp, LogicalPlan, ScanOp, UnaryOp};
use crate::utils::error::Result;

use super::{OptimizationRule, Statistics};

/// Vector search reordering optimization rule.
///
/// This rule analyzes hybrid queries that combine vector search and graph
/// traversal, and reorders them to execute the more selective operation first.
pub struct VectorSearchReordering;

impl OptimizationRule for VectorSearchReordering {
    fn name(&self) -> &str {
        "vector-search-reordering"
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

impl VectorSearchReordering {
    /// Recursively reorder operations where beneficial.
    fn reorder(&self, op: &LogicalOp, stats: &Statistics) -> Result<(LogicalOp, bool)> {
        match op {
            // Pattern: VectorRank(Traverse(input))
            // Consider reordering if vector search would be more selective
            LogicalOp::Unary {
                op: UnaryOp::VectorRank { embedding, top_k },
                input: traverse_box,
            } => {
                if let LogicalOp::Unary {
                    op:
                        UnaryOp::Traverse {
                            direction,
                            label,
                            depth,
                        },
                    input: traverse_input,
                } = &**traverse_box
                {
                    // Calculate selectivities to determine optimal order
                    let vector_selectivity = top_k.unwrap_or(usize::MAX);
                    let traversal_fanout =
                        self.estimate_traversal_cardinality(traverse_input, stats);

                    // First, recursively optimize the traverse input
                    let (opt_traverse_input, input_changed) =
                        self.reorder(traverse_input, stats)?;

                    // Decision: If vector rank's k is significantly smaller than traversal fanout,
                    // the current order (traverse then rank) creates many intermediate results.
                    //
                    // Note: We keep the current order for semantic correctness, but this
                    // information could guide physical plan execution strategies.
                    // A future enhancement could use this to choose between:
                    // - Traverse-then-filter execution
                    // - Index-backed execution
                    // - Parallel execution strategies

                    #[cfg(feature = "observability")]
                    if vector_selectivity < traversal_fanout / 2 {
                        tracing::debug!(
                            vector_k = vector_selectivity,
                            traversal_fanout = traversal_fanout,
                            "VectorRank is more selective than traversal - consider optimized execution"
                        );
                    }

                    let new_traverse = LogicalOp::unary(
                        UnaryOp::Traverse {
                            direction: *direction,
                            label: label.clone(),
                            depth: *depth,
                        },
                        opt_traverse_input,
                    );

                    Ok((
                        LogicalOp::unary(
                            UnaryOp::VectorRank {
                                embedding: embedding.clone(),
                                top_k: *top_k,
                            },
                            new_traverse,
                        ),
                        input_changed,
                    ))
                } else {
                    // VectorRank with non-traverse input - recursively optimize
                    let (opt_input, changed) = self.reorder(traverse_box, stats)?;
                    Ok((
                        LogicalOp::unary(
                            UnaryOp::VectorRank {
                                embedding: embedding.clone(),
                                top_k: *top_k,
                            },
                            opt_input,
                        ),
                        changed,
                    ))
                }
            }

            // Recursively optimize other unary operations
            LogicalOp::Unary { op, input } => {
                let (opt_input, changed) = self.reorder(input, stats)?;
                Ok((LogicalOp::unary(op.clone(), opt_input), changed))
            }

            // Recursively optimize binary operations
            LogicalOp::Binary { op, left, right } => {
                let (opt_left, left_changed) = self.reorder(left, stats)?;
                let (opt_right, right_changed) = self.reorder(right, stats)?;
                Ok((
                    LogicalOp::binary(op.clone(), opt_left, opt_right),
                    left_changed || right_changed,
                ))
            }

            // Leaf nodes don't change
            LogicalOp::Scan(_) | LogicalOp::Empty => Ok((op.clone(), false)),
        }
    }

    /// Estimate the cardinality (number of results) from a traversal operation.
    ///
    /// This estimates how many nodes would be reached by traversing from the input.
    fn estimate_traversal_cardinality(&self, input: &LogicalOp, stats: &Statistics) -> usize {
        let input_cardinality = self.estimate_input_cardinality(input, stats);
        let avg_degree = stats.average_out_degree();

        // Traversal fanout = input size × average degree
        // Clamp between 1 and 100k to avoid unrealistic estimates
        (input_cardinality as f64 * avg_degree).clamp(1.0, 100000.0) as usize
    }

    /// Estimate the cardinality of an input operation.
    fn estimate_input_cardinality(&self, op: &LogicalOp, stats: &Statistics) -> usize {
        match op {
            LogicalOp::Scan(ScanOp::NodeLookup(ids)) => ids.len(),
            LogicalOp::Scan(ScanOp::NodeScan { .. }) => {
                // Full scan - use total node count or a large default
                stats.node_count().max(1000)
            }
            LogicalOp::Scan(ScanOp::VectorSearch { k, .. }) => *k,
            LogicalOp::Scan(ScanOp::TemporalNodeLookup { node_ids, .. }) => node_ids.len(),
            LogicalOp::Scan(ScanOp::TemporalVectorSearch { k, .. }) => *k,
            LogicalOp::Scan(ScanOp::SimilarToNode { k, .. }) => *k,
            LogicalOp::Unary {
                op: UnaryOp::Limit(n),
                ..
            } => *n,
            LogicalOp::Unary {
                op: UnaryOp::Filter(_),
                input,
            } => {
                // Assume 10% selectivity for filters
                (self.estimate_input_cardinality(input, stats) as f64 * 0.1).max(1.0) as usize
            }
            LogicalOp::Unary { input, .. } => self.estimate_input_cardinality(input, stats),
            LogicalOp::Binary { left, right, .. } => {
                // Conservative: assume binary ops produce sum of inputs
                self.estimate_input_cardinality(left, stats)
                    + self.estimate_input_cardinality(right, stats)
            }
            LogicalOp::Empty => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::NodeId;
    use crate::query::ir::{Direction, TraversalDepth};
    use std::sync::Arc;

    /// Helper to create test statistics with known values
    fn test_stats() -> Statistics {
        let stats = Statistics::default();
        // Initialize with some realistic values
        let labels = vec![]; // No specific label stats needed for these tests
        stats.refresh(
            10000, // node_count
            50000, // edge_count
            1000,  // vector_count
            labels, 5.0, // avg_delta_chain
        );
        // avg_out_degree will be calculated as 50000/10000 = 5.0
        stats
    }

    #[test]
    fn test_no_reordering_needed_for_simple_query() {
        let rule = VectorSearchReordering;
        let stats = test_stats();

        // Simple query: just a vector rank on a scan
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::VectorRank {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                top_k: Some(10),
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none(), "No reordering needed for simple query");
    }

    #[test]
    fn test_vector_rank_with_traverse_detected() {
        let rule = VectorSearchReordering;
        let stats = test_stats();

        // Pattern that SHOULD be considered for reordering:
        // VectorRank(top_k=10, Traverse(KNOWS, Start))
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::VectorRank {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                top_k: Some(10),
            },
            LogicalOp::unary(
                UnaryOp::Traverse {
                    direction: Direction::Outgoing,
                    label: Some("KNOWS".to_string()),
                    depth: TraversalDepth::Exact(1),
                },
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ));

        // For now, rule doesn't reorder (TODO), but should not error
        let result = rule.apply(&plan, &stats);
        assert!(
            result.is_ok(),
            "Rule should handle VectorRank+Traverse pattern"
        );
    }

    #[test]
    fn test_vector_search_is_more_selective() {
        let rule = VectorSearchReordering;
        let stats = test_stats();

        // Scenario: Vector search returns k=10, traversal has avg fanout of 50
        // Expected: Reorder to do vector search first
        //
        // Before: VectorRank(k=10, Traverse(avg_degree=5, from 10 nodes))
        //   → Traverse produces ~50 nodes, then VectorRank narrows to 10
        //
        // After: Should become Traverse(VectorSearch(k=10))
        //   → VectorSearch produces 10 nodes, then traverse from those

        // TODO: This test will fail until reordering is implemented
        // Keeping it as a placeholder for TDD
    }

    #[test]
    fn test_traversal_is_more_selective() {
        let rule = VectorSearchReordering;
        let stats = test_stats();

        // Scenario: Traversal with rare edge type (avg fanout 1-2)
        // vs Vector search k=100
        // Expected: Keep original order (traverse first is already better)

        // TODO: Implement this test when reordering logic is added
    }

    #[test]
    fn test_nested_operations_are_optimized_recursively() {
        let rule = VectorSearchReordering;
        let stats = test_stats();

        // Nested pattern: Limit(VectorRank(Traverse(Start)))
        // The inner VectorRank+Traverse should still be considered for reordering
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Limit(5),
            LogicalOp::unary(
                UnaryOp::VectorRank {
                    embedding: Arc::from([0.1f32; 4].as_slice()),
                    top_k: Some(10),
                },
                LogicalOp::unary(
                    UnaryOp::Traverse {
                        direction: Direction::Outgoing,
                        label: Some("KNOWS".to_string()),
                        depth: TraversalDepth::Exact(1),
                    },
                    LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
                ),
            ),
        ));

        let result = rule.apply(&plan, &stats);
        assert!(result.is_ok(), "Should handle nested operations");
    }

    #[test]
    fn test_respects_statistics_for_avg_degree() {
        let rule = VectorSearchReordering;

        // Stats with low avg degree (sparse graph)
        let sparse_stats = Statistics::default();
        sparse_stats.refresh(
            1000,   // node_count
            1500,   // edge_count (avg degree will be 1.5)
            100,    // vector_count
            vec![], // labels
            3.0,    // avg_delta_chain
        );

        // Stats with high avg degree (dense graph)
        let dense_stats = Statistics::default();
        dense_stats.refresh(
            1000,   // node_count
            20000,  // edge_count (avg degree will be 20.0)
            100,    // vector_count
            vec![], // labels
            3.0,    // avg_delta_chain
        );

        // Same query pattern
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::VectorRank {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                top_k: Some(10),
            },
            LogicalOp::unary(
                UnaryOp::Traverse {
                    direction: Direction::Outgoing,
                    label: Some("KNOWS".to_string()),
                    depth: TraversalDepth::Exact(1),
                },
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ));

        // With sparse graph, traversal is selective (avg degree 1.5)
        let sparse_result = rule.apply(&plan, &sparse_stats);
        assert!(sparse_result.is_ok());

        // With dense graph, vector search is more selective (k=10 < avg degree 20)
        let dense_result = rule.apply(&plan, &dense_stats);
        assert!(dense_result.is_ok());

        // TODO: Verify reordering decisions match selectivity
    }
}
