//! Query Planner
//!
//! Transforms logical plans into optimized physical plans for execution.
//! The planner applies optimization rules and uses a cost model to choose
//! the best execution strategy.

pub mod cost;
pub mod physical;
pub mod rules;
pub mod stats;

use std::sync::Arc;

#[cfg(feature = "observability")]
use tracing;

use crate::utils::error::{Error, QueryError, Result};

use super::builder::Query;
use super::ir::QueryOp;
use super::plan::{LogicalOp, LogicalPlan, ScanOp, TemporalContext, UnaryOp};

pub use cost::{Cost, CostModel};
pub use physical::{PhysicalOp, PhysicalPlan};
pub use rules::OptimizationRule;
pub use stats::Statistics;

/// Query planner that transforms queries into executable physical plans.
pub struct QueryPlanner {
    /// Statistics for cardinality estimation
    stats: Arc<Statistics>,
    /// Cost model for plan comparison
    cost_model: CostModel,
    /// Optimization rules to apply
    rules: Vec<Box<dyn OptimizationRule>>,
}

impl QueryPlanner {
    /// Create a new query planner with the given statistics
    #[must_use]
    pub fn new(stats: Arc<Statistics>) -> Self {
        QueryPlanner {
            stats,
            cost_model: CostModel::default(),
            rules: rules::default_rules(),
        }
    }

    /// Create a planner with custom cost model
    #[must_use]
    pub fn with_cost_model(mut self, cost_model: CostModel) -> Self {
        self.cost_model = cost_model;
        self
    }

    /// Create a planner with custom optimization rules
    #[must_use]
    pub fn with_rules(mut self, rules: Vec<Box<dyn OptimizationRule>>) -> Self {
        self.rules = rules;
        self
    }

    /// Plan a query, returning an executable physical plan
    pub fn plan(&self, query: Query) -> Result<PhysicalPlan> {
        // 1. Convert query to logical plan
        let logical = self.to_logical_plan(&query)?;

        // 2. Apply optimization rules
        let optimized = self.optimize(logical)?;

        // 3. Generate physical plan
        let physical = self.to_physical_plan(&optimized)?;

        Ok(physical)
    }

    /// Convert a Query to a LogicalPlan
    fn to_logical_plan(&self, query: &Query) -> Result<LogicalPlan> {
        if query.ops.is_empty() {
            return Err(Error::Query(QueryError::SyntaxError {
                message: "Query has no operations".to_string(),
            }));
        }

        // Build the logical plan tree from operations
        let mut current: Option<LogicalOp> = None;

        for op in &query.ops {
            current = Some(self.apply_query_op(current, op)?);
        }

        let root = current.ok_or_else(|| {
            Error::Query(QueryError::SyntaxError {
                message: "Empty query".to_string(),
            })
        })?;

        let mut plan = LogicalPlan::new(root);

        if let Some(ref temporal) = query.temporal_context {
            plan = plan.with_temporal_context(temporal.clone());
        }

        plan = plan.with_hints(query.hints.clone());

        Ok(plan)
    }

