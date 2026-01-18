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

use crate::storage::CurrentStorage;
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
    /// Reference to current storage for index validation
    storage: Arc<CurrentStorage>,
}

impl QueryPlanner {
    /// Create a new query planner with the given statistics and storage
    ///
    /// The storage reference is used to validate that required indexes exist during
    /// query planning, providing earlier and more informative error messages.
    #[must_use]
    pub fn new(stats: Arc<Statistics>, storage: Arc<CurrentStorage>) -> Self {
        QueryPlanner {
            stats,
            cost_model: CostModel::default(),
            rules: rules::default_rules(),
            storage,
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
                property_key,
            } => Ok(LogicalOp::Scan(ScanOp::VectorSearch {
                embedding: embedding.clone(),
                k: *k,
                label_filter: None,
                metric: *metric,
                property_key: property_key.clone(),
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
            QueryOp::RankBySimilarity {
                embedding,
                top_k,
                property_key,
            } => {
                let input = current.ok_or_else(|| {
                    Error::Query(QueryError::SyntaxError {
                        message: "RankBySimilarity requires a source".to_string(),
                    })
                })?;
                Ok(LogicalOp::unary(
                    UnaryOp::VectorRank {
                        embedding: embedding.clone(),
                        top_k: *top_k,
                        property_key: property_key.clone(),
                    },
                    input,
                ))
            }

            QueryOp::SimilarTo {
                source_node,
                k,
                property_key,
                label_filter,
            } => {
                // SimilarTo is a scan operation that looks up a node, extracts its embedding,
                // and performs k-NN search - all handled by the executor
                Ok(LogicalOp::Scan(ScanOp::SimilarToNode {
                    source_node: *source_node,
                    property_key: property_key.as_deref().unwrap_or("embedding").to_string(),
                    k: *k,
                    label_filter: label_filter.clone(),
                }))
            }

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
            include_provenance: logical.hints.include_provenance,
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
                self.unary_to_physical(op, physical_input, temporal)
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
    /// # Index Validation
    ///
    /// This function validates that required indexes exist before generating physical
    /// operations. If a vector index is required but not enabled, it returns an
    /// `IndexNotFound` error with a helpful hint on how to enable the index.
    fn scan_to_physical(
        &self,
        scan: &ScanOp,
        temporal: &Option<TemporalContext>,
    ) -> Result<PhysicalOp> {
        match scan {
            ScanOp::NodeLookup(ids) => {
                if let Some((valid_time, tx_time)) = temporal.as_ref().and_then(|ctx| ctx.as_of) {
                    let use_batch = self.cost_model.should_use_batch_temporal_lookup(ids.len());
                    return Ok(PhysicalOp::TemporalNodeLookup {
                        node_ids: ids.clone(),
                        valid_time,
                        transaction_time: tx_time,
                        use_batch,
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
                property_key,
            } => {
                // Use specified property or default to "embedding"
                let effective_property = property_key.as_deref().unwrap_or("embedding").to_string();

                // Validate that vector index is enabled for the property
                if !self.storage.has_vector_index(&effective_property) {
                    return Err(Error::Query(QueryError::IndexNotFound {
                        index_type: "vector".to_string(),
                        property_name: effective_property,
                        hint: Some(format!(
                            "Call db.enable_vector_index(\"{}\", config) first",
                            property_key.as_deref().unwrap_or("embedding")
                        )),
                    }));
                }

                if let Some((_, tx_time)) = temporal.as_ref().and_then(|ctx| ctx.as_of) {
                    return Ok(PhysicalOp::TemporalVectorSearch {
                        embedding: embedding.clone(),
                        k: *k,
                        timestamp: tx_time,
                        property_key: property_key.clone(),
                    });
                }
                Ok(PhysicalOp::HnswSearch {
                    embedding: embedding.clone(),
                    k: *k,
                    label_filter: label_filter.clone(),
                    property_key: property_key.clone(),
                })
            }

            ScanOp::TemporalNodeLookup {
                node_ids,
                valid_time,
                transaction_time,
            } => {
                let use_batch = self
                    .cost_model
                    .should_use_batch_temporal_lookup(node_ids.len());
                Ok(PhysicalOp::TemporalNodeLookup {
                    node_ids: node_ids.clone(),
                    valid_time: *valid_time,
                    transaction_time: *transaction_time,
                    use_batch,
                })
            }

            // TemporalVectorSearch can be created via two paths:
            // 1. Direct: ScanOp::TemporalVectorSearch (from programmatic logical plan construction)
            //    - This path is used when code directly creates a LogicalPlan with TemporalVectorSearch
            //    - Less common, mainly for advanced use cases or internal transformations
            // 2. Conversion: ScanOp::VectorSearch + temporal_context → PhysicalOp::TemporalVectorSearch
            //    - This is the primary path used by QueryBuilder when .as_of() or .between() is combined
            //      with .find_similar() or .find_similar_builder().property("key")
            //    - Handled above at lines 447-453 where VectorSearch checks temporal_context
            //    - The property_key from VectorSearch is preserved in the conversion
            // Both paths validate property_key against enabled vector indexes.
            ScanOp::TemporalVectorSearch {
                embedding,
                k,
                timestamp,
                property_key,
            } => {
                // Use specified property or default to "embedding"
                let effective_property = property_key.as_deref().unwrap_or("embedding");

                // Validate that vector index is enabled for the property
                if !self.storage.has_vector_index(effective_property) {
                    return Err(Error::Query(QueryError::IndexNotFound {
                        index_type: "vector".to_string(),
                        property_name: effective_property.to_string(),
                        hint: Some(format!(
                            "Call db.enable_vector_index(\"{}\", config) first",
                            effective_property
                        )),
                    }));
                }

                Ok(PhysicalOp::TemporalVectorSearch {
                    embedding: embedding.clone(),
                    k: *k,
                    timestamp: *timestamp,
                    property_key: property_key.clone(),
                })
            }
            ScanOp::SimilarToNode {
                source_node,
                property_key,
                k,
                label_filter,
            } => {
                // Validate that vector index is enabled for SimilarTo queries
                if !self.storage.is_vector_index_enabled() {
                    let property_name = property_key.clone();
                    return Err(Error::Query(QueryError::IndexNotFound {
                        index_type: "vector".to_string(),
                        property_name,
                        hint: Some(
                            "Call db.enable_vector_index(\"embedding\", config) first".to_string(),
                        ),
                    }));
                }

                Ok(PhysicalOp::SimilarToNode {
                    source_node: *source_node,
                    property_key: property_key.clone(),
                    k: *k,
                    label_filter: label_filter.clone(),
                })
            }
        }
    }

    /// Convert a unary operation to physical
    fn unary_to_physical(
        &self,
        op: &UnaryOp,
        input: PhysicalOp,
        temporal: &Option<TemporalContext>,
    ) -> Result<PhysicalOp> {
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
            } => {
                // Extract temporal context for edge filtering during traversal
                let temporal_ctx = temporal.as_ref().and_then(|ctx| ctx.as_of);
                Ok(PhysicalOp::IndexedTraversal {
                    input: Box::new(input),
                    direction: *direction,
                    label: label.clone(),
                    depth: depth.max_depth().unwrap_or(10),
                    temporal_context: temporal_ctx,
                })
            }

            UnaryOp::VectorRank {
                embedding,
                top_k,
                property_key,
            } => {
                // Use specified property or default to "embedding"
                let effective_property = property_key.as_deref().unwrap_or("embedding").to_string();

                // Validate that vector index is enabled for reranking
                if !self.storage.has_vector_index(&effective_property) {
                    return Err(Error::Query(QueryError::IndexNotFound {
                        index_type: "vector".to_string(),
                        property_name: effective_property,
                        hint: Some(format!(
                            "Call db.enable_vector_index(\"{}\", config) first",
                            property_key.as_deref().unwrap_or("embedding")
                        )),
                    }));
                }

                Ok(PhysicalOp::VectorRerank {
                    input: Box::new(input),
                    embedding: embedding.clone(),
                    k: top_k.unwrap_or(10),
                    property_key: property_key.clone(),
                })
            }

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
    use crate::query::ir::{Direction, Predicate, TraversalDepth};
    use crate::query::plan::QueryHints;

    fn test_planner() -> QueryPlanner {
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::hnsw::HnswConfig;
        use crate::storage::CurrentStorage;

        // Create storage with vector index enabled for most tests
        let storage = Arc::new(CurrentStorage::new());
        let config = HnswConfig::new(4, DistanceMetric::Cosine);
        storage.enable_vector_index("embedding", config).unwrap();

        QueryPlanner::new(Arc::new(Statistics::default()), storage)
    }

    // ==================== Basic Planner Tests ====================

    #[test]
    fn test_planner_new() {
        use crate::storage::CurrentStorage;

        let stats = Arc::new(Statistics::default());
        let storage = Arc::new(CurrentStorage::new());
        let planner = QueryPlanner::new(Arc::clone(&stats), storage);
        // Verify the planner was created (no public fields to check)
        let _ = planner;
    }

    #[test]
    fn test_planner_with_cost_model() {
        use crate::storage::CurrentStorage;

        let stats = Arc::new(Statistics::default());
        let storage = Arc::new(CurrentStorage::new());
        let custom_cost = CostModel::default();
        let planner = QueryPlanner::new(stats, storage).with_cost_model(custom_cost);
        let _ = planner;
    }

    #[test]
    fn test_planner_with_rules() {
        use crate::storage::CurrentStorage;

        let stats = Arc::new(Statistics::default());
        let storage = Arc::new(CurrentStorage::new());
        let custom_rules: Vec<Box<dyn OptimizationRule>> = vec![];
        let planner = QueryPlanner::new(stats, storage).with_rules(custom_rules);
        let _ = planner;
    }

    #[test]
    fn test_simple_node_lookup() {
        let planner = test_planner();
        let query = QueryBuilder::new().start(NodeId::new(1).unwrap()).build();

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::NodeLookup { .. }));
    }

    #[test]
    fn test_multiple_node_lookup() {
        let planner = test_planner();
        let ids = vec![
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            NodeId::new(3).unwrap(),
        ];
        let query = QueryBuilder::new().start_from(ids.clone()).build();

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::NodeLookup { node_ids } => {
                assert_eq!(node_ids.len(), 3);
            }
            _ => panic!("Expected NodeLookup"),
        }
    }

    // ==================== Node Scan Tests ====================

    #[test]
    fn test_node_scan_all() {
        let planner = test_planner();
        let query = QueryBuilder::new().scan(None).build();

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::NodeScan { label, .. } => {
                assert!(label.is_none());
            }
            _ => panic!("Expected NodeScan"),
        }
    }

    #[test]
    fn test_node_scan_with_label() {
        let planner = test_planner();
        let query = QueryBuilder::new().scan_label("Person").build();

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::NodeScan { label, .. } => {
                assert_eq!(label.as_ref().unwrap(), "Person");
            }
            _ => panic!("Expected NodeScan"),
        }
    }

    // ==================== Traverse Tests ====================

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
    fn test_traverse_outgoing() {
        let planner = test_planner();
        // Use traverse() which defaults to outgoing
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .traverse("KNOWS")
            .build();

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::IndexedTraversal { direction, .. } => {
                assert_eq!(*direction, Direction::Outgoing);
            }
            _ => panic!("Expected IndexedTraversal"),
        }
    }

    #[test]
    fn test_traverse_incoming() {
        let planner = test_planner();
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .traverse_in("KNOWS")
            .build();

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::IndexedTraversal { direction, .. } => {
                assert_eq!(*direction, Direction::Incoming);
            }
            _ => panic!("Expected IndexedTraversal"),
        }
    }

