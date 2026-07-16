//! Query Planner
//!
//! Transforms logical plans into optimized physical plans for execution.
//! The planner applies optimization rules and uses a cost model to choose
//! the best execution strategy.
//!
//! # Life of a Query
//!
//! 1. **Logical Planning**: The `Query` struct (IR) is converted into a `LogicalPlan`.
//!    This represents *what* to compute without specifying *how*.
//!    - Example: `Scan(Person) -> Filter(age > 30)`
//!
//! 2. **Optimization**: The planner applies a series of `OptimizationRule`s to the logical plan.
//!    - **Filter Pushdown**: Moves filters closer to the data source.
//!    - **Join Reordering**: Reorders joins to minimize intermediate result sizes.
//!    - **Cost-Based Decisions**: Uses `Statistics` and a `CostModel` to estimate the cost of different plans.
//!
//! 3. **Physical Planning**: The optimized logical plan is converted into a `PhysicalPlan`.
//!    This represents the actual execution strategy (e.g., "Index Scan" vs "Full Scan").
//!    - Example: `IndexScan(Person, age > 30)`
//!
//! 4. **Execution**: The `PhysicalPlan` is handed off to the `Executor` (not part of this module).
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use aletheiadb::query::planner::{QueryPlanner, Statistics};
//! use aletheiadb::storage::CurrentStorage;
//! use aletheiadb::query::builder::QueryBuilder;
//! use aletheiadb::core::NodeId;
//!
//! // 1. Setup dependencies
//! let storage = Arc::new(CurrentStorage::new());
//! let stats = Arc::new(Statistics::default());
//! let planner = QueryPlanner::new(stats, storage);
//!
//! // 2. Build a query
//! let query = QueryBuilder::new()
//!     .start(NodeId::new(1).unwrap())
//!     .traverse("KNOWS")
//!     .filter(aletheiadb::query::ir::Predicate::eq("name", "Alice"))
//!     .build();
//!
//! // 3. Plan the query
//! match planner.plan(query) {
//!     Ok(physical_plan) => {
//!         println!("Plan created with cost: {:?}", physical_plan.estimated_cost);
//!         // Pass physical_plan to Executor...
//!     }
//!     Err(e) => eprintln!("Planning failed: {}", e),
//! }
//! ```

pub mod cost;
pub mod physical;
pub mod rules;
pub mod stats;

use std::sync::Arc;

#[cfg(feature = "observability")]
use tracing;

use crate::core::error::{Error, QueryError, Result};
use crate::storage::CurrentStorage;

use super::builder::Query;
use super::ir::QueryOp;
use super::plan::{LogicalOp, LogicalPlan, OptionalStep, ScanOp, TemporalContext, UnaryOp};

pub use cost::{Cost, CostModel};
pub use physical::{PhysicalOp, PhysicalPlan};
pub use rules::OptimizationRule;
pub use stats::Statistics;

// ── Planner defaults ─────────────────────────────────────────────────────────

/// Default estimated row count for a full node scan when no statistics are available.
const DEFAULT_SCAN_ESTIMATED_ROWS: usize = 1000;

/// Default estimated row count for a fused property-equality scan.
const DEFAULT_PROPERTY_SCAN_ROWS: usize = 100;

/// Fallback maximum traversal depth when the query specifies unbounded (`Variable`) depth.
const DEFAULT_MAX_TRAVERSAL_DEPTH: usize = 10;

/// Resolve the effective maximum traversal depth for a [`TraversalDepth`],
/// clamping the unbounded upper cases to [`DEFAULT_MAX_TRAVERSAL_DEPTH`].
///
/// Both `Variable` (`*`) and the `*N..` / `Min` form (which the converter
/// models as `Range { min, max: usize::MAX }`) are unbounded above and must be
/// capped so BFS terminates; finite bounds (`Exact`, `Max`, closed `Range`)
/// pass through unchanged. Configurable caps are a noted follow-up.
fn cap_traversal_depth(depth: &super::ir::TraversalDepth) -> usize {
    match depth.max_depth() {
        Some(max) if max == usize::MAX => DEFAULT_MAX_TRAVERSAL_DEPTH,
        Some(max) => max,
        None => DEFAULT_MAX_TRAVERSAL_DEPTH,
    }
}

/// Default `top_k` for vector re-rank when the caller does not provide one.
const DEFAULT_VECTOR_TOP_K: usize = 10;

/// Pseudo-property key used to represent the node label in filter predicates.
const LABEL_PSEUDO_KEY: &str = "_label";

/// Default property key assumed to hold a node's embedding vector.
const DEFAULT_EMBEDDING_PROPERTY: &str = "embedding";

/// Query planner that transforms queries into executable physical plans.
///
/// The planner is responsible for:
/// - converting the high-level `Query` IR into a `LogicalPlan`
/// - applying optimization rules to the `LogicalPlan`
/// - selecting the most efficient `PhysicalPlan` based on a cost model
pub struct QueryPlanner {
    /// Statistics for cardinality estimation (e.g., node counts, property histograms).
    stats: Arc<Statistics>,
    /// Cost model used to compare different execution plans.
    cost_model: CostModel,
    /// Ordered list of optimization rules to apply.
    rules: Vec<Box<dyn OptimizationRule>>,
    /// Reference to current storage, used to validate index existence during planning.
    storage: Arc<CurrentStorage>,
}