    /// Apply a QueryOp to the current logical plan
    fn apply_query_op(&self, current: Option<LogicalOp>, op: &QueryOp) -> Result<LogicalOp> {
        match op {
            // Source operations
            QueryOp::StartNode(id) => Ok(LogicalOp::Scan(ScanOp::NodeLookup(vec![*id]))),

            QueryOp::StartNodes(ids) => Ok(LogicalOp::Scan(ScanOp::NodeLookup(ids.clone()))),

            QueryOp::ScanNodes { label } => Ok(LogicalOp::Scan(ScanOp::NodeScan {
                label: label.clone(),
                estimated_rows: None,
            })),

            QueryOp::VectorSearch {
                embedding,
                k,
                metric,
            } => Ok(LogicalOp::Scan(ScanOp::VectorSearch {
                embedding: embedding.clone(),
                k: *k,
                label_filter: None,
                metric: *metric,
            })),

            // Graph operations - require input
            QueryOp::TraverseOut { label, depth } => {
                let input = current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "Traverse requires a source".to_string(),
                    })
                })?;
                Ok(LogicalOp::unary(
                    UnaryOp::Traverse {
                        direction: super::ir::Direction::Outgoing,
                        label: label.clone(),
                        depth: *depth,
                    },
                    input,
                ))
            }

            QueryOp::TraverseIn { label, depth } => {
                let input = current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "Traverse requires a source".to_string(),
                    })
                })?;
                Ok(LogicalOp::unary(
                    UnaryOp::Traverse {
                        direction: super::ir::Direction::Incoming,
                        label: label.clone(),
                        depth: *depth,
                    },
                    input,
                ))
            }

            QueryOp::TraverseBoth { label, depth } => {
                let input = current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "Traverse requires a source".to_string(),
                    })
                })?;
                Ok(LogicalOp::unary(
                    UnaryOp::Traverse {
                        direction: super::ir::Direction::Both,
                        label: label.clone(),
                        depth: *depth,
                    },
                    input,
                ))
            }

            // Vector operations
            QueryOp::RankBySimilarity { embedding, top_k } => {
                let input = current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "RankBySimilarity requires a source".to_string(),
                    })
                })?;
                Ok(LogicalOp::unary(
                    UnaryOp::VectorRank {
                        embedding: embedding.clone(),
                        top_k: *top_k,
                    },
                    input,
                ))
            }

            QueryOp::SimilarTo { source_node, k } => {
                // TODO: SimilarTo requires multi-step planning:
                // 1. Lookup source node to extract its embedding
                // 2. Perform vector search with that embedding
                // This requires runtime embedding extraction which isn't supported yet.
                // Return error until properly implemented.
                Err(Error::Query(QueryError::SyntaxError {
                    message: format!(
                        "SimilarTo operation not yet implemented: \
                         finding {} nodes similar to {:?} requires runtime embedding extraction",
                        k, source_node
                    ),
                }))
            }

            // Filter operations
            QueryOp::Filter(predicate) => {
                let input = current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "Filter requires a source".to_string(),
                    })
                })?;
                Ok(LogicalOp::unary(UnaryOp::Filter(predicate.clone()), input))
            }

            QueryOp::FilterLabel(label) => {
                let input = current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "FilterLabel requires a source".to_string(),
                    })
                })?;
                // Convert label filter to predicate on _label property
                Ok(LogicalOp::unary(
                    UnaryOp::Filter(super::ir::Predicate::Eq {
                        key: "_label".to_string(),
                        value: super::ir::PredicateValue::String(label.clone()),
                    }),
                    input,
                ))
            }

            QueryOp::Limit(n) => {
                let input = current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "Limit requires a source".to_string(),
                    })
                })?;
                Ok(LogicalOp::unary(UnaryOp::Limit(*n), input))
            }

            QueryOp::Skip(n) => {
                let input = current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "Skip requires a source".to_string(),
                    })
                })?;
                Ok(LogicalOp::unary(UnaryOp::Skip(*n), input))
            }

            // Temporal operations are handled at plan level, not as operators
            QueryOp::AsOf { .. } | QueryOp::Between { .. } | QueryOp::TrackChanges { .. } => {
                // These are handled by temporal_context in the plan
                current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "Temporal operation requires context".to_string(),
                    })
                })
            }

            // Aggregation operations
            QueryOp::Count => {
                let input = current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "Count requires a source".to_string(),
                    })
                })?;
                Ok(LogicalOp::unary(UnaryOp::Count, input))
            }

            QueryOp::Distinct => {
                let input = current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "Distinct requires a source".to_string(),
                    })
                })?;
                Ok(LogicalOp::unary(UnaryOp::Distinct, input))
            }

            QueryOp::Project(props) => {
                let input = current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "Project requires a source".to_string(),
                    })
                })?;
                Ok(LogicalOp::unary(UnaryOp::Project(props.clone()), input))
            }

            QueryOp::GetEdges { direction: _ } => {
                // Handle get edges - for now, just pass through
                current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "GetEdges requires a source".to_string(),
                    })
                })
            }
        }
    }

    /// Apply optimization rules iteratively until no more changes
    fn optimize(&self, plan: LogicalPlan) -> Result<LogicalPlan> {
        let mut current = plan;
        let mut changed = true;
        let mut iterations = 0;
        const MAX_ITERATIONS: usize = 100;

        while changed && iterations < MAX_ITERATIONS {
            changed = false;
            for rule in &self.rules {
                if let Some(optimized) = rule.apply(&current, &self.stats)? {
                    current = optimized;
                    changed = true;
                }
            }
            iterations += 1;
        }

        // Warn if optimization didn't converge - may indicate cyclic rules
        if iterations >= MAX_ITERATIONS && changed {
            #[cfg(feature = "observability")]
            tracing::warn!(
                "Query optimization reached maximum iterations ({}), \
                 optimization may be incomplete - check for cyclic rules",
                MAX_ITERATIONS
            );
        }

        Ok(current)
    }

    /// Convert logical plan to physical plan
    fn to_physical_plan(&self, logical: &LogicalPlan) -> Result<PhysicalPlan> {
        let physical_op = self.to_physical_op(&logical.root, &logical.temporal_context)?;
        let cost = self.cost_model.estimate(&physical_op, &self.stats);

        Ok(PhysicalPlan {
            root: physical_op,
            estimated_cost: cost,
            temporal_context: logical.temporal_context.clone(),
            parallel: logical.hints.parallel,
        })
    }

    /// Convert a logical operator to a physical operator
    fn to_physical_op(
        &self,
        logical: &LogicalOp,
        temporal: &Option<TemporalContext>,
    ) -> Result<PhysicalOp> {
        match logical {
            LogicalOp::Scan(scan) => self.scan_to_physical(scan, temporal),

            LogicalOp::Unary { op, input } => {
                let physical_input = self.to_physical_op(input, temporal)?;
                self.unary_to_physical(op, physical_input)
            }

            LogicalOp::Binary { op, left, right } => {
                let physical_left = self.to_physical_op(left, temporal)?;
                let physical_right = self.to_physical_op(right, temporal)?;
                self.binary_to_physical(op, physical_left, physical_right)
            }

            LogicalOp::Empty => Ok(PhysicalOp::Empty),
        }
    }

    /// Convert a scan operation to physical
    ///
    /// # Limitations
    ///
    /// TODO: This function does not validate that required indexes exist (e.g., vector index
    /// for HnswSearch). Validation happens at execution time in the executor. For better
    /// error messages, the planner should receive a reference to CurrentStorage to check
    /// index availability during planning. See issue for future improvement.
    fn scan_to_physical(
        &self,
        scan: &ScanOp,
        temporal: &Option<TemporalContext>,
    ) -> Result<PhysicalOp> {
        match scan {
            ScanOp::NodeLookup(ids) => {
                if let Some((valid_time, tx_time)) = temporal.as_ref().and_then(|ctx| ctx.as_of) {
                    return Ok(PhysicalOp::TemporalNodeLookup {
                        node_ids: ids.clone(),
                        valid_time,
                        transaction_time: tx_time,
                    });
                }
                Ok(PhysicalOp::NodeLookup {
                    node_ids: ids.clone(),
                })
            }

            ScanOp::NodeScan {
                label,
                estimated_rows,
            } => Ok(PhysicalOp::NodeScan {
                label: label.clone(),
                estimated_rows: estimated_rows.unwrap_or(1000),
            }),

            ScanOp::VectorSearch {
                embedding,
                k,
                label_filter,
                metric: _,
            } => {
                if let Some((_, tx_time)) = temporal.as_ref().and_then(|ctx| ctx.as_of) {
                    return Ok(PhysicalOp::TemporalVectorSearch {
                        embedding: embedding.clone(),
                        k: *k,
                        timestamp: tx_time,
                    });
                }
                Ok(PhysicalOp::HnswSearch {
                    embedding: embedding.clone(),
                    k: *k,
                    label_filter: label_filter.clone(),
                })
            }

            ScanOp::TemporalNodeLookup {
                node_ids,
                valid_time,
                transaction_time,
            } => Ok(PhysicalOp::TemporalNodeLookup {
                node_ids: node_ids.clone(),
                valid_time: *valid_time,
                transaction_time: *transaction_time,
            }),

            ScanOp::TemporalVectorSearch {
                embedding,
                k,
                timestamp,
            } => Ok(PhysicalOp::TemporalVectorSearch {
                embedding: embedding.clone(),
                k: *k,
                timestamp: *timestamp,
            }),
        }
    }

    /// Convert a unary operation to physical
    fn unary_to_physical(&self, op: &UnaryOp, input: PhysicalOp) -> Result<PhysicalOp> {
        match op {
            UnaryOp::Filter(predicate) => Ok(PhysicalOp::Filter {
                input: Box::new(input),
                predicate: predicate.clone(),
            }),

            UnaryOp::Limit(n) => Ok(PhysicalOp::Limit {
                input: Box::new(input),
                count: *n,
                offset: 0,
            }),

            UnaryOp::Skip(n) => {
                // Skip is implemented as Limit with offset
                // We need to know the total to implement properly
                // For now, wrap with a marker
                Ok(PhysicalOp::Limit {
                    input: Box::new(input),
                    count: usize::MAX,
                    offset: *n,
                })
            }

            UnaryOp::Traverse {
                direction,
                label,
                depth,
            } => Ok(PhysicalOp::IndexedTraversal {
                input: Box::new(input),
                direction: *direction,
                label: label.clone(),
                depth: depth.max_depth().unwrap_or(10),
            }),

            UnaryOp::VectorRank { embedding, top_k } => Ok(PhysicalOp::VectorRerank {
                input: Box::new(input),
                embedding: embedding.clone(),
                k: top_k.unwrap_or(10),
            }),

            UnaryOp::Sort { key, descending } => Ok(PhysicalOp::Sort {
                input: Box::new(input),
                key: key.clone(),
                descending: *descending,
            }),

            UnaryOp::Project(props) => Ok(PhysicalOp::Project {
                input: Box::new(input),
                properties: props.clone(),
            }),

            UnaryOp::Distinct => Ok(PhysicalOp::Distinct {
                input: Box::new(input),
            }),

            UnaryOp::Count => Ok(PhysicalOp::Count {
                input: Box::new(input),
            }),

            UnaryOp::TemporalTrack { time_range } => Ok(PhysicalOp::TemporalTrack {
                input: Box::new(input),
                time_range: *time_range,
            }),
        }
    }

    /// Convert a binary operation to physical
    fn binary_to_physical(
        &self,
        op: &super::plan::BinaryOp,
        left: PhysicalOp,
        right: PhysicalOp,
    ) -> Result<PhysicalOp> {
        match op {
            super::plan::BinaryOp::Union => Ok(PhysicalOp::Union {
                left: Box::new(left),
                right: Box::new(right),
            }),

            super::plan::BinaryOp::Intersect => Ok(PhysicalOp::Intersect {
                left: Box::new(left),
                right: Box::new(right),
            }),

            super::plan::BinaryOp::Except => Ok(PhysicalOp::Except {
                left: Box::new(left),
                right: Box::new(right),
            }),

            super::plan::BinaryOp::Join {
                left_key,
                right_key,
            } => Ok(PhysicalOp::HashJoin {
                left: Box::new(left),
                right: Box::new(right),
                left_key: left_key.clone(),
                right_key: right_key.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::NodeId;
    use crate::query::builder::QueryBuilder;
    use crate::query::plan::QueryHints;

    fn test_planner() -> QueryPlanner {
        QueryPlanner::new(Arc::new(Statistics::default()))
    }

    #[test]
    fn test_simple_node_lookup() {
        let planner = test_planner();
        let query = QueryBuilder::new().start(NodeId::new(1).unwrap()).build();

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::NodeLookup { .. }));
    }

    #[test]
    fn test_traverse_planning() {
        let planner = test_planner();
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .traverse("KNOWS")
            .build();

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::IndexedTraversal { .. }));
    }

    #[test]
    fn test_vector_search_planning() {
        let planner = test_planner();
        let embedding = [0.1f32; 4];
        let query = QueryBuilder::new().find_similar(&embedding, 10).build();

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::HnswSearch { .. }));
    }

    #[test]
    fn test_temporal_planning() {
        let planner = test_planner();
        let query = QueryBuilder::new()
            .as_of(1000, 2000)
            .start(NodeId::new(1).unwrap())
            .build();

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::TemporalNodeLookup { .. }));
    }

    #[test]
    fn test_hybrid_planning() {
        let planner = test_planner();
        let embedding = [0.1f32; 4];
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .traverse("KNOWS")
            .rank_by_similarity(&embedding, 10)
            .build();

        let plan = planner.plan(query).unwrap();
        // Should be VectorRerank(IndexedTraversal(NodeLookup))
        assert!(matches!(plan.root, PhysicalOp::VectorRerank { .. }));
    }

    #[test]
    fn test_empty_query_error() {
        let planner = test_planner();
        let query = Query {
            ops: vec![],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        assert!(planner.plan(query).is_err());
    }
}