    #[test]
    fn test_traverse_both() {
        let planner = test_planner();
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .traverse_both("KNOWS")
            .build();

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::IndexedTraversal { direction, .. } => {
                assert_eq!(*direction, Direction::Both);
            }
            _ => panic!("Expected IndexedTraversal"),
        }
    }

    #[test]
    fn test_traverse_without_source_error() {
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::TraverseOut {
                label: Some("KNOWS".to_string()),
                depth: TraversalDepth::Exact(1),
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        assert!(planner.plan(query).is_err());
    }

    // ==================== Filter Tests ====================

    #[test]
    fn test_filter_planning() {
        let planner = test_planner();
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .filter(Predicate::eq("name", "Alice"))
            .build();

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::Filter { .. }));
    }

    #[test]
    fn test_filter_without_source_error() {
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::Filter(Predicate::True)],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        assert!(planner.plan(query).is_err());
    }

    #[test]
    fn test_filter_label_planning() {
        let planner = test_planner();
        let query = QueryBuilder::new().scan(None).with_label("Person").build();

        let plan = planner.plan(query).unwrap();
        // with_label gets converted to Filter with _label predicate
        assert!(matches!(plan.root, PhysicalOp::Filter { .. }));
    }

    // ==================== Limit/Skip Tests ====================

    #[test]
    fn test_limit_planning() {
        let planner = test_planner();
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .limit(10)
            .build();

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::Limit { count, offset, .. } => {
                assert_eq!(*count, 10);
                assert_eq!(*offset, 0);
            }
            _ => panic!("Expected Limit"),
        }
    }

    #[test]
    fn test_skip_planning() {
        let planner = test_planner();
        let query = QueryBuilder::new().scan(None).skip(5).build();

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::Limit { offset, .. } => {
                assert_eq!(*offset, 5);
            }
            _ => panic!("Expected Limit with offset (Skip)"),
        }
    }

    #[test]
    fn test_limit_without_source_error() {
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::Limit(10)],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        assert!(planner.plan(query).is_err());
    }

    // ==================== Vector Search Tests ====================

    #[test]
    fn test_vector_search_planning() {
        let planner = test_planner();
        let embedding = [0.1f32; 4];
        let query = QueryBuilder::new().find_similar(&embedding, 10).build();

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::HnswSearch { .. }));
    }

    #[test]
    fn test_vector_rerank_planning() {
        let planner = test_planner();
        let embedding = [0.1f32; 4];
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .rank_by_similarity(&embedding, 10)
            .build();

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::VectorRerank { .. }));
    }

    // ==================== Temporal Tests ====================

    #[test]
    fn test_temporal_planning() {
        let planner = test_planner();
        let query = QueryBuilder::new()
            .as_of(1000, 2000)
            .start(NodeId::new(1).unwrap())
            .build();

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::TemporalNodeLookup { .. }));
        assert!(plan.temporal_context.is_some());
    }

    #[test]
    fn test_temporal_vector_search() {
        let planner = test_planner();
        let embedding = [0.1f32; 4];
        let query = QueryBuilder::new()
            .as_of(1000, 2000)
            .find_similar(&embedding, 10)
            .build();

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::TemporalVectorSearch { .. }));
    }

    // ==================== Aggregation Tests ====================

    #[test]
    fn test_count_planning() {
        let planner = test_planner();
        // Use raw Query since count() is not on QueryBuilder
        let query = Query {
            ops: vec![QueryOp::ScanNodes { label: None }, QueryOp::Count],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::Count { .. }));
    }

    #[test]
    fn test_count_without_source_error() {
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::Count],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        assert!(planner.plan(query).is_err());
    }

    #[test]
    fn test_distinct_planning() {
        let planner = test_planner();
        // Use raw Query since distinct() is not on QueryBuilder
        let query = Query {
            ops: vec![QueryOp::ScanNodes { label: None }, QueryOp::Distinct],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::Distinct { .. }));
    }

    #[test]
    fn test_project_planning() {
        let planner = test_planner();
        // Use raw Query since project() is not on QueryBuilder
        let query = Query {
            ops: vec![
                QueryOp::StartNode(NodeId::new(1).unwrap()),
                QueryOp::Project(vec!["name".to_string(), "age".to_string()]),
            ],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::Project { properties, .. } => {
                assert_eq!(properties.len(), 2);
                assert!(properties.contains(&"name".to_string()));
                assert!(properties.contains(&"age".to_string()));
            }
            _ => panic!("Expected Project"),
        }
    }

    // ==================== Hybrid Query Tests ====================

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
    fn test_complex_query_chain() {
        let planner = test_planner();
        let query = QueryBuilder::new()
            .scan_label("Person")
            .filter(Predicate::gt("age", 21i64))
            .limit(100)
            .build();

        let plan = planner.plan(query).unwrap();
        // Should be Limit(Filter(NodeScan))
        assert!(matches!(plan.root, PhysicalOp::Limit { .. }));
    }

    // ==================== Error Cases ====================

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

    #[test]
    fn test_rank_without_source_error() {
        let planner = test_planner();
        let embedding = [0.1f32; 4];
        let query = Query {
            ops: vec![QueryOp::RankBySimilarity {
                embedding: Arc::from(embedding.as_slice()),
                top_k: Some(10),
                property_key: None,
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        assert!(planner.plan(query).is_err());
    }

    #[test]
    fn test_distinct_without_source_error() {
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::Distinct],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        assert!(planner.plan(query).is_err());
    }

    #[test]
    fn test_project_without_source_error() {
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::Project(vec!["name".to_string()])],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        assert!(planner.plan(query).is_err());
    }

    // ==================== Plan Properties Tests ====================

    #[test]
    fn test_plan_has_estimated_cost() {
        let planner = test_planner();
        let query = QueryBuilder::new().start(NodeId::new(1).unwrap()).build();

        let plan = planner.plan(query).unwrap();
        // Cost should be non-zero
        assert!(
            plan.estimated_cost.cpu > 0.0
                || plan.estimated_cost.io > 0.0
                || plan.estimated_cost.memory > 0
        );
    }

    #[test]
    fn test_plan_parallel_hint() {
        let planner = test_planner();
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .parallel()
            .build();

        let plan = planner.plan(query).unwrap();
        assert!(plan.parallel);
    }

    #[test]
    fn test_plan_default_not_parallel() {
        let planner = test_planner();
        let query = QueryBuilder::new().start(NodeId::new(1).unwrap()).build();

        let plan = planner.plan(query).unwrap();
        assert!(!plan.parallel);
    }

    // ==================== Additional Operation Tests ====================

    #[test]
    fn test_traverse_in_direction() {
        let planner = test_planner();
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .traverse_in("KNOWS")
            .build();

        let plan = planner.plan(query).unwrap();
        // Should be IndexedTraversal with Incoming direction
        if let PhysicalOp::IndexedTraversal { direction, .. } = plan.root {
            assert_eq!(direction, crate::query::ir::Direction::Incoming);
        } else {
            panic!("Expected IndexedTraversal");
        }
    }

    #[test]
    fn test_traverse_both_directions() {
        let planner = test_planner();
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .traverse_both("KNOWS")
            .build();

        let plan = planner.plan(query).unwrap();
        // Should be IndexedTraversal with Both direction
        if let PhysicalOp::IndexedTraversal { direction, .. } = plan.root {
            assert_eq!(direction, crate::query::ir::Direction::Both);
        } else {
            panic!("Expected IndexedTraversal");
        }
    }

    #[test]
    fn test_filter_label_operation() {
        let planner = test_planner();
        let query = Query {
            ops: vec![
                QueryOp::ScanNodes {
                    label: Some("Person".to_string()),
                },
                QueryOp::FilterLabel("Admin".to_string()),
            ],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        // Should be Filter(NodeScan)
        assert!(matches!(plan.root, PhysicalOp::Filter { .. }));
    }

    #[test]
    fn test_skip_operation() {
        let planner = test_planner();
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .skip(10)
            .build();

        let plan = planner.plan(query).unwrap();
        // Skip is converted to Limit with offset
        if let PhysicalOp::Limit { offset, .. } = plan.root {
            assert_eq!(offset, 10);
        } else {
            panic!("Expected Limit with offset");
        }
    }

    #[test]
    fn test_count_operation() {
        let planner = test_planner();
        let query = Query {
            ops: vec![
                QueryOp::ScanNodes {
                    label: Some("Person".to_string()),
                },
                QueryOp::Count,
            ],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        // Should be Count(NodeScan)
        assert!(matches!(plan.root, PhysicalOp::Count { .. }));
    }

    #[test]
    fn test_get_edges_requires_source() {
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::GetEdges {
                direction: crate::query::ir::Direction::Outgoing,
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let result = planner.plan(query);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("requires a source"));
    }

    #[test]
    fn test_temporal_as_of_without_source() {
        let planner = test_planner();
        let now = crate::core::temporal::time::now();
        let query = Query {
            ops: vec![QueryOp::AsOf {
                valid_time: now,
                transaction_time: now,
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let result = planner.plan(query);
        assert!(result.is_err());
    }

    #[test]
    fn test_temporal_between_without_source() {
        let planner = test_planner();
        let now = crate::core::temporal::time::now();
        let query = Query {
            ops: vec![QueryOp::Between {
                time_range: crate::core::temporal::TimeRange::new(now.into(), now.into()).unwrap(),
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let result = planner.plan(query);
        assert!(result.is_err());
    }

    #[test]
    fn test_track_changes_without_source() {
        let planner = test_planner();
        let now = crate::core::temporal::time::now();
        let query = Query {
            ops: vec![QueryOp::TrackChanges {
                time_range: crate::core::temporal::TimeRange::new(now.into(), now.into()).unwrap(),
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let result = planner.plan(query);
        assert!(result.is_err());
    }

    #[test]
    fn test_temporal_node_lookup_with_context() {
        let planner = test_planner();
        let now = crate::core::temporal::time::now();
        let mut query = QueryBuilder::new().start(NodeId::new(1).unwrap()).build();

        // Add temporal context
        query.temporal_context = Some(TemporalContext {
            as_of: Some((now, now)),
            between: None,
        });

        let plan = planner.plan(query).unwrap();
        // Should be TemporalNodeLookup instead of NodeLookup
        assert!(matches!(plan.root, PhysicalOp::TemporalNodeLookup { .. }));
    }

    #[test]
    fn test_temporal_vector_search_with_context() {
        let planner = test_planner();
        let embedding = [0.1f32; 4];
        let now = crate::core::temporal::time::now();

        let mut query = Query {
            ops: vec![QueryOp::VectorSearch {
                embedding: Arc::from(embedding.as_slice()),
                k: 10,
                metric: crate::index::vector::DistanceMetric::Cosine,
                property_key: None,
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        // Add temporal context
        query.temporal_context = Some(TemporalContext {
            as_of: Some((now, now)),
            between: None,
        });

        let plan = planner.plan(query).unwrap();
        // Should be TemporalVectorSearch instead of HnswSearch
        assert!(matches!(plan.root, PhysicalOp::TemporalVectorSearch { .. }));
    }

    #[test]
    fn test_filter_label_without_source_error() {
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::FilterLabel("Person".to_string())],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let result = planner.plan(query);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("requires a source"));
    }

    #[test]
    fn test_skip_without_source_error() {
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::Skip(10)],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let result = planner.plan(query);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("requires a source"));
    }

    // ==================== SimilarTo Tests ====================

    #[test]
    fn test_similar_to_planning() {
        let planner = test_planner();
        let source_node = NodeId::new(1).unwrap();
        let query = QueryBuilder::new()
            .start(source_node)
            .similar_to(source_node, 10)
            .build();

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::SimilarToNode { .. }));
    }

    #[test]
    fn test_similar_to_node_parameters() {
        let planner = test_planner();
        let source_node = NodeId::new(42).unwrap();
        let k = 15;
        let query = Query {
            ops: vec![QueryOp::SimilarTo {
                source_node,
                k,
                property_key: None,
                label_filter: None,
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::SimilarToNode {
                source_node: sn,
                k: result_k,
                ..
            } => {
                assert_eq!(*sn, source_node);
                assert_eq!(*result_k, k);
            }
            _ => panic!("Expected SimilarToNode, got {:?}", plan.root.name()),
        }
    }

    #[test]
    fn test_similar_to_with_property_key() {
        let planner = test_planner();
        let source_node = NodeId::new(1).unwrap();
        let query = Query {
            ops: vec![QueryOp::SimilarTo {
                source_node,
                k: 10,
                property_key: None,
                label_filter: None,
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::SimilarToNode { property_key, .. } => {
                // Default property key should be "embedding"
                assert_eq!(property_key, "embedding");
            }
            _ => panic!("Expected SimilarToNode"),
        }
    }

    // ==================== Index Validation Tests (Issue #309) ====================

    #[test]
    fn test_vector_search_without_index_error() {
        use crate::storage::CurrentStorage;

        // Create planner with storage (no vector index enabled)
        let storage = Arc::new(CurrentStorage::new());
        let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

        let embedding = [0.1f32; 4];
        let query = QueryBuilder::new().find_similar(&embedding, 10).build();

        // Should fail during planning with IndexNotFound error
        let result = planner.plan(query);
        assert!(result.is_err());

        let err = result.unwrap_err();
        let err_msg = format!("{}", err);
        assert!(err_msg.contains("index"));
        assert!(err_msg.contains("embedding"));
        assert!(
            err_msg.contains("enable_vector_index"),
            "Error message should provide hint to enable index: {}",
            err_msg
        );
    }

    #[test]
    fn test_vector_rerank_without_index_error() {
        use crate::storage::CurrentStorage;

        let storage = Arc::new(CurrentStorage::new());
        let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

        let embedding = [0.1f32; 4];
        let query = QueryBuilder::new()
            .start(NodeId::new(1).unwrap())
            .rank_by_similarity(&embedding, 10)
            .build();

        let result = planner.plan(query);
        assert!(result.is_err());

        let err = result.unwrap_err();
        let err_msg = format!("{}", err).to_lowercase();
        assert!(err_msg.contains("vector"));
        assert!(err_msg.contains("index"));
        assert!(err_msg.contains("embedding"));
    }

    #[test]
    fn test_similar_to_without_index_error() {
        use crate::storage::CurrentStorage;

        let storage = Arc::new(CurrentStorage::new());
        let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

        let source_node = NodeId::new(1).unwrap();
        let query = QueryBuilder::new()
            .start(source_node)
            .similar_to(source_node, 10)
            .build();

        let result = planner.plan(query);
        assert!(result.is_err());

        let err = result.unwrap_err();
        let err_msg = format!("{}", err);
        assert!(err_msg.contains("index"));
    }

    #[test]
    fn test_temporal_vector_search_without_index_error() {
        use crate::storage::CurrentStorage;

        let storage = Arc::new(CurrentStorage::new());
        let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

        let embedding = [0.1f32; 4];
        let query = QueryBuilder::new()
            .as_of(1000, 2000)
            .find_similar(&embedding, 10)
            .build();

        let result = planner.plan(query);
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, crate::utils::error::Error::Query(_)));
    }

    // ==================== Multi-Property Temporal Vector Search Tests (Issue #411) ====================

    #[test]
    fn test_scan_op_temporal_vector_search_with_property_key() {
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::hnsw::HnswConfig;
        use crate::storage::CurrentStorage;

        // Create planner with multi-property vector index
        let storage = Arc::new(CurrentStorage::new());
        let config = HnswConfig::new(4, DistanceMetric::Cosine);
        storage
            .enable_vector_index("embedding", config.clone())
            .unwrap();
        storage
            .enable_vector_index("title_embedding", config)
            .unwrap();
        let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

        // Create a logical plan with ScanOp::TemporalVectorSearch directly
        let embedding: Arc<[f32]> = Arc::from([0.1f32; 4].as_slice());
        let logical_plan = LogicalPlan::new(LogicalOp::Scan(ScanOp::TemporalVectorSearch {
            embedding,
            k: 10,
            timestamp: 1000,
            property_key: Some("title_embedding".to_string()),
        }));

        let physical_plan = planner.to_physical_plan(&logical_plan).unwrap();
        match &physical_plan.root {
            PhysicalOp::TemporalVectorSearch { property_key, .. } => {
                assert_eq!(
                    property_key.as_deref(),
                    Some("title_embedding"),
                    "property_key should be extracted from ScanOp::TemporalVectorSearch"
                );
            }
            _ => panic!(
                "Expected TemporalVectorSearch, got {:?}",
                physical_plan.root.name()
            ),
        }
    }

    #[test]
    fn test_vector_search_with_temporal_context_preserves_property_key() {
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::hnsw::HnswConfig;
        use crate::storage::CurrentStorage;

        // This tests the existing path: VectorSearch + temporal_context -> TemporalVectorSearch
        let storage = Arc::new(CurrentStorage::new());
        let config = HnswConfig::new(4, DistanceMetric::Cosine);
        storage
            .enable_vector_index("embedding", config.clone())
            .unwrap();
        storage
            .enable_vector_index("title_embedding", config)
            .unwrap();
        let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

        let embedding = [0.1f32; 4];
        let query = QueryBuilder::new()
            .as_of(1000, 2000)
            .find_similar_builder(&embedding, 10)
            .property("title_embedding")
            .finish()
            .build();

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::TemporalVectorSearch { property_key, .. } => {
                assert_eq!(
                    property_key.as_deref(),
                    Some("title_embedding"),
                    "property_key should be preserved through VectorSearch->TemporalVectorSearch conversion"
                );
            }
            _ => panic!("Expected TemporalVectorSearch, got {:?}", plan.root.name()),
        }
    }

    #[test]
    fn test_temporal_vector_search_default_property() {
        let planner = test_planner();
        let embedding = [0.1f32; 4];
        let query = QueryBuilder::new()
            .as_of(1000, 2000)
            .find_similar(&embedding, 10)
            .build();

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::TemporalVectorSearch { property_key, .. } => {
                assert_eq!(
                    property_key, &None,
                    "property_key should be None when using default property"
                );
            }
            _ => panic!("Expected TemporalVectorSearch, got {:?}", plan.root.name()),
        }
    }

    #[test]
    fn test_temporal_vector_search_invalid_property_error() {
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::hnsw::HnswConfig;
        use crate::storage::CurrentStorage;

        // Create planner with only "embedding" property enabled
        let storage = Arc::new(CurrentStorage::new());
        let config = HnswConfig::new(4, DistanceMetric::Cosine);
        storage.enable_vector_index("embedding", config).unwrap();
        let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

        // Try to use a non-existent property in temporal search
        let embedding: Arc<[f32]> = Arc::from([0.1f32; 4].as_slice());
        let logical_plan = LogicalPlan::new(LogicalOp::Scan(ScanOp::TemporalVectorSearch {
            embedding,
            k: 10,
            timestamp: 1000,
            property_key: Some("nonexistent_property".to_string()),
        }));

        let result = planner.to_physical_plan(&logical_plan);
        assert!(result.is_err(), "Should reject invalid property name");

        let err = result.unwrap_err();
        match err {
            Error::Query(QueryError::IndexNotFound {
                index_type,
                property_name,
                ..
            }) => {
                assert_eq!(index_type, "vector");
                assert_eq!(property_name, "nonexistent_property");
            }
            _ => panic!("Expected IndexNotFound error, got {:?}", err),
        }
    }
}