impl QueryPlanner {
    /// Create a new query planner with the given statistics and storage.
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

    /// Create a planner with custom cost model.
    ///
    /// The cost model is used to estimate the cost of different execution plans,
    /// enabling the planner to choose the most efficient strategy.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use std::sync::Arc;
    /// # use aletheiadb::query::planner::{QueryPlanner, Statistics, CostModel};
    /// # use aletheiadb::storage::CurrentStorage;
    /// let storage = Arc::new(CurrentStorage::new());
    /// let stats = Arc::new(Statistics::default());
    /// let custom_cost = CostModel::default();
    ///
    /// let planner = QueryPlanner::new(stats, storage)
    ///     .with_cost_model(custom_cost);
    /// ```
    #[must_use]
    pub fn with_cost_model(mut self, cost_model: CostModel) -> Self {
        self.cost_model = cost_model;
        self
    }

    /// Create a planner with custom optimization rules.
    ///
    /// Optimization rules are applied to the logical plan before physical planning.
    /// They can simplify the query, push down filters, or reorder operations.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use std::sync::Arc;
    /// # use aletheiadb::query::planner::{QueryPlanner, Statistics, OptimizationRule};
    /// # use aletheiadb::storage::CurrentStorage;
    /// let storage = Arc::new(CurrentStorage::new());
    /// let stats = Arc::new(Statistics::default());
    /// let custom_rules: Vec<Box<dyn OptimizationRule>> = vec![];
    ///
    /// let planner = QueryPlanner::new(stats, storage)
    ///     .with_rules(custom_rules);
    /// ```
    #[must_use]
    pub fn with_rules(mut self, rules: Vec<Box<dyn OptimizationRule>>) -> Self {
        self.rules = rules;
        self
    }

    /// Plan a query, returning an executable physical plan.
    ///
    /// This method orchestrates the entire planning process:
    /// 1. **Logical Planning**: Converts the `Query` IR into a `LogicalPlan`.
    /// 2. **Optimization**: Applies registered `OptimizationRule`s to improve the plan.
    /// 3. **Physical Planning**: Converts the optimized logical plan into a `PhysicalPlan`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use aletheiadb::query::planner::{QueryPlanner, Statistics};
    /// use aletheiadb::storage::CurrentStorage;
    /// use aletheiadb::query::builder::QueryBuilder;
    /// use aletheiadb::core::NodeId;
    ///
    /// // 1. Setup dependencies
    /// let storage = Arc::new(CurrentStorage::new());
    /// let stats = Arc::new(Statistics::default());
    /// let planner = QueryPlanner::new(stats, storage);
    ///
    /// // 2. Build a query
    /// let query = QueryBuilder::new()
    ///     .start(NodeId::new(1).unwrap())
    ///     .traverse("KNOWS")
    ///     .filter(aletheiadb::query::ir::Predicate::eq("name", "Alice"))
    ///     .build();
    ///
    /// // 3. Plan the query
    /// match planner.plan(query) {
    ///     Ok(physical_plan) => {
    ///         println!("Plan created with cost: {:?}", physical_plan.estimated_cost);
    ///         // Pass physical_plan to Executor...
    ///     }
    ///     Err(e) => eprintln!("Planning failed: {}", e),
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The query is invalid (e.g., empty, syntax error).
    /// - A required index is missing (e.g., vector search without enabled index).
    /// - An internal planning error occurs.
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
        // Optional (OPTIONAL MATCH) can appear both in source position (as a
        // leading clause with its own scan) and as a per-row apply operator.
        if let QueryOp::Optional { ops } = op {
            let source_position = current.is_none();
            let steps = self.convert_optional_steps(ops, source_position)?;
            let input = current.unwrap_or(LogicalOp::Empty);
            return Ok(LogicalOp::unary(UnaryOp::OptionalApply { steps }, input));
        }

        // Check if it is a source operation (starts a new pipeline)
        if let Some(source_op) = self.apply_source_op(op)? {
            return Ok(source_op);
        }

        // If not a source op, we must have a current input
        let input = current.ok_or_else(|| self.missing_source_error(op))?;

        self.apply_unary_op(input, op)
    }

    /// Convert the sub-operations of a [`QueryOp::Optional`] into
    /// [`OptionalStep`]s for the logical plan.
    ///
    /// In source position (a leading `OPTIONAL MATCH`) the first sub-op must
    /// be a `ScanNodes` source; otherwise only traversals and filters are
    /// allowed.
    fn convert_optional_steps(
        &self,
        ops: &[QueryOp],
        source_position: bool,
    ) -> Result<Vec<OptionalStep>> {
        let mut steps = Vec::with_capacity(ops.len());
        for (i, op) in ops.iter().enumerate() {
            let step = match op {
                QueryOp::ScanNodes { label } if source_position && i == 0 => OptionalStep::Scan {
                    label: label.clone(),
                },
                QueryOp::TraverseOut { label, depth } => OptionalStep::Traverse {
                    direction: super::ir::Direction::Outgoing,
                    label: label.clone(),
                    depth: *depth,
                },
                QueryOp::TraverseIn { label, depth } => OptionalStep::Traverse {
                    direction: super::ir::Direction::Incoming,
                    label: label.clone(),
                    depth: *depth,
                },
                QueryOp::TraverseBoth { label, depth } => OptionalStep::Traverse {
                    direction: super::ir::Direction::Both,
                    label: label.clone(),
                    depth: *depth,
                },
                QueryOp::Filter(predicate) => OptionalStep::Filter(predicate.clone()),
                other => {
                    return Err(Error::Query(QueryError::SyntaxError {
                        message: format!("unsupported operation inside OPTIONAL MATCH: {other:?}"),
                    }));
                }
            };
            steps.push(step);
        }
        if source_position && !matches!(steps.first(), Some(OptionalStep::Scan { .. })) {
            return Err(Error::Query(QueryError::SyntaxError {
                message: "a leading OPTIONAL MATCH must begin with a node scan".to_string(),
            }));
        }
        Ok(steps)
    }

    /// Try to apply a source operation. Returns Ok(Some(op)) if successful,
    /// Ok(None) if it's not a source operation.
    fn apply_source_op(&self, op: &QueryOp) -> Result<Option<LogicalOp>> {
        match op {
            QueryOp::StartNode(id) => Ok(Some(LogicalOp::Scan(ScanOp::NodeLookup(vec![*id])))),

            QueryOp::StartNodes(ids) => Ok(Some(LogicalOp::Scan(ScanOp::NodeLookup(ids.clone())))),

            QueryOp::ScanNodes { label } => Ok(Some(LogicalOp::Scan(ScanOp::NodeScan {
                label: label.clone(),
                estimated_rows: None,
            }))),

            QueryOp::ScanEdges { edge_type } => Ok(Some(LogicalOp::Scan(ScanOp::EdgeScan {
                edge_type: edge_type.clone(),
                estimated_rows: None,
            }))),

            QueryOp::VectorSearch {
                embedding,
                k,
                metric,
                property_key,
            } => Ok(Some(LogicalOp::Scan(ScanOp::VectorSearch {
                embedding: embedding.clone(),
                k: *k,
                label_filter: None,
                metric: *metric,
                property_key: property_key.clone(),
            }))),

            QueryOp::SimilarTo {
                source_node,
                k,
                property_key,
                label_filter,
            } => {
                // SimilarTo is a scan operation that looks up a node, extracts its embedding,
                // and performs k-NN search - all handled by the executor
                Ok(Some(LogicalOp::Scan(ScanOp::SimilarToNode {
                    source_node: *source_node,
                    property_key: property_key
                        .as_deref()
                        .unwrap_or(DEFAULT_EMBEDDING_PROPERTY)
                        .to_string(),
                    k: *k,
                    label_filter: label_filter.clone(),
                })))
            }

            _ => Ok(None),
        }
    }

    /// Apply a unary operation to an input logical op.
    fn apply_unary_op(&self, input: LogicalOp, op: &QueryOp) -> Result<LogicalOp> {
        match op {
            // Graph operations
            QueryOp::TraverseOut { label, depth } => Ok(LogicalOp::unary(
                UnaryOp::Traverse {
                    direction: super::ir::Direction::Outgoing,
                    label: label.clone(),
                    depth: *depth,
                },
                input,
            )),

            QueryOp::TraverseIn { label, depth } => Ok(LogicalOp::unary(
                UnaryOp::Traverse {
                    direction: super::ir::Direction::Incoming,
                    label: label.clone(),
                    depth: *depth,
                },
                input,
            )),

            QueryOp::TraverseBoth { label, depth } => Ok(LogicalOp::unary(
                UnaryOp::Traverse {
                    direction: super::ir::Direction::Both,
                    label: label.clone(),
                    depth: *depth,
                },
                input,
            )),

            // Vector operations
            QueryOp::RankBySimilarity {
                embedding,
                top_k,
                property_key,
                metric,
                threshold,
                score_alias,
            } => Ok(LogicalOp::unary(
                UnaryOp::VectorRank {
                    embedding: embedding.clone(),
                    top_k: *top_k,
                    property_key: property_key.clone(),
                    metric: *metric,
                    threshold: *threshold,
                    score_alias: score_alias.clone(),
                },
                input,
            )),

            // Filter operations
            QueryOp::Filter(predicate) => {
                Ok(LogicalOp::unary(UnaryOp::Filter(predicate.clone()), input))
            }

            QueryOp::FilterLabel(label) => Ok(LogicalOp::unary(
                UnaryOp::Filter(super::ir::Predicate::Eq {
                    key: LABEL_PSEUDO_KEY.to_string(),
                    value: super::ir::PredicateValue::String(label.clone()),
                }),
                input,
            )),

            QueryOp::Limit(n) => Ok(LogicalOp::unary(UnaryOp::Limit(*n), input)),

            QueryOp::Skip(n) => Ok(LogicalOp::unary(UnaryOp::Skip(*n), input)),

            // Aggregation operations
            QueryOp::Count => Ok(LogicalOp::unary(UnaryOp::Count, input)),

            QueryOp::Aggregate {
                group_keys,
                aggregates,
            } => Ok(LogicalOp::unary(
                UnaryOp::Aggregate {
                    group_keys: group_keys.clone(),
                    aggregates: aggregates.clone(),
                },
                input,
            )),

            QueryOp::TemporalWindowAggregate(spec) => Ok(LogicalOp::unary(
                UnaryOp::TemporalWindowAggregate(spec.clone()),
                input,
            )),

            QueryOp::TemporalAlign(spec) => Ok(LogicalOp::unary(
                UnaryOp::TemporalAlign(spec.clone()),
                input,
            )),

            QueryOp::Distinct => Ok(LogicalOp::unary(UnaryOp::Distinct, input)),

            QueryOp::Project(props) => Ok(LogicalOp::unary(UnaryOp::Project(props.clone()), input)),

            QueryOp::ProjectProvenance(projection) => Ok(LogicalOp::unary(
                UnaryOp::ProjectProvenance(projection.clone()),
                input,
            )),

            QueryOp::Sort { key, descending } => Ok(LogicalOp::unary(
                UnaryOp::Sort {
                    key: key.clone(),
                    descending: *descending,
                },
                input,
            )),

            QueryOp::GetEdges { direction: _ } => {
                // Handle get edges - for now, just pass through
                Ok(input)
            }

            // Temporal operations are handled at plan level, not as operators
            QueryOp::AsOf { .. } | QueryOp::Between { .. } | QueryOp::TrackChanges { .. } => {
                // Pass through the input unchanged (original behavior)
                Ok(input)
            }

            // Should have been handled by apply_source_op
            _ => Err(Error::Query(QueryError::SyntaxError {
                message: format!("Unexpected source operation in unary context: {:?}", op),
            })),
        }
    }

    /// Generate an appropriate error message for missing source.
    fn missing_source_error(&self, op: &QueryOp) -> Error {
        let op_name = match op {
            QueryOp::TraverseOut { .. }
            | QueryOp::TraverseIn { .. }
            | QueryOp::TraverseBoth { .. } => "Traverse",
            QueryOp::Filter(_) => "Filter",
            QueryOp::FilterLabel(_) => "FilterLabel",
            QueryOp::Limit(_) => "Limit",
            QueryOp::Skip(_) => "Skip",
            QueryOp::Count => "Count",
            QueryOp::Aggregate { .. } => "Aggregate",
            QueryOp::TemporalWindowAggregate(_) => "TemporalWindowAggregate",
            QueryOp::TemporalAlign(_) => "TemporalAlign",
            QueryOp::Distinct => "Distinct",
            QueryOp::Project(_) => "Project",
            QueryOp::ProjectProvenance(_) => "ProjectProvenance",
            QueryOp::Sort { .. } => "Sort",
            QueryOp::RankBySimilarity { .. } => "RankBySimilarity",
            QueryOp::GetEdges { .. } => "GetEdges",
            _ => "Operation",
        };

        Error::Query(QueryError::SyntaxError {
            message: format!("{} requires a source", op_name),
        })
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
                // Fold a chain of consecutive Sort ops (from a multi-key
                // `ORDER BY a, b, c`) into ONE multi-key PhysicalOp::Sort so the
                // FIRST-emitted key is primary. Emitting one stable Sort per key
                // would otherwise invert precedence (the last-applied stable
                // sort dominates). Fixes both Cypher and AQL (Issue #558).
                if matches!(op, UnaryOp::Sort { .. }) {
                    return self.fold_sort_chain(logical, temporal);
                }
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

    /// Fold a chain of consecutive [`UnaryOp::Sort`] operators into a single
    /// multi-key [`PhysicalOp::Sort`].
    ///
    /// The logical chain nests outermost = last-emitted `ORDER BY` key,
    /// innermost = first-emitted key. openCypher wants the first key to be
    /// primary, so we collect from the outside in, then reverse: the resulting
    /// `keys` vector is in precedence order (first = primary). The base (first
    /// non-Sort operator) is converted once.
    fn fold_sort_chain(
        &self,
        logical: &LogicalOp,
        temporal: &Option<TemporalContext>,
    ) -> Result<PhysicalOp> {
        use super::plan::SortKey;

        let mut keys: Vec<(SortKey, bool)> = Vec::new();
        let mut cursor = logical;
        let base = loop {
            match cursor {
                LogicalOp::Unary {
                    op: UnaryOp::Sort { key, descending },
                    input,
                } => {
                    keys.push((key.clone(), *descending));
                    cursor = input;
                }
                other => break other,
            }
        };

        let physical_input = self.to_physical_op(base, temporal)?;
        // Reverse so the first-emitted (innermost) key becomes primary.
        keys.reverse();

        Ok(PhysicalOp::Sort {
            input: Box::new(physical_input),
            keys,
        })
    }

    /// Helper to validate vector index existence
    fn validate_vector_index(&self, property_key: Option<&str>) -> Result<String> {
        let effective_property = property_key
            .unwrap_or(DEFAULT_EMBEDDING_PROPERTY)
            .to_string();

        if !self.storage.has_vector_index(&effective_property) {
            return Err(Error::Query(QueryError::IndexNotFound {
                index_type: "vector".to_string(),
                property_name: effective_property.clone(),
                hint: Some(format!(
                    "Call db.vector_index(\"{}\").hnsw(...).enable() first",
                    effective_property
                )),
            }));
        }
        Ok(effective_property)
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
                if let Some((valid_time, tx_time)) =
                    temporal.as_ref().and_then(|ctx| ctx.as_of_tuple())
                {
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
            } => {
                // Bi-temporal label scans (Issues #550/#551/#552). When the query
                // carries a temporal context the label scan must reconstruct
                // history instead of reading current storage -- otherwise
                // `MATCH (n:Label) AS OF T RETURN n` silently returns present-day
                // data. When no temporal context is present the fast current-state
                // path below is 100% unchanged.
                if let Some(ctx) = temporal.as_ref() {
                    // Point-in-time (`AS OF`). Any single dimension is enough; the
                    // missing dimension resolves to "now" via `resolve_now()`, so
                    // `AS OF VALID_TIME` and `AS OF SYSTEM_TIME` alone both work.
                    if ctx.valid_time_as_of.is_some() || ctx.transaction_time_as_of.is_some() {
                        let (valid_time, transaction_time) = ctx.resolve_now();
                        return Ok(PhysicalOp::TemporalNodeScan {
                            label: label.clone(),
                            valid_time,
                            transaction_time,
                        });
                    }
                    // Valid-time range (`BETWEEN ... AND ...`): every version whose
                    // valid interval overlaps the range, observed at the current
                    // transaction time (or an explicit system-time anchor).
                    if let Some(range) = ctx.valid_time_between {
                        let transaction_time = ctx
                            .transaction_time_as_of
                            .unwrap_or_else(crate::core::temporal::time::now);
                        return Ok(PhysicalOp::TemporalNodeRangeScan {
                            label: label.clone(),
                            valid_from: range.start(),
                            valid_to: range.end(),
                            transaction_time,
                        });
                    }
                    // A transaction-time RANGE (`FOR SYSTEM_TIME BETWEEN ...`,
                    // reachable via the SQL:2011 parser / `QueryBuilder`) has no
                    // label-scan lowering yet. Reject it with a structured error
                    // rather than silently falling through to the current-state
                    // `NodeScan` below (which would return present-day data -- the
                    // exact bug class this PR fixes, for the tx-range dimension).
                    if ctx.transaction_time_between.is_some() {
                        return Err(Error::Query(QueryError::UnsupportedFeature {
                            feature: "transaction-time range scan (FOR SYSTEM_TIME BETWEEN) on a \
                                      label scan is not supported; use AS OF SYSTEM_TIME for a \
                                      point-in-time transaction-time query"
                                .to_string(),
                        }));
                    }
                }
                Ok(PhysicalOp::NodeScan {
                    label: label.clone(),
                    estimated_rows: estimated_rows.unwrap_or(DEFAULT_SCAN_ESTIMATED_ROWS),
                })
            }

            ScanOp::EdgeScan {
                edge_type,
                estimated_rows,
            } => Ok(PhysicalOp::EdgeScan {
                edge_type: edge_type.clone(),
                estimated_rows: estimated_rows.unwrap_or(DEFAULT_SCAN_ESTIMATED_ROWS),
            }),

            ScanOp::VectorSearch {
                embedding,
                k,
                label_filter,
                metric: _,
                property_key,
            } => {
                self.validate_vector_index(property_key.as_deref())?;

                if let Some((_, tx_time)) = temporal.as_ref().and_then(|ctx| ctx.as_of_tuple()) {
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

            ScanOp::TemporalVectorSearch {
                embedding,
                k,
                timestamp,
                property_key,
            } => {
                self.validate_vector_index(property_key.as_deref())?;

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
                self.validate_vector_index(Some(property_key))?;

                Ok(PhysicalOp::SimilarToNode {
                    source_node: *source_node,
                    property_key: property_key.clone(),
                    k: *k,
                    label_filter: label_filter.clone(),
                })
            }

            ScanOp::PropertyScan { label, key, value } => Ok(PhysicalOp::PropertyScan {
                label: label.clone(),
                key: key.clone(),
                value: value.clone(),
                // Property scans are selective - assume a small fraction of label's rows
                estimated_rows: DEFAULT_PROPERTY_SCAN_ROWS,
            }),
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
                let temporal_ctx = temporal.as_ref().and_then(|ctx| ctx.as_of_tuple());
                Ok(PhysicalOp::IndexedTraversal {
                    input: Box::new(input),
                    direction: *direction,
                    label: label.clone(),
                    min_depth: depth.min_depth(),
                    depth: cap_traversal_depth(depth),
                    temporal_context: temporal_ctx,
                })
            }

            UnaryOp::VectorRank {
                embedding,
                top_k,
                property_key,
                metric,
                threshold,
                score_alias,
            } => {
                self.validate_vector_index(property_key.as_deref())?;

                // A pure threshold filter (no explicit top_k) keeps *every*
                // passing row, so leave `k` unbounded rather than clamping to
                // the default top-k; a genuine top_k still bounds the result.
                let k = top_k.unwrap_or(if threshold.is_some() {
                    usize::MAX
                } else {
                    DEFAULT_VECTOR_TOP_K
                });

                Ok(PhysicalOp::VectorRerank {
                    input: Box::new(input),
                    embedding: embedding.clone(),
                    k,
                    property_key: property_key.clone(),
                    metric: *metric,
                    threshold: *threshold,
                    score_alias: score_alias.clone(),
                })
            }

            // A lone Sort (not part of a chain intercepted by the fold in
            // `to_physical_op`) lowers to a single-key multi-key Sort.
            UnaryOp::Sort { key, descending } => Ok(PhysicalOp::Sort {
                input: Box::new(input),
                keys: vec![(key.clone(), *descending)],
            }),

            UnaryOp::Project(props) => Ok(PhysicalOp::Project {
                input: Box::new(input),
                properties: props.clone(),
            }),

            UnaryOp::ProjectProvenance(projection) => Ok(PhysicalOp::ProjectProvenance {
                input: Box::new(input),
                projection: projection.clone(),
            }),

            UnaryOp::Distinct => Ok(PhysicalOp::Distinct {
                input: Box::new(input),
            }),

            UnaryOp::Count => Ok(PhysicalOp::Count {
                input: Box::new(input),
            }),

            UnaryOp::Aggregate {
                group_keys,
                aggregates,
            } => Ok(PhysicalOp::Aggregate {
                input: Box::new(input),
                group_keys: group_keys.clone(),
                aggregates: aggregates.clone(),
            }),

            UnaryOp::TemporalTrack { time_range } => Ok(PhysicalOp::TemporalTrack {
                input: Box::new(input),
                time_range: *time_range,
            }),

            UnaryOp::TemporalWindowAggregate(spec) => Ok(PhysicalOp::TemporalWindowAggregate {
                input: Box::new(input),
                spec: spec.clone(),
            }),

            UnaryOp::TemporalAlign(spec) => Ok(PhysicalOp::TemporalAlign {
                input: Box::new(input),
                spec: spec.clone(),
            }),

            UnaryOp::OptionalApply { steps } => {
                // Mirror UnaryOp::Traverse: extract the temporal context for
                // edge filtering inside optional traversal steps.
                let temporal_ctx = temporal.as_ref().and_then(|ctx| ctx.as_of_tuple());
                let physical_steps = steps
                    .iter()
                    .map(|step| match step {
                        OptionalStep::Scan { label } => physical::OptionalPhysicalStep::Scan {
                            label: label.clone(),
                        },
                        OptionalStep::Traverse {
                            direction,
                            label,
                            depth,
                        } => physical::OptionalPhysicalStep::Traverse {
                            direction: *direction,
                            label: label.clone(),
                            min_depth: depth.min_depth(),
                            depth: cap_traversal_depth(depth),
                            temporal_context: temporal_ctx,
                        },
                        OptionalStep::Filter(predicate) => {
                            physical::OptionalPhysicalStep::Filter(predicate.clone())
                        }
                    })
                    .collect();
                Ok(PhysicalOp::OptionalApply {
                    input: Box::new(input),
                    steps: physical_steps,
                })
            }
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
            .as_of(1000.into(), 2000.into())
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
            .as_of(1000.into(), 2000.into())
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
                metric: crate::core::vector::DistanceMetric::Cosine,
                threshold: None,
                score_alias: None,
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
                time_range: crate::core::temporal::TimeRange::new(now, now).unwrap(),
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
                time_range: crate::core::temporal::TimeRange::new(now, now).unwrap(),
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
        query.temporal_context = Some(TemporalContext::as_of(now, now));

        let plan = planner.plan(query).unwrap();
        // Should be TemporalNodeLookup instead of NodeLookup
        assert!(matches!(plan.root, PhysicalOp::TemporalNodeLookup { .. }));
    }

    #[test]
    fn test_temporal_node_scan_with_context() {
        // Companion to `test_temporal_node_lookup_with_context`: a *label scan*
        // carrying a point-in-time context must lower to `TemporalNodeScan`, not
        // the current-state `NodeScan` (Issues #550/#551) -- otherwise
        // `MATCH (n:Label) AS OF T RETURN n` silently returns present-day data.
        let planner = test_planner();
        let now = crate::core::temporal::time::now();
        let query = Query {
            ops: vec![QueryOp::ScanNodes {
                label: Some("Person".to_string()),
            }],
            temporal_context: Some(TemporalContext::as_of(now, now)),
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        assert!(
            matches!(plan.root, PhysicalOp::TemporalNodeScan { .. }),
            "AS OF label scan must lower to TemporalNodeScan, got {:?}",
            plan.root.name()
        );
    }

    #[test]
    fn test_temporal_node_scan_single_dimension_context() {
        // A single-dimension `AS OF SYSTEM_TIME` (only transaction time set)
        // must still lower to the temporal scan; the missing dimension resolves
        // to "now" (Issue #551).
        let planner = test_planner();
        let now = crate::core::temporal::time::now();
        let query = Query {
            ops: vec![QueryOp::ScanNodes {
                label: Some("Person".to_string()),
            }],
            temporal_context: Some(TemporalContext::as_of_transaction_time(now)),
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::TemporalNodeScan { .. }));
    }

    #[test]
    fn test_temporal_node_range_scan_with_between_context() {
        // A `BETWEEN` valid-time range on a label scan must lower to
        // `TemporalNodeRangeScan` (Issue #552).
        let planner = test_planner();
        let range = crate::core::temporal::TimeRange::new(1_000.into(), 2_000.into()).unwrap();
        let query = Query {
            ops: vec![QueryOp::ScanNodes {
                label: Some("Person".to_string()),
            }],
            temporal_context: Some(TemporalContext::valid_time_between(range)),
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        assert!(
            matches!(plan.root, PhysicalOp::TemporalNodeRangeScan { .. }),
            "BETWEEN label scan must lower to TemporalNodeRangeScan, got {:?}",
            plan.root.name()
        );
    }

    #[test]
    fn test_node_scan_without_temporal_context_unchanged() {
        // Regression: with no temporal context the fast current-state path is
        // untouched -- still a plain `NodeScan`.
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::ScanNodes {
                label: Some("Person".to_string()),
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        assert!(matches!(plan.root, PhysicalOp::NodeScan { .. }));
    }

    #[test]
    fn test_transaction_time_between_label_scan_rejected() {
        // A transaction-time RANGE context on a label scan has no lowering yet;
        // it must be REJECTED with a structured error, never fall through to a
        // current-state NodeScan (which would silently return present-day data).
        let planner = test_planner();
        let range = crate::core::temporal::TimeRange::new(1_000.into(), 2_000.into()).unwrap();
        let query = Query {
            ops: vec![QueryOp::ScanNodes {
                label: Some("Person".to_string()),
            }],
            temporal_context: Some(TemporalContext::transaction_time_between(range)),
            hints: QueryHints::default(),
        };

        let err = planner.plan(query).unwrap_err();
        assert!(
            matches!(err, Error::Query(QueryError::UnsupportedFeature { .. })),
            "transaction_time_between must be an UnsupportedFeature error, got {err:?}"
        );
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
        query.temporal_context = Some(TemporalContext::as_of(now, now));

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
            err_msg.contains("vector_index(\"embedding\").hnsw"),
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
            .as_of(1000.into(), 2000.into())
            .find_similar(&embedding, 10)
            .build();

        let result = planner.plan(query);
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, crate::core::error::Error::Query(_)));
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
            timestamp: 1000.into(),
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
            .as_of(1000.into(), 2000.into())
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
            .as_of(1000.into(), 2000.into())
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
            timestamp: 1000.into(),
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

    #[test]
    fn test_plan_edge_scan() {
        let planner = test_planner();

        let query = Query {
            ops: vec![
                QueryOp::ScanEdges {
                    edge_type: Some("KNOWS".to_string()),
                },
                QueryOp::Limit(10),
            ],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        let explain = plan.explain();
        assert!(
            explain.contains("EdgeScan") || explain.contains("Limit"),
            "Plan should contain EdgeScan: {}",
            explain
        );
    }

    #[test]
    fn test_plan_edge_scan_no_type() {
        let planner = test_planner();

        let query = Query {
            ops: vec![QueryOp::ScanEdges { edge_type: None }, QueryOp::Limit(5)],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        let explain = plan.explain();
        assert!(
            explain.contains("EdgeScan"),
            "Plan should contain EdgeScan: {}",
            explain
        );
    }

    // ==================== OPTIONAL MATCH (OptionalApply) Tests ====================

    /// A non-leading `QueryOp::Optional` plans to a `PhysicalOp::OptionalApply`
    /// whose steps mirror the traverse + filter sub-ops (per-row apply form).
    #[test]
    fn test_optional_apply_planning_after_scan() {
        let planner = test_planner();
        let query = Query {
            ops: vec![
                QueryOp::ScanNodes {
                    label: Some("Person".to_string()),
                },
                QueryOp::Optional {
                    ops: vec![
                        QueryOp::TraverseOut {
                            label: Some("KNOWS".to_string()),
                            depth: TraversalDepth::Exact(1),
                        },
                        QueryOp::Filter(Predicate::eq("name", "Alice")),
                    ],
                },
            ],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::OptionalApply { input, steps } => {
                assert!(matches!(**input, PhysicalOp::NodeScan { .. }));
                assert_eq!(steps.len(), 2);
                match &steps[0] {
                    physical::OptionalPhysicalStep::Traverse {
                        direction,
                        label,
                        min_depth,
                        depth,
                        temporal_context,
                    } => {
                        assert_eq!(*direction, Direction::Outgoing);
                        assert_eq!(label.as_deref(), Some("KNOWS"));
                        assert_eq!(*min_depth, 1);
                        assert_eq!(*depth, 1);
                        assert!(temporal_context.is_none());
                    }
                    other => panic!("expected Traverse step, got {other:?}"),
                }
                assert!(matches!(
                    steps[1],
                    physical::OptionalPhysicalStep::Filter(_)
                ));
            }
            other => panic!("expected OptionalApply root, got {other:?}"),
        }
    }

    /// TraverseIn / TraverseBoth inside an optional map to Incoming / Both,
    /// and unbounded `Variable` depth falls back to the planner default.
    #[test]
    fn test_optional_apply_traverse_directions_and_default_depth() {
        let planner = test_planner();
        let query = Query {
            ops: vec![
                QueryOp::ScanNodes { label: None },
                QueryOp::Optional {
                    ops: vec![
                        QueryOp::TraverseIn {
                            label: None,
                            depth: TraversalDepth::Exact(2),
                        },
                        QueryOp::TraverseBoth {
                            label: None,
                            depth: TraversalDepth::Variable,
                        },
                    ],
                },
            ],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::OptionalApply { steps, .. } => {
                match &steps[0] {
                    physical::OptionalPhysicalStep::Traverse {
                        direction, depth, ..
                    } => {
                        assert_eq!(*direction, Direction::Incoming);
                        assert_eq!(*depth, 2);
                    }
                    other => panic!("expected Traverse step, got {other:?}"),
                }
                match &steps[1] {
                    physical::OptionalPhysicalStep::Traverse {
                        direction, depth, ..
                    } => {
                        assert_eq!(*direction, Direction::Both);
                        assert_eq!(*depth, DEFAULT_MAX_TRAVERSAL_DEPTH);
                    }
                    other => panic!("expected Traverse step, got {other:?}"),
                }
            }
            other => panic!("expected OptionalApply root, got {other:?}"),
        }
    }

    /// A leading `Optional` (source position) plans to OptionalApply over an
    /// Empty input with a Scan first step (standalone form).
    #[test]
    fn test_leading_optional_match_planning() {
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::Optional {
                ops: vec![
                    QueryOp::ScanNodes {
                        label: Some("Person".to_string()),
                    },
                    QueryOp::TraverseOut {
                        label: Some("KNOWS".to_string()),
                        depth: TraversalDepth::Exact(1),
                    },
                ],
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::OptionalApply { input, steps } => {
                assert!(matches!(**input, PhysicalOp::Empty));
                assert_eq!(steps.len(), 2);
                match &steps[0] {
                    physical::OptionalPhysicalStep::Scan { label } => {
                        assert_eq!(label.as_deref(), Some("Person"));
                    }
                    other => panic!("expected Scan step, got {other:?}"),
                }
            }
            other => panic!("expected OptionalApply root, got {other:?}"),
        }
    }

    /// Sub-ops other than scan/traverse/filter are rejected inside an
    /// optional pattern.
    #[test]
    fn test_optional_unsupported_op_rejected() {
        let planner = test_planner();
        let query = Query {
            ops: vec![
                QueryOp::ScanNodes { label: None },
                QueryOp::Optional {
                    ops: vec![QueryOp::Limit(10)],
                },
            ],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let err = planner.plan(query).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported operation inside OPTIONAL MATCH"),
            "unexpected error: {err}"
        );
    }

    /// A leading OPTIONAL MATCH whose first sub-op is not a node scan is
    /// rejected.
    #[test]
    fn test_leading_optional_without_scan_rejected() {
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::Optional {
                ops: vec![QueryOp::TraverseOut {
                    label: None,
                    depth: TraversalDepth::Exact(1),
                }],
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let err = planner.plan(query).unwrap_err();
        assert!(
            err.to_string().contains("must begin with a node scan"),
            "unexpected error: {err}"
        );
    }

    /// A scan is only allowed as the *first* step of a *leading* optional:
    /// in apply position (after a source), it is rejected.
    #[test]
    fn test_optional_scan_rejected_in_apply_position() {
        let planner = test_planner();
        let query = Query {
            ops: vec![
                QueryOp::ScanNodes { label: None },
                QueryOp::Optional {
                    ops: vec![QueryOp::ScanNodes {
                        label: Some("Person".to_string()),
                    }],
                },
            ],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let err = planner.plan(query).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported operation inside OPTIONAL MATCH"),
            "unexpected error: {err}"
        );
    }

    /// Even in a leading optional, a scan after the first step is rejected
    /// (the `i == 0` guard).
    #[test]
    fn test_leading_optional_second_scan_rejected() {
        let planner = test_planner();
        let query = Query {
            ops: vec![QueryOp::Optional {
                ops: vec![
                    QueryOp::ScanNodes {
                        label: Some("Person".to_string()),
                    },
                    QueryOp::ScanNodes {
                        label: Some("Place".to_string()),
                    },
                ],
            }],
            temporal_context: None,
            hints: QueryHints::default(),
        };

        let err = planner.plan(query).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported operation inside OPTIONAL MATCH"),
            "unexpected error: {err}"
        );
    }

    /// The query's AS OF temporal context propagates into optional traverse
    /// steps, mirroring `UnaryOp::Traverse`.
    #[test]
    fn test_optional_traverse_inherits_temporal_context() {
        let planner = test_planner();
        let query = Query {
            ops: vec![
                QueryOp::ScanNodes { label: None },
                QueryOp::Optional {
                    ops: vec![QueryOp::TraverseOut {
                        label: None,
                        depth: TraversalDepth::Exact(1),
                    }],
                },
            ],
            temporal_context: Some(TemporalContext::as_of(1000.into(), 2000.into())),
            hints: QueryHints::default(),
        };

        let plan = planner.plan(query).unwrap();
        match &plan.root {
            PhysicalOp::OptionalApply { steps, .. } => match &steps[0] {
                physical::OptionalPhysicalStep::Traverse {
                    temporal_context, ..
                } => {
                    let (valid, tx) = temporal_context.expect("temporal context must propagate");
                    assert_eq!(valid, 1000.into());
                    assert_eq!(tx, 2000.into());
                }
                other => panic!("expected Traverse step, got {other:?}"),
            },
            other => panic!("expected OptionalApply root, got {other:?}"),
        }
    }
}
