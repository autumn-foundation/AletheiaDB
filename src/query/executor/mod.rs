//! Query Executor
//!
//! Executes physical query plans using a pull-based iterator model.
//! The executor transforms physical operators into iterators that
//! lazily produce results.

pub(crate) mod iterators;
mod profiling;
mod results;

use parking_lot::RwLock;
use std::sync::Arc;

use crate::core::error::Result;
use crate::core::namespace::NamespaceScope;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;

use super::planner::physical::{PhysicalOp, PhysicalPlan};

pub use iterators::EdgeScanIterator;
#[doc(hidden)]
pub use iterators::NodeScanIterator;
pub use iterators::ResultIterator;
#[doc(hidden)]
pub use iterators::ScanStrategy;
pub use iterators::TemporalNodeScanIterator;
pub use iterators::{
    AggregateIterator, BatchTemporalNodeIterator, CountIterator, DistinctIterator, FilterIterator,
    LimitIterator, ProjectIterator, ProvenanceFilterIterator, SortIterator, TemporalNodeIterator,
    TemporalNodeRangeScanIterator, VectorRerankIterator, VectorResultIterator,
};
pub use profiling::{OpProfile, ProfileRegistry, ProfilingIterator};
pub use results::{EntityId, EntityResult, QueryResults, QueryRow};

/// Configuration for query execution.
///
/// # Examples
///
/// ```rust
/// use aletheiadb::query::executor::ExecutionConfig;
///
/// let config = ExecutionConfig {
///     max_buffer_size: 5000,
///     parallel: true,
///     timeout_ms: 1000,
/// };
/// assert_eq!(config.max_buffer_size, 5000);
/// ```
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Maximum number of results to buffer before backpressure is applied.
    /// Default is 10,000.
    pub max_buffer_size: usize,
    /// Enable parallel execution of query operators (where applicable).
    /// Default is false.
    pub parallel: bool,
    /// Execution timeout in milliseconds.
    /// 0 means no timeout. Default is 0.
    pub timeout_ms: u64,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        ExecutionConfig {
            max_buffer_size: 10_000,
            parallel: false,
            timeout_ms: 0,
        }
    }
}

/// Query executor that runs physical plans against storage.
///
/// The executor converts a `PhysicalPlan` into a pipeline of iterators (`ResultIterator`).
/// It manages access to both `CurrentStorage` (for recent data) and `HistoricalStorage`
/// (for temporal data).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use parking_lot::RwLock;
/// use aletheiadb::storage::current::CurrentStorage;
/// use aletheiadb::storage::historical::HistoricalStorage;
/// use aletheiadb::query::{QueryExecutor, PhysicalPlan};
/// use aletheiadb::query::planner::PhysicalOp;
///
/// # fn run() -> Result<(), Box<dyn std::error::Error>> {
/// // 1. Setup storage
/// let current = Arc::new(CurrentStorage::new());
/// let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
///
/// // 2. Create executor
/// let executor = QueryExecutor::new(current, historical);
///
/// // 3. Define a plan (e.g., scan all Person nodes)
/// let plan = PhysicalPlan {
///     root: PhysicalOp::NodeScan {
///         label: Some("Person".to_string()),
///         estimated_rows: 100,
///     },
///     estimated_cost: Default::default(),
///     temporal_context: None,
///     parallel: false,
///     include_provenance: true,
/// };
///
/// // 4. Execute
/// let results = executor.execute(plan)?;
///
/// // 5. Iterate results
/// for row in results {
///     let row = row?;
///     println!("Found entity: {:?}", row.entity);
/// }
/// # Ok(())
/// # }
/// ```
pub struct QueryExecutor {
    /// Reference to current storage
    current: Arc<CurrentStorage>,
    /// Reference to historical storage
    historical: Arc<RwLock<HistoricalStorage>>,
    /// Execution configuration (used for timeout/parallelism in future)
    _config: ExecutionConfig,
    /// Optional namespace scope (Issue #3349, PR2). When set, the executor
    /// filters produced entities to those whose namespace ∈ scope and threads
    /// the boundary into graph traversal. `Arc` so it can be cheaply shared into
    /// the traversal iterator. `None` ⇒ prior namespace-agnostic behavior.
    scope: Option<Arc<NamespaceScope>>,
}

impl QueryExecutor {
    /// Create a new query executor
    pub fn new(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>) -> Self {
        QueryExecutor {
            current,
            historical,
            _config: ExecutionConfig::default(),
            scope: None,
        }
    }

    /// Create an executor with custom configuration
    pub fn with_config(
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        config: ExecutionConfig,
    ) -> Self {
        QueryExecutor {
            current,
            historical,
            _config: config,
            scope: None,
        }
    }

    /// Attach a namespace scope (Issue #3349, PR2). Produced entities are
    /// filtered to those in scope and graph traversal honors the scope boundary
    /// (an out-of-scope edge or node is never crossed). [`NamespaceScope::All`]
    /// is a no-op filter. See [`QueryBuilder::in_namespace`](crate::query::QueryBuilder::in_namespace).
    #[must_use]
    pub fn with_namespace_scope(mut self, scope: NamespaceScope) -> Self {
        self.scope = Some(Arc::new(scope));
        self
    }

    /// Whether the attached scope actually restricts results (i.e. is present and
    /// not [`NamespaceScope::All`]).
    fn scope_is_restricting(&self) -> bool {
        matches!(&self.scope, Some(s) if !matches!(s.as_ref(), NamespaceScope::All))
    }

    /// Execute a physical plan and return results.
    ///
    /// This method recursively transforms the operator tree starting at `plan.root`
    /// into a chain of iterators. The execution is lazy: no data is fetched until
    /// the returned `QueryResults` iterator is consumed.
    ///
    /// # Arguments
    ///
    /// * `plan` - The physical query plan to execute.
    ///
    /// # Returns
    ///
    /// Returns a `QueryResults` iterator that produces `Result<QueryRow>`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use parking_lot::RwLock;
    /// use aletheiadb::storage::current::CurrentStorage;
    /// use aletheiadb::storage::historical::HistoricalStorage;
    /// use aletheiadb::query::{QueryExecutor, PhysicalPlan};
    /// use aletheiadb::query::planner::PhysicalOp;
    ///
    /// // 1. Setup storage and executor
    /// let current = Arc::new(CurrentStorage::new());
    /// let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    /// let executor = QueryExecutor::new(current, historical);
    ///
    /// // 2. Create a physical plan (usually done by planner)
    /// let plan = PhysicalPlan {
    ///     root: PhysicalOp::Empty, // Using Empty for example simplicity
    ///     estimated_cost: Default::default(),
    ///     temporal_context: None,
    ///     parallel: false,
    ///     include_provenance: true,
    /// };
    ///
    /// // 3. Execute
    /// let results = executor.execute(plan).unwrap();
    ///
    /// // 4. Iterate
    /// for row in results {
    ///     println!("Got row: {:?}", row);
    /// }
    /// ```
    pub fn execute(&self, plan: PhysicalPlan) -> Result<QueryResults> {
        let iterator = {
            let bind_edge = plan_needs_edge_binding(&plan.root);
            self.build_op(&plan.root, &mut None, 0, bind_edge)?
        };
        // Namespace scope (Issue #3349, PR2) is enforced at the source operators
        // during `build_op` (see `maybe_scope_source`), so everything downstream --
        // LIMIT/SKIP, ORDER BY, aggregation, COUNT -- already sees only in-scope
        // rows. No outermost post-filter is applied here.
        // Wrap with provenance filter to conditionally strip metadata
        let filtered = Box::new(iterators::ProvenanceFilterIterator::new(
            iterator,
            plan.include_provenance,
        ));
        Ok(QueryResults::new(filtered))
    }

    /// Execute a physical plan with per-operator profiling instrumentation
    /// (Issue #562, the `PROFILE` entry point).
    ///
    /// Builds the same iterator pipeline as [`Self::execute`], but wraps every
    /// operator in a [`ProfilingIterator`] that records the rows it emits and
    /// the cumulative wall-clock time spent in its `next()`. The returned
    /// [`ProfileRegistry`] is ordered in plan-tree **pre-order**, aligning by
    /// index with `PhysicalPlan::explain`'s traversal so the caller can annotate
    /// each plan line with its executed stats.
    ///
    /// Stats are only meaningful **after** the returned [`QueryResults`] stream
    /// is fully drained (an iterator is lazy). The caller
    /// (`AletheiaDB::execute_cypher` for `PROFILE`) drains the stream, then reads
    /// the registry.
    pub fn execute_profiled(&self, plan: &PhysicalPlan) -> Result<(QueryResults, ProfileRegistry)> {
        let mut registry: Option<ProfileRegistry> = Some(Vec::new());
        let iterator = {
            let bind_edge = plan_needs_edge_binding(&plan.root);
            self.build_op(&plan.root, &mut registry, 0, bind_edge)?
        };
        // Scope is applied at the source operators (see `maybe_scope_source`).
        let filtered = Box::new(iterators::ProvenanceFilterIterator::new(
            iterator,
            plan.include_provenance,
        ));
        // `registry` was seeded `Some` above and is never taken, so the unwrap
        // is infallible.
        let registry = registry.unwrap_or_default();
        Ok((QueryResults::new(filtered), registry))
    }

    /// Candidate node ids for a temporal label scan (`AS OF` / `BETWEEN`).
    ///
    /// Enumerates every node that has ever had a version recorded -- the same
    /// candidate set the AS OF node-find oracle
    /// (`AletheiaDB::find_nodes_at_time`) uses, which stays complete for nodes
    /// deleted from current state. The set is capped at the configured
    /// `max_schema_as_of_entities` limit (lowest ids kept) so a pathological
    /// history can't make a single scan unbounded, then sorted for
    /// deterministic, stable output.
    ///
    /// # Truncation caveat
    ///
    /// When recorded history exceeds `max_schema_as_of_entities` (default
    /// 50,000) the candidate set is **truncated** (lowest ids kept, newest
    /// dropped) and the temporal scan returns an *incomplete* result. This
    /// mirrors the oracle's [`NodesAtTime::sampled`] cap. The query-results
    /// envelope does not yet carry a `truncated`/`sampled` flag, so today
    /// truncation is only surfaced via an `observability` `warn!`; wiring the
    /// flag through the executor result is tracked as a follow-up. Callers with
    /// larger histories should raise `max_schema_as_of_entities`.
    fn temporal_scan_candidates(&self) -> Vec<crate::core::NodeId> {
        let historical = self.historical.read();
        let mut ids = historical.versioned_node_ids();
        let cap = historical.max_schema_as_of_entities();
        drop(historical);
        // History exceeds the cap => incomplete result (lowest ids kept).
        // Surface it rather than silently dropping the signal.
        let truncated = crate::db::schema::cap_ids(&mut ids, cap);
        #[cfg(feature = "observability")]
        if truncated {
            tracing::warn!(
                cap,
                kept = ids.len(),
                "temporal label scan candidate set truncated at max_schema_as_of_entities; \
                 result is incomplete (newest node ids dropped)"
            );
        }
        #[cfg(not(feature = "observability"))]
        let _ = truncated;
        ids.sort_unstable();
        ids
    }

    /// Execute a physical operator, returning an iterator.
    ///
    /// Builds the iterator pipeline for `op`. When `profile` is `Some`, every
    /// operator is instrumented: a shared [`OpProfile`] handle is registered
    /// (in plan-tree **pre-order** -- parent before children, so the registry
    /// index aligns with `PhysicalPlan::explain`'s pre-order render) and this
    /// operator's iterator is wrapped in a [`ProfilingIterator`]. When `profile`
    /// is `None` (the ordinary [`Self::execute`] path) no wrapping occurs and
    /// there is no measurable overhead.
    fn build_op(
        &self,
        op: &PhysicalOp,
        profile: &mut Option<ProfileRegistry>,
        depth: usize,
        // Whether the overall physical plan references an edge variable (a
        // `Predicate::EdgeScoped` / `SortKey::EdgeProperty`), computed once at
        // the plan root. When set, `IndexedTraversal` attaches the traversed
        // edge to each row for edge-property WHERE / ORDER BY (Issue #3622).
        bind_edge: bool,
    ) -> Result<Box<dyn ResultIterator>> {
        // Reserve this operator's profile slot *before* building its children so
        // the registry is in pre-order (the closure's mutable borrow of the
        // registry ends before the match reborrows `profile` for recursion).
        let handle = profile.as_mut().map(|registry| {
            let h = Arc::new(OpProfile::new(op.name(), depth));
            registry.push(Arc::clone(&h));
            h
        });

        let child_depth = depth + 1;
        let iter: Box<dyn ResultIterator> =
            match op {
                PhysicalOp::NodeLookup { node_ids } => Box::new(
                    iterators::NodeLookupIterator::new(node_ids.clone(), Arc::clone(&self.current)),
                ),

                PhysicalOp::NodeScan { label, .. } => Box::new(iterators::NodeScanIterator::new(
                    label.clone(),
                    Arc::clone(&self.current),
                )),

                // Full edge scan (SQL `SELECT * FROM edges`). Mirrors `NodeScan`.
                // Yields `EntityResult::Edge` rows; these survive `collect_all` /
                // `count_all` / direct iteration and the edge-shaped structured
                // projection `collect_structured_edges`/`collect_edges` (Issue
                // #3626). The node-centric `collect_structured`/`collect_nodes`
                // helpers remain node-only by design and drop edge rows (see
                // `results.rs`).
                PhysicalOp::EdgeScan { edge_type, .. } => Box::new(
                    iterators::EdgeScanIterator::new(edge_type.clone(), Arc::clone(&self.current)),
                ),

                PhysicalOp::HnswSearch {
                    embedding,
                    k,
                    label_filter,
                    property_key,
                } => self.execute_hnsw_search(
                    embedding,
                    *k,
                    label_filter.as_deref(),
                    property_key.as_deref(),
                )?,

                PhysicalOp::TemporalNodeLookup {
                    node_ids,
                    valid_time,
                    transaction_time,
                    use_batch,
                } => {
                    if *use_batch {
                        // Use batch iterator for large queries (holds lock across all iterations)
                        Box::new(iterators::BatchTemporalNodeIterator::new(
                            node_ids.clone(),
                            *valid_time,
                            *transaction_time,
                            Arc::clone(&self.historical),
                        )?)
                    } else {
                        // Use per-node iterator for small queries (lock per node)
                        Box::new(iterators::TemporalNodeIterator::new(
                            node_ids.clone(),
                            *valid_time,
                            *transaction_time,
                            Arc::clone(&self.historical),
                        ))
                    }
                }

                PhysicalOp::TemporalNodeScan {
                    label,
                    valid_time,
                    transaction_time,
                } => {
                    // Point-in-time label scan (`AS OF`, Issues #550/#551). Enumerate
                    // every ever-versioned node (mirroring the AS OF node-find oracle
                    // `AletheiaDB::find_nodes_at_time`) and reconstruct each at the
                    // requested bi-temporal point, filtering by label; candidates that
                    // did not exist at that instant are skipped, not errors.
                    let node_ids = self.temporal_scan_candidates();
                    Box::new(
                        iterators::TemporalNodeScanIterator::new(
                            node_ids,
                            *valid_time,
                            *transaction_time,
                            Arc::clone(&self.historical),
                            label.clone(),
                        )
                        .skipping_missing(),
                    )
                }

                PhysicalOp::TemporalNodeRangeScan {
                    label,
                    valid_from,
                    valid_to,
                    transaction_time,
                } => {
                    // Valid-time range label scan (`BETWEEN`, Issue #552). Same
                    // candidate enumeration; the iterator emits each node's
                    // believed-at-`transaction_time` version whose valid interval
                    // overlaps the range (at most one row per node -- multiple rows
                    // only across distinct nodes).
                    let node_ids = self.temporal_scan_candidates();
                    Box::new(iterators::TemporalNodeRangeScanIterator::new(
                        node_ids,
                        *valid_from,
                        *valid_to,
                        *transaction_time,
                        Arc::clone(&self.historical),
                        label.clone(),
                    ))
                }

                PhysicalOp::TemporalVectorSearch {
                    embedding,
                    k,
                    timestamp,
                    property_key,
                } => {
                    // Always use the multi-property-aware method with explicit property name.
                    // This ensures correctness in multi-property setups instead of relying on
                    // the default-property resolution (alphabetically first temporal index).
                    let prop = property_key.as_deref().unwrap_or("embedding");
                    let results = self
                        .current
                        .find_similar_as_of_in(prop, embedding, *k, *timestamp)?;

                    Box::new(iterators::VectorResultIterator::new(
                        results,
                        Arc::clone(&self.current),
                    ))
                }

                PhysicalOp::IndexedTraversal {
                    input,
                    direction,
                    label,
                    min_depth,
                    depth: traversal_depth,
                    temporal_context,
                } => {
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    let mut traversal = iterators::TraversalIterator::new(
                        input_iter,
                        *direction,
                        label.clone(),
                        *min_depth,
                        *traversal_depth,
                        Arc::clone(&self.current),
                        Arc::clone(&self.historical),
                        *temporal_context,
                    )
                    .bind_edges(bind_edge);
                    // Namespace boundary (Issue #3349, PR2): when a restricting
                    // scope is attached, the traversal never crosses an
                    // out-of-scope edge nor bridges through an out-of-scope node.
                    if self.scope_is_restricting()
                        && let Some(scope) = &self.scope
                    {
                        traversal = traversal.with_namespace_scope(Arc::clone(scope));
                    }
                    Box::new(traversal)
                }

                PhysicalOp::PropertyScan {
                    label, key, value, ..
                } => Box::new(iterators::PropertyScanIterator::new(
                    label.clone(),
                    key.clone(),
                    value,
                    Arc::clone(&self.current),
                )),

                PhysicalOp::Filter { input, predicate } => {
                    // Real edge-property WHERE (Issue #3622): when the filter's
                    // input stream is rooted at an `EdgeScan` (the SQL `FROM
                    // edges` lane), the stream is pure edges, so property leaves
                    // are evaluated against each edge's own properties instead of
                    // the AQL/Cypher single-entity pass-through. AQL/Cypher never
                    // emit `EdgeScan`, so this stays `false` there.
                    let edge_mode = subtree_yields_edges(input);
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    // Pass historical storage so a `Predicate::Provenance` leaf
                    // (Issue #3354a) can resolve each row entity's write-time
                    // provenance; property-only filters never touch it.
                    Box::new(
                        iterators::FilterIterator::with_historical(
                            input_iter,
                            predicate.clone(),
                            Arc::clone(&self.historical),
                        )
                        .evaluate_edge_properties(edge_mode),
                    )
                }

                PhysicalOp::VectorRerank {
                    input,
                    embedding,
                    k,
                    property_key,
                    metric,
                    threshold,
                    score_alias,
                } => {
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    Box::new(iterators::VectorRerankIterator::with_options(
                        input_iter,
                        embedding.clone(),
                        *k,
                        Arc::clone(&self.current),
                        property_key.clone(),
                        *metric,
                        *threshold,
                        score_alias.clone(),
                    ))
                }

                PhysicalOp::Limit {
                    input,
                    count,
                    offset,
                } => {
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    Box::new(iterators::LimitIterator::new(input_iter, *offset, *count))
                }

                PhysicalOp::Project { input, properties } => {
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    Box::new(iterators::ProjectIterator::new(
                        input_iter,
                        properties.clone(),
                    ))
                }

                PhysicalOp::OptionalApply { input, steps } => {
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    Box::new(iterators::OptionalApplyIterator::new(
                        input_iter,
                        steps.clone(),
                        Arc::clone(&self.current),
                        Arc::clone(&self.historical),
                    ))
                }

                PhysicalOp::Aggregate {
                    input,
                    group_keys,
                    aggregates,
                } => {
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    Box::new(iterators::AggregateIterator::new(
                        input_iter,
                        group_keys.clone(),
                        aggregates.clone(),
                    ))
                }

                PhysicalOp::TemporalWindowAggregate { input, spec } => {
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    // Historical storage is required to reconstruct each matched
                    // entity's valid-time history per window (Issue #3363).
                    Box::new(iterators::TemporalWindowAggregateIterator::new(
                        input_iter,
                        spec.clone(),
                        Arc::clone(&self.historical),
                    ))
                }

                PhysicalOp::TemporalAlign { input, spec } => {
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    // Historical storage is required to reconstruct each matched
                    // participant's valid-time history (and gating edge validity)
                    // at the alignment coordinates (Issue #3379).
                    Box::new(iterators::TemporalJoinIterator::new(
                        input_iter,
                        spec.clone(),
                        Arc::clone(&self.historical),
                    ))
                }

                PhysicalOp::Distinct { input } => {
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    Box::new(iterators::DistinctIterator::new(input_iter))
                }

                PhysicalOp::Sort { input, keys } => {
                    // Real edge-property ORDER BY (Issue #3622): a property sort
                    // key over an `EdgeScan`-rooted (SQL `FROM edges`) stream
                    // reads each edge's own properties instead of resolving to
                    // null. AQL/Cypher never emit `EdgeScan`, so this is `false`
                    // there and node-only sort-key extraction is unchanged.
                    let edge_mode = subtree_yields_edges(input);
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    // Pass historical storage so a `SortKey::Provenance` key
                    // (Issue #3354) can resolve each row entity's write-time
                    // provenance; property/score sorts never touch it.
                    Box::new(
                        iterators::SortIterator::with_historical(
                            input_iter,
                            keys.clone(),
                            Arc::clone(&self.historical),
                        )
                        .order_by_edge_properties(edge_mode),
                    )
                }

                PhysicalOp::ProjectProvenance { input, projection } => {
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    Box::new(iterators::ProvenanceProjectIterator::new(
                        input_iter,
                        projection.clone(),
                        Arc::clone(&self.historical),
                    ))
                }

                PhysicalOp::Count { input } => {
                    let input_iter = self.build_op(input, profile, child_depth, bind_edge)?;
                    Box::new(iterators::CountIterator::new(input_iter))
                }

                PhysicalOp::Empty => Box::new(iterators::EmptyIterator),
                PhysicalOp::SimilarToNode {
                    source_node,
                    property_key,
                    k,
                    label_filter,
                } => self.execute_similar_to_node(
                    *source_node,
                    property_key,
                    *k,
                    label_filter.as_deref(),
                )?,

                // For unsupported operations, return error
                _ => {
                    return Err(crate::core::error::Error::Query(
                        crate::core::error::QueryError::SyntaxError {
                            message: format!("Unsupported physical operator: {:?}", op.name()),
                        },
                    ));
                }
            };

        // Namespace scope (Issue #3349, PR2): push the scope filter down onto the
        // SOURCE operators (scans, lookups, property/vector sources) rather than
        // applying it as an outermost post-filter. Wrapping the leaf that produces
        // entities guarantees every downstream operator -- LIMIT/SKIP, ORDER BY,
        // DISTINCT, aggregation, and COUNT -- sees only in-scope rows, so a scoped
        // `count()` reports the scoped cardinality (not the global one) and a
        // scoped `LIMIT n` returns up to `n` *in-scope* rows (not `n` pre-filter
        // rows of which some are then dropped). Graph traversal is deliberately
        // NOT wrapped here: `IndexedTraversal` enforces the boundary at build time
        // (never bridging an out-of-scope edge/node) and its own input source is
        // wrapped by this same recursion, so its start node is scope-checked
        // (Issue #3349 A4) and its emitted targets are already in-scope.
        let iter = self.maybe_scope_source(op, iter);

        // Instrument this operator when profiling.
        Ok(match handle {
            Some(h) => Box::new(ProfilingIterator::new(iter, h)),
            None => iter,
        })
    }

    /// Wrap a **source-leaf** operator's iterator in the namespace-scope entity
    /// filter (Issue #3349, PR2) when a restricting scope is attached; otherwise
    /// return it unchanged so the namespace-agnostic path pays nothing.
    ///
    /// Only leaf operators that *produce* entities are wrapped (scans, id
    /// lookups, property scans, and vector/temporal sources). Intermediate and
    /// combining operators are never wrapped: their entity rows always originate
    /// at a wrapped source (or at an already-boundary-filtered traversal), so
    /// double-filtering is avoided. See [`is_scoped_source_leaf`].
    fn maybe_scope_source(
        &self,
        op: &PhysicalOp,
        iter: Box<dyn ResultIterator>,
    ) -> Box<dyn ResultIterator> {
        match &self.scope {
            Some(scope) if self.scope_is_restricting() && is_scoped_source_leaf(op) => {
                Box::new(iterators::ScopeFilterIterator::new(
                    iter,
                    Arc::clone(scope),
                    Arc::clone(&self.current),
                ))
            }
            _ => iter,
        }
    }

    fn execute_hnsw_search(
        &self,
        embedding: &[f32],
        k: usize,
        label_filter: Option<&str>,
        property_key: Option<&str>,
    ) -> Result<Box<dyn ResultIterator>> {
        let results = match (property_key, label_filter) {
            // Property-specific with label filter
            (Some(prop), Some(label)) => self
                .current
                .find_similar_by_embedding_in_with_label(prop, embedding, label, k)?,
            // Property-specific without label filter
            (Some(prop), None) => self
                .current
                .find_similar_by_embedding_in(prop, embedding, k)?,
            // Default property with label filter
            (None, Some(label)) => self
                .current
                .find_similar_by_embedding_with_label(embedding, label, k)?,
            // Default property without label filter
            (None, None) => self.current.find_similar_by_embedding(embedding, k)?,
        };

        Ok(Box::new(iterators::VectorResultIterator::new(
            results,
            Arc::clone(&self.current),
        )))
    }

    fn execute_similar_to_node(
        &self,
        source_node: crate::core::NodeId,
        property_key: &str,
        k: usize,
        label_filter: Option<&str>,
    ) -> Result<Box<dyn ResultIterator>> {
        // 1. Validate that property_key matches the indexed property
        let indexed_property = self.current.get_indexed_property_name().ok_or_else(|| {
            crate::core::error::Error::Query(crate::core::error::QueryError::ExecutionError {
                message: "No vector index is enabled. Call db.vector_index(\"...\").hnsw(...).enable() first."
                    .to_string(),
            })
        })?;

        if property_key != indexed_property {
            return Err(crate::core::error::Error::Query(
                crate::core::error::QueryError::ExecutionError {
                    message: format!(
                        "Property key '{}' does not match indexed property '{}'. \
                         Vector index was built on '{}', so similar_to queries must use the same property.",
                        property_key, indexed_property, indexed_property
                    ),
                },
            ));
        }

        // 2. Look up the source node
        let node = self.current.get_node(source_node).map_err(|_| {
            crate::core::error::Error::Query(crate::core::error::QueryError::ExecutionError {
                message: format!("Source node {:?} not found", source_node),
            })
        })?;

        // 3. Extract the embedding from the specified property
        let embedding = node
            .properties
            .get(property_key)
            .and_then(|v: &crate::core::PropertyValue| v.as_vector())
            .ok_or_else(|| {
                crate::core::error::Error::Query(crate::core::error::QueryError::ExecutionError {
                    message: format!(
                        "Node {:?} does not have a vector property '{}'",
                        source_node, property_key
                    ),
                })
            })?;

        // 4. Perform HNSW search with the extracted embedding.
        // Request k+1 results to account for filtering out the source node.
        // Use checked_add to prevent overflow (though k=usize::MAX is extremely unlikely).
        let k_with_source = k.checked_add(1).unwrap_or(k);
        let mut results = if let Some(label) = label_filter {
            self.current
                .find_similar_by_embedding_with_label(embedding, label, k_with_source)?
        } else {
            self.current
                .find_similar_by_embedding(embedding, k_with_source)?
        };

        // Remove source node from results. In vector similarity with cosine distance,
        // a node has similarity 1.0 with itself, so it's always the first result.
        // Filtering it out ensures we return k truly *different* nodes.
        results.retain(|(node_id, _)| node_id != &source_node);

        // Trim to requested k results after filtering
        results.truncate(k);

        Ok(Box::new(iterators::VectorResultIterator::new(
            results,
            Arc::clone(&self.current),
        )))
    }
}

/// Returns `true` when the row stream produced by `op` is composed of edge rows,
/// i.e. the subtree is rooted at an [`PhysicalOp::EdgeScan`] reached through only
/// row-preserving unary operators (Issue #3622).
///
/// Used to decide whether a `Filter`/`Sort` above the subtree should evaluate
/// property predicates / sort keys against the edge's own properties. `EdgeScan`
/// is emitted **only** by the SQL `FROM edges` lane (AQL/Cypher traverse edges
/// via `TraversalIterator`, which yields target *nodes*), so this is a
/// zero-false-positive proxy for "pure edge stream where every bare property key
/// unambiguously refers to the edge". The walked unary ops preserve edge rows
/// unchanged (`ProjectIterator` rewrites only node rows). Any other operator --
/// including binary set ops, aggregation, and node/temporal scans -- is treated
/// as not edge-typed, so the conservative default is the existing pass-through /
/// node-only behavior.
/// Whether `op` is a **source leaf** that produces entity rows directly from
/// storage and therefore must have the namespace scope filter applied to it
/// (Issue #3349, PR2). These are the scans, id lookups, property scans, and
/// vector/temporal sources. Every entity row in a plan originates at one of
/// these leaves (or at a boundary-filtered [`PhysicalOp::IndexedTraversal`],
/// which is handled separately and is intentionally excluded here), so wrapping
/// exactly these leaves pushes the scope filter below LIMIT/SKIP/ORDER BY/COUNT
/// without any double-filtering.
///
/// `Empty` produces no rows, so it is not wrapped. `IndexedTraversal` is
/// excluded (it enforces the boundary at build time and its own input source is
/// wrapped by the recursion). Combining/intermediate operators are excluded
/// because their inputs are already scoped.
fn is_scoped_source_leaf(op: &PhysicalOp) -> bool {
    matches!(
        op,
        PhysicalOp::NodeLookup { .. }
            | PhysicalOp::NodeScan { .. }
            | PhysicalOp::EdgeScan { .. }
            | PhysicalOp::HnswSearch { .. }
            | PhysicalOp::TemporalNodeLookup { .. }
            | PhysicalOp::TemporalVectorSearch { .. }
            | PhysicalOp::TemporalNodeScan { .. }
            | PhysicalOp::TemporalNodeRangeScan { .. }
            | PhysicalOp::SimilarToNode { .. }
            | PhysicalOp::PropertyScan { .. }
    )
}

fn subtree_yields_edges(op: &PhysicalOp) -> bool {
    match op {
        PhysicalOp::EdgeScan { .. } => true,
        PhysicalOp::Filter { input, .. }
        | PhysicalOp::Sort { input, .. }
        | PhysicalOp::Limit { input, .. }
        | PhysicalOp::Project { input, .. }
        | PhysicalOp::Distinct { input, .. }
        | PhysicalOp::VectorRerank { input, .. } => subtree_yields_edges(input),
        _ => false,
    }
}

/// Whether the physical plan references an edge variable -- i.e. contains a
/// [`Predicate::EdgeScoped`] leaf in any `Filter` or a [`SortKey::EdgeProperty`]
/// key in any `Sort` (Issue #3622). Computed once at the plan root so the
/// `IndexedTraversal` iterator only attaches the traversed edge to its rows
/// (an extra reconstruct per row) when an edge-property `WHERE` / `ORDER BY`
/// actually needs it; otherwise traversal behavior is byte-identical.
fn plan_needs_edge_binding(op: &PhysicalOp) -> bool {
    use crate::query::ir::{Predicate, SortKey};

    fn predicate_has_edge_scoped(p: &Predicate) -> bool {
        match p {
            Predicate::EdgeScoped(_) => true,
            Predicate::And(v) | Predicate::Or(v) => v.iter().any(predicate_has_edge_scoped),
            Predicate::Not(inner) => predicate_has_edge_scoped(inner),
            _ => false,
        }
    }

    match op {
        PhysicalOp::Filter { input, predicate } => {
            predicate_has_edge_scoped(predicate) || plan_needs_edge_binding(input)
        }
        PhysicalOp::Sort { input, keys } => {
            keys.iter()
                .any(|(k, _)| matches!(k, SortKey::EdgeProperty(_)))
                || plan_needs_edge_binding(input)
        }
        // Single-input pass-through operators: recurse into the child.
        PhysicalOp::IndexedTraversal { input, .. }
        | PhysicalOp::Limit { input, .. }
        | PhysicalOp::Project { input, .. }
        | PhysicalOp::ProjectProvenance { input, .. }
        | PhysicalOp::Distinct { input, .. }
        | PhysicalOp::Count { input, .. }
        | PhysicalOp::Aggregate { input, .. }
        | PhysicalOp::VectorRerank { input, .. }
        | PhysicalOp::TemporalTrack { input, .. }
        | PhysicalOp::TemporalWindowAggregate { input, .. }
        | PhysicalOp::TemporalAlign { input, .. }
        | PhysicalOp::Materialize { input, .. }
        | PhysicalOp::OptionalApply { input, .. } => plan_needs_edge_binding(input),
        // Binary set operators: either branch may carry the reference.
        PhysicalOp::HashJoin { left, right, .. }
        | PhysicalOp::Union { left, right, .. }
        | PhysicalOp::Intersect { left, right, .. }
        | PhysicalOp::Except { left, right, .. } => {
            plan_needs_edge_binding(left) || plan_needs_edge_binding(right)
        }
        // Leaf sources (scans, similarity source) carry no edge-scoped
        // reference. Every input-bearing operator is enumerated above, so a
        // future single-input op that could sit over a Filter/Sort will fail to
        // compile-match here rather than silently defaulting to `false`
        // (edge-scoped -> all-false / edge sort-key -> null: a silent-wrong
        // hazard). Keep this list exhaustive with the enum.
        PhysicalOp::Empty
        | PhysicalOp::NodeLookup { .. }
        | PhysicalOp::NodeScan { .. }
        | PhysicalOp::EdgeScan { .. }
        | PhysicalOp::HnswSearch { .. }
        | PhysicalOp::TemporalNodeLookup { .. }
        | PhysicalOp::TemporalVectorSearch { .. }
        | PhysicalOp::TemporalNodeScan { .. }
        | PhysicalOp::TemporalNodeRangeScan { .. }
        | PhysicalOp::SimilarToNode { .. }
        | PhysicalOp::PropertyScan { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::NodeId;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::version::AnchorConfig;
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::hnsw::HnswConfig;
    use crate::query::planner::physical::PhysicalOp;

    fn create_test_storage() -> (Arc<CurrentStorage>, Arc<RwLock<HistoricalStorage>>) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
            AnchorConfig::default(),
        )));
        (current, historical)
    }

    fn create_test_storage_with_data() -> (
        Arc<CurrentStorage>,
        Arc<RwLock<HistoricalStorage>>,
        NodeId,
        NodeId,
    ) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
            AnchorConfig::default(),
        )));

        // Enable vector index
        current
            .enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
            .unwrap();

        // Create test nodes
        let alice_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
            .build();
        let alice = current.create_node("Person", alice_props.clone()).unwrap();

        let bob_props = PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert_vector("embedding", &[0.9f32, 0.1, 0.0, 0.0])
            .build();
        let bob = current.create_node("Person", bob_props.clone()).unwrap();

        // Create edge
        current
            .create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Add versions to historical storage (needed for temporal queries)
        use crate::core::temporal::time;
        let now = time::now();
        let alice_label = crate::core::interning::GLOBAL_INTERNER
            .intern("Person")
            .unwrap();
        let bob_label = alice_label;
        {
            let mut hist = historical.write();
            hist.add_node_version(
                alice,
                crate::core::id::VersionId::new(1).unwrap(),
                now,
                now,
                alice_label,
                alice_props,
                false, // not a tombstone
            )
            .unwrap();
            hist.add_node_version(
                bob,
                crate::core::id::VersionId::new(2).unwrap(),
                now,
                now,
                bob_label,
                bob_props,
                false, // not a tombstone
            )
            .unwrap();
        }

        (current, historical, alice, bob)
    }

    #[test]
    fn test_execution_config_default() {
        let config = ExecutionConfig::default();

        assert_eq!(config.max_buffer_size, 10_000);
        assert!(!config.parallel);
        assert_eq!(config.timeout_ms, 0);
    }

    #[test]
    fn test_execution_config_custom() {
        let config = ExecutionConfig {
            max_buffer_size: 1000,
            parallel: true,
            timeout_ms: 5000,
        };

        assert_eq!(config.max_buffer_size, 1000);
        assert!(config.parallel);
        assert_eq!(config.timeout_ms, 5000);
    }

    #[test]
    fn test_executor_new() {
        let (current, historical) = create_test_storage();
        let executor = QueryExecutor::new(current, historical);

        // Just verify it was created
        assert_eq!(executor._config.max_buffer_size, 10_000);
    }

    #[test]
    fn test_executor_with_config() {
        let (current, historical) = create_test_storage();
        let config = ExecutionConfig {
            max_buffer_size: 500,
            parallel: true,
            timeout_ms: 1000,
        };
        let executor = QueryExecutor::with_config(current, historical, config);

        assert_eq!(executor._config.max_buffer_size, 500);
        assert!(executor._config.parallel);
    }

    #[test]
    fn test_execute_node_lookup() {
        let (current, historical, alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::NodeLookup {
                node_ids: vec![alice],
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.node_id(), Some(alice));
    }

    #[test]
    fn test_execute_node_scan() {
        let (current, historical, _alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: 100,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 2); // Alice and Bob
    }

    #[test]
    fn test_execute_hnsw_search() {
        let (current, historical, _alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::HnswSearch {
                embedding: vec![1.0f32, 0.0, 0.0, 0.0].into(),
                k: 2,
                label_filter: None,
                property_key: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn test_execute_hnsw_search_with_label() {
        let (current, historical, _alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::HnswSearch {
                embedding: vec![1.0f32, 0.0, 0.0, 0.0].into(),
                k: 2,
                label_filter: Some("Person".to_string()),
                property_key: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn test_execute_indexed_traversal() {
        let (current, historical, alice, bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::IndexedTraversal {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![alice],
                }),
                direction: crate::query::ir::Direction::Outgoing,
                label: Some("KNOWS".to_string()),
                min_depth: 1,
                depth: 1,
                temporal_context: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.node_id(), Some(bob));
    }

    #[test]
    fn test_execute_filter() {
        let (current, historical, alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::Filter {
                input: Box::new(PhysicalOp::NodeScan {
                    label: Some("Person".to_string()),
                    estimated_rows: 100,
                }),
                predicate: crate::query::Predicate::eq("name", "Alice"),
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.node_id(), Some(alice));
    }

    #[test]
    fn test_execute_vector_rerank() {
        let (current, historical, alice, bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::VectorRerank {
                input: Box::new(PhysicalOp::NodeScan {
                    label: Some("Person".to_string()),
                    estimated_rows: 100,
                }),
                embedding: vec![1.0f32, 0.0, 0.0, 0.0].into(),
                k: 2,
                property_key: None,
                metric: crate::core::vector::DistanceMetric::Cosine,
                threshold: None,
                score_alias: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 2);
        // Alice should be first (exact match)
        assert_eq!(rows[0].entity.node_id(), Some(alice));
        assert_eq!(rows[1].entity.node_id(), Some(bob));
    }

    #[test]
    fn test_execute_limit() {
        let (current, historical, _alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::Limit {
                input: Box::new(PhysicalOp::NodeScan {
                    label: Some("Person".to_string()),
                    estimated_rows: 100,
                }),
                count: 1,
                offset: 0,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn test_execute_limit_with_offset() {
        let (current, historical, _alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::Limit {
                input: Box::new(PhysicalOp::NodeScan {
                    label: Some("Person".to_string()),
                    estimated_rows: 100,
                }),
                count: 10,
                offset: 1,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1); // Skipped 1, only 1 remaining
    }

    #[test]
    fn test_execute_empty() {
        let (current, historical) = create_test_storage();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::Empty,
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert!(rows.is_empty());
    }

    #[test]
    fn test_subtree_yields_edges_detects_edge_rooted_streams() {
        use crate::query::ir::{Predicate, SortKey};
        // Bare EdgeScan is an edge stream.
        let edge_scan = PhysicalOp::EdgeScan {
            edge_type: None,
            estimated_rows: 0,
        };
        assert!(subtree_yields_edges(&edge_scan));

        // Filter/Sort/Limit over an EdgeScan preserve the edge typing.
        let filtered = PhysicalOp::Filter {
            input: Box::new(PhysicalOp::EdgeScan {
                edge_type: None,
                estimated_rows: 0,
            }),
            predicate: Predicate::True,
        };
        let sorted_over_filter = PhysicalOp::Sort {
            input: Box::new(filtered),
            keys: vec![(SortKey::Property("since".to_string()), true)],
        };
        assert!(subtree_yields_edges(&sorted_over_filter));

        // A NodeScan-rooted stream (the AQL/Cypher shape) is NOT edge-typed.
        let node_scan = PhysicalOp::NodeScan {
            label: None,
            estimated_rows: 0,
        };
        assert!(!subtree_yields_edges(&node_scan));
        let filter_over_nodes = PhysicalOp::Filter {
            input: Box::new(PhysicalOp::NodeScan {
                label: None,
                estimated_rows: 0,
            }),
            predicate: Predicate::True,
        };
        assert!(!subtree_yields_edges(&filter_over_nodes));
    }

    #[test]
    fn test_plan_needs_edge_binding_detects_edge_var_references() {
        use crate::query::ir::{Direction, Predicate, PredicateValue, SortKey};

        let traversal = |input: PhysicalOp| PhysicalOp::IndexedTraversal {
            input: Box::new(input),
            direction: Direction::Outgoing,
            label: Some("KNOWS".to_string()),
            min_depth: 1,
            depth: 1,
            temporal_context: None,
        };
        let node_scan = || PhysicalOp::NodeScan {
            label: Some("Person".to_string()),
            estimated_rows: 0,
        };

        // Filter carrying an `EdgeScoped` leaf above a traversal -> needs binding.
        let edge_where = PhysicalOp::Filter {
            input: Box::new(traversal(node_scan())),
            predicate: Predicate::EdgeScoped(Box::new(Predicate::Gt {
                key: "since".to_string(),
                value: PredicateValue::Int(2020),
            })),
        };
        assert!(plan_needs_edge_binding(&edge_where));

        // Sort with an `EdgeProperty` key (even nested under a Project) -> needs binding.
        let edge_order = PhysicalOp::Sort {
            input: Box::new(PhysicalOp::Project {
                input: Box::new(traversal(node_scan())),
                properties: vec!["name".to_string()],
            }),
            keys: vec![(SortKey::EdgeProperty("since".to_string()), false)],
        };
        assert!(plan_needs_edge_binding(&edge_order));

        // A node-only WHERE + ORDER BY over the same traversal -> no binding.
        let node_only = PhysicalOp::Sort {
            input: Box::new(PhysicalOp::Filter {
                input: Box::new(traversal(node_scan())),
                predicate: Predicate::Gt {
                    key: "age".to_string(),
                    value: PredicateValue::Int(18),
                },
            }),
            keys: vec![(SortKey::Property("age".to_string()), false)],
        };
        assert!(!plan_needs_edge_binding(&node_only));

        // A bare traversal references no edge var.
        assert!(!plan_needs_edge_binding(&traversal(node_scan())));
    }

    /// Planner-invariant guard (Issue #3622 review fix): every `Filter`/`Sort`
    /// node in the physical plan for a SQL `FROM edges WHERE ... ORDER BY ...`
    /// query must root an `EdgeScan` stream (through only row-preserving unary
    /// ops), so `edge_mode` engages and edge properties are actually evaluated.
    ///
    /// If a future optimization inserts a non-whitelisted physical op between a
    /// `Filter`/`Sort` and its rooting `EdgeScan`, `subtree_yields_edges` would
    /// return `false`, `edge_mode` would silently turn off, and the lane would
    /// return ALL edges (pass-through) instead of the filtered/sorted set. This
    /// test fails LOUDLY in CI rather than letting that regress silently.
    #[cfg(feature = "sql")]
    #[test]
    fn sql_edge_plan_filter_sort_subtrees_stay_edge_typed() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::sql::parse_sql;

        // Walk the plan, asserting Filter/Sort subtrees are edge-typed and
        // counting them so the assertion is not vacuous.
        fn walk(op: &PhysicalOp, filters: &mut usize, sorts: &mut usize) {
            match op {
                PhysicalOp::Filter { input, .. } => {
                    *filters += 1;
                    assert!(
                        subtree_yields_edges(input),
                        "Filter over SQL `FROM edges` must root an edge stream; \
                         a non-whitelisted op broke the invariant: {input:?}"
                    );
                }
                PhysicalOp::Sort { input, .. } => {
                    *sorts += 1;
                    assert!(
                        subtree_yields_edges(input),
                        "Sort over SQL `FROM edges` must root an edge stream; \
                         a non-whitelisted op broke the invariant: {input:?}"
                    );
                }
                _ => {}
            }
            match op {
                PhysicalOp::Filter { input, .. }
                | PhysicalOp::Sort { input, .. }
                | PhysicalOp::Limit { input, .. }
                | PhysicalOp::Project { input, .. }
                | PhysicalOp::Distinct { input, .. }
                | PhysicalOp::VectorRerank { input, .. } => walk(input, filters, sorts),
                _ => {}
            }
        }

        let query = parse_sql("SELECT * FROM edges WHERE since > 2020 ORDER BY since DESC LIMIT 5")
            .expect("parse edge SQL");
        let planner = QueryPlanner::new(
            Arc::new(Statistics::default()),
            Arc::new(CurrentStorage::new()),
        );
        let plan = planner.plan(query).expect("plan edge SQL");

        let (mut filters, mut sorts) = (0usize, 0usize);
        walk(&plan.root, &mut filters, &mut sorts);
        assert!(
            filters >= 1 && sorts >= 1,
            "the edge WHERE+ORDER BY plan must contain a Filter and a Sort \
             (found {filters} filters, {sorts} sorts) -- otherwise the guard is vacuous"
        );
    }

    /// AQL/Cypher edge-touching regression (Issue #3622 review fix): an AQL
    /// traversal over `KNOWS` edges (a) returns the correct target nodes AND
    /// (b) never lowers to `QueryOp::ScanEdges` -- the ONLY logical op that
    /// becomes the physical `EdgeScan` that `subtree_yields_edges` keys
    /// `edge_mode` on. So `edge_mode` can never engage for AQL/Cypher and
    /// behavior is identical to trunk. This is the executable end-to-end
    /// counterpart to the synthetic `subtree_yields_edges` unit assertion.
    #[test]
    fn aql_edge_traversal_never_enters_edge_mode() {
        use crate::core::property::PropertyValue;
        use crate::query::ir::QueryOp;
        use crate::query::parse_query;

        let db = crate::AletheiaDB::new().expect("create db");
        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .expect("alice");
        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .expect("bob");
        let carol = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Carol").build(),
            )
            .expect("carol");
        db.create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020).build(),
        )
        .expect("edge alice->bob");
        db.create_edge(
            bob,
            carol,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2021).build(),
        )
        .expect("edge bob->carol");

        let query =
            parse_query("MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN b").expect("parse AQL");
        // (b) No `ScanEdges` => the physical plan has no `EdgeScan` => `edge_mode`
        // stays off. If AQL/Cypher ever started sharing the SQL edge scan, this
        // fails loudly before any silent semantic drift.
        assert!(
            !query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::ScanEdges { .. })),
            "AQL traversal must not emit ScanEdges (would wrongly enable edge_mode)"
        );

        // (a) Correct target nodes end-to-end (Alice->Bob, Bob->Carol).
        let rows = db
            .execute_query(query)
            .expect("execute AQL")
            .collect_all()
            .expect("collect rows");
        let mut names: Vec<String> = rows
            .iter()
            .filter_map(|r| r.entity.as_node())
            .filter_map(|n| match n.get_property("name") {
                Some(PropertyValue::String(s)) => Some(s.to_string()),
                _ => None,
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["Bob".to_string(), "Carol".to_string()],
            "AQL KNOWS traversal must return the correct target nodes (trunk behavior)"
        );
    }

    #[test]
    fn test_execute_temporal_node_lookup() {
        let (current, historical, alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let now = crate::core::temporal::time::now();
        let plan = PhysicalPlan {
            root: PhysicalOp::TemporalNodeLookup {
                node_ids: vec![alice],
                valid_time: now,
                transaction_time: now,
                use_batch: false,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        // This will return empty since there's no historical data yet
        // but it exercises the code path
        let results = executor.execute(plan).expect("Execution failed");
        let _rows: Vec<_> = results.collect_all().expect("Collection failed");
        // Result may be empty or have current data depending on implementation
    }

    #[test]
    fn test_execute_nested_operations() {
        let (current, historical, alice, bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        // Traverse -> Filter -> Limit
        let plan = PhysicalPlan {
            root: PhysicalOp::Limit {
                input: Box::new(PhysicalOp::Filter {
                    input: Box::new(PhysicalOp::IndexedTraversal {
                        input: Box::new(PhysicalOp::NodeLookup {
                            node_ids: vec![alice],
                        }),
                        direction: crate::query::ir::Direction::Outgoing,
                        label: Some("KNOWS".to_string()),
                        min_depth: 1,
                        depth: 1,
                        temporal_context: None,
                    }),
                    predicate: crate::query::Predicate::eq("name", "Bob"),
                }),
                count: 10,
                offset: 0,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.node_id(), Some(bob));
    }

    // ==================== Profiling (PROFILE) Tests ====================

    #[test]
    fn test_execute_profiled_records_per_operator_rows() {
        let (current, historical, alice, bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        // Limit (count well above input) over a NodeLookup of two ids, so both
        // operators forward both rows.
        let plan = PhysicalPlan {
            root: PhysicalOp::Limit {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![alice, bob],
                }),
                count: 100,
                offset: 0,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let (results, registry) = executor
            .execute_profiled(&plan)
            .expect("Profiled execution failed");

        // The registry is seeded in plan-tree pre-order: [Limit, NodeLookup].
        assert_eq!(registry.len(), 2);
        assert_eq!(registry[0].op_name(), "Limit");
        assert_eq!(registry[0].depth(), 0);
        assert_eq!(registry[1].op_name(), "NodeLookup");
        assert_eq!(registry[1].depth(), 1);

        // Stats are only meaningful after the (lazy) stream is drained.
        assert_eq!(
            registry[0].actual_rows(),
            0,
            "no rows counted before draining"
        );

        let rows: Vec<_> = results.collect_all().expect("Collection failed");
        assert_eq!(rows.len(), 2);

        // Both operators emitted the two rows.
        assert_eq!(registry[1].actual_rows(), 2, "NodeLookup emitted 2 rows");
        assert_eq!(registry[0].actual_rows(), 2, "Limit forwarded 2 rows");
    }

    #[test]
    fn test_execute_profiled_annotations_align_with_explain() {
        let (current, historical, alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::Limit {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![alice],
                }),
                count: 10,
                offset: 0,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let (results, registry) = executor
            .execute_profiled(&plan)
            .expect("Profiled execution failed");
        let _ = results.collect_all().expect("Collection failed");

        // Feed the per-operator annotations back into the plan renderer and
        // confirm each operator line carries its own stats, in order.
        let annotations: Vec<String> = registry.iter().map(|p| p.annotation()).collect();
        let explained = plan.explain_annotated(&annotations);
        let lines: Vec<&str> = explained.lines().collect();
        // Header + 2 operator lines.
        assert_eq!(lines.len(), 3);
        assert!(lines[1].contains("Limit"));
        assert!(lines[1].contains("actual rows: 1"));
        assert!(lines[2].contains("NodeLookup"));
        assert!(lines[2].contains("actual rows: 1"));
    }

    // ==================== SimilarTo Tests ====================

    #[test]
    fn test_similar_to_node_execution() {
        use crate::core::PropertyMapBuilder;
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let (storage, historical) = create_test_storage();
        let executor = QueryExecutor::new(Arc::clone(&storage), historical);

        // Enable vector index first
        storage
            .enable_vector_index("embedding", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        // Create test nodes with embeddings
        let embedding1 = vec![1.0, 0.0, 0.0];
        let embedding2 = vec![0.9, 0.1, 0.0]; // Similar to embedding1
        let embedding3 = vec![0.0, 1.0, 0.0]; // Different from embedding1

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert("title", "Doc1")
                    .insert_vector("embedding", &embedding1)
                    .build(),
            )
            .unwrap();

        let node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert("title", "Doc2")
                    .insert_vector("embedding", &embedding2)
                    .build(),
            )
            .unwrap();

        let _node3 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert("title", "Doc3")
                    .insert_vector("embedding", &embedding3)
                    .build(),
            )
            .unwrap();

        // Execute SimilarTo query
        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: node1,
                property_key: "embedding".to_string(),
                k: 5,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        // Should find node2 as most similar (excluding node1 itself)
        assert!(!rows.is_empty());
        assert_eq!(rows[0].entity.node_id(), Some(node2));
    }

    #[test]
    fn test_similar_to_with_label_filter() {
        use crate::core::PropertyMapBuilder;
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let (storage, historical) = create_test_storage();
        let executor = QueryExecutor::new(Arc::clone(&storage), historical);

        // Enable vector index first
        storage
            .enable_vector_index("embedding", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        let embedding = vec![1.0, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();

        // Create similar node with different label
        let _node2 = storage
            .create_node(
                "Other",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.9, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        // Create similar node with same label
        let node3 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.95, 0.05, 0.0])
                    .build(),
            )
            .unwrap();

        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: node1,
                property_key: "embedding".to_string(),
                k: 5,
                label_filter: Some("Doc".to_string()),
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        // Should only find node3 (Doc label), not node2 (Other label)
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.node_id(), Some(node3));
    }

    #[test]
    fn test_similar_to_source_node_not_found() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let (storage, historical) = create_test_storage();

        // Enable vector index so we can test the "node not found" error
        storage
            .enable_vector_index("embedding", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        let executor = QueryExecutor::new(storage, historical);

        let nonexistent_node = NodeId::new(9999).unwrap();

        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: nonexistent_node,
                property_key: "embedding".to_string(),
                k: 5,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let result = executor.execute(plan);
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("Source node"));
        }
    }

    #[test]
    fn test_similar_to_missing_vector_property() {
        use crate::core::PropertyMapBuilder;
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let (storage, historical) = create_test_storage();
        let executor = QueryExecutor::new(Arc::clone(&storage), historical);

        // Enable vector index so we can test the "missing property" error
        storage
            .enable_vector_index("embedding", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        // Create node without vector property
        let node = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert("title", "No embedding")
                    .build(),
            )
            .unwrap();

        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: node,
                property_key: "embedding".to_string(),
                k: 5,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let result = executor.execute(plan);
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("does not have a vector property"));
        }
    }

    #[test]
    fn test_similar_to_custom_property_key() {
        use crate::core::PropertyMapBuilder;
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let (storage, historical) = create_test_storage();
        let executor = QueryExecutor::new(Arc::clone(&storage), historical);

        // Enable vector index on custom property first
        storage
            .enable_vector_index("custom_vector", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        let embedding1 = vec![1.0, 0.0, 0.0];
        let embedding2 = vec![0.9, 0.1, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("custom_vector", &embedding1)
                    .build(),
            )
            .unwrap();

        let node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("custom_vector", &embedding2)
                    .build(),
            )
            .unwrap();

        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: node1,
                property_key: "custom_vector".to_string(),
                k: 5,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert!(!rows.is_empty());
        assert_eq!(rows[0].entity.node_id(), Some(node2));
    }

    #[test]
    fn test_similar_to_no_vector_index() {
        use crate::core::PropertyMapBuilder;

        let (storage, historical) = create_test_storage();
        let executor = QueryExecutor::new(Arc::clone(&storage), historical);

        let embedding = vec![1.0, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();

        // NOTE: Vector index NOT enabled for "embedding" property

        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: node1,
                property_key: "embedding".to_string(),
                k: 5,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let result = executor.execute(plan);
        assert!(
            result.is_err(),
            "Should return error when vector index is not enabled"
        );

        // Verify error message indicates index not found
        if let Err(e) = result {
            let error_msg = format!("{}", e);
            assert!(
                error_msg.contains("index") || error_msg.contains("Index"),
                "Error should mention missing index: {}",
                error_msg
            );
        }
    }

    #[test]
    fn test_similar_to_fewer_results_than_k() {
        use crate::core::PropertyMapBuilder;
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let (storage, historical) = create_test_storage();
        let executor = QueryExecutor::new(Arc::clone(&storage), historical);

        // Enable vector index
        storage
            .enable_vector_index("embedding", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        // Create only 3 nodes total
        let embedding1 = vec![1.0, 0.0, 0.0];
        let embedding2 = vec![0.9, 0.1, 0.0];
        let embedding3 = vec![0.8, 0.2, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &embedding1)
                    .build(),
            )
            .unwrap();

        let _node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &embedding2)
                    .build(),
            )
            .unwrap();

        let _node3 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &embedding3)
                    .build(),
            )
            .unwrap();

        // Request k=10 similar nodes, but only 2 exist (3 total - 1 source)
        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: node1,
                property_key: "embedding".to_string(),
                k: 10,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        // Should return only 2 results (3 nodes - source node), not 10
        assert_eq!(
            rows.len(),
            2,
            "Should return only 2 results when database has fewer nodes than k"
        );
    }

    // ==================== Multi-Property Vector Index Tests ====================

    /// Helper to create storage with multiple vector properties
    fn create_multi_property_vector_storage() -> (
        Arc<CurrentStorage>,
        Arc<RwLock<HistoricalStorage>>,
        NodeId,
        NodeId,
    ) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
            AnchorConfig::default(),
        )));

        // Enable TWO different vector indexes with different dimensions
        current
            .enable_vector_index(
                "title_embedding",
                HnswConfig::new(4, DistanceMetric::Cosine),
            )
            .expect("Should enable first vector index");
        current
            .enable_vector_index(
                "content_embedding",
                HnswConfig::new(8, DistanceMetric::Cosine),
            )
            .expect("Should enable second vector index");

        // Create test nodes with DIFFERENT embeddings for each property
        // Node 1: title_embedding is similar to [1,0,0,0], content_embedding is different
        let doc1_props = PropertyMapBuilder::new()
            .insert("title", "Rust Programming")
            .insert_vector("title_embedding", &[1.0f32, 0.0, 0.0, 0.0])
            .insert_vector(
                "content_embedding",
                &[0.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
            )
            .build();
        let doc1 = current.create_node("Document", doc1_props).unwrap();

        // Node 2: title_embedding is different, content_embedding is similar to [0,0,0,0,1,0,0,0]
        let doc2_props = PropertyMapBuilder::new()
            .insert("title", "Python Basics")
            .insert_vector("title_embedding", &[0.0f32, 1.0, 0.0, 0.0])
            .insert_vector(
                "content_embedding",
                &[0.0f32, 0.0, 0.0, 0.0, 0.9, 0.1, 0.0, 0.0],
            )
            .build();
        let doc2 = current.create_node("Document", doc2_props).unwrap();

        (current, historical, doc1, doc2)
    }

    /// Test that HnswSearch uses property_key to query the correct index.
    ///
    /// This test verifies that when querying "title_embedding" vs "content_embedding",
    /// we get different results because the embeddings are different.
    #[test]
    fn test_hnsw_search_multi_property() {
        let (current, historical, doc1, _doc2) = create_multi_property_vector_storage();
        let executor = QueryExecutor::new(current, historical);

        // Query title_embedding with vector similar to doc1's title_embedding
        let plan = PhysicalPlan {
            root: PhysicalOp::HnswSearch {
                embedding: vec![1.0f32, 0.0, 0.0, 0.0].into(),
                k: 1,
                label_filter: None,
                property_key: Some("title_embedding".to_string()),
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        // The top result should be doc1 because its title_embedding is [1,0,0,0]
        match &rows[0].entity {
            EntityResult::NodeId(id) => {
                assert_eq!(*id, doc1, "Should return doc1 for title_embedding query")
            }
            EntityResult::Node(node) => assert_eq!(
                node.id, doc1,
                "Should return doc1 for title_embedding query"
            ),
            _ => panic!("Expected Node or NodeId result"),
        }
    }

    /// Test HnswSearch with both property_key and label_filter.
    /// This covers the code path where both are Some.
    #[test]
    fn test_hnsw_search_multi_property_with_label() {
        let (current, historical, doc1, _doc2) = create_multi_property_vector_storage();
        let executor = QueryExecutor::new(current, historical);

        // Query title_embedding with vector similar to doc1's title_embedding, filtered by label "Document"
        let plan = PhysicalPlan {
            root: PhysicalOp::HnswSearch {
                embedding: vec![1.0f32, 0.0, 0.0, 0.0].into(),
                k: 1,
                label_filter: Some("Document".to_string()),
                property_key: Some("title_embedding".to_string()),
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        // The top result should be doc1
        match &rows[0].entity {
            EntityResult::NodeId(id) => {
                assert_eq!(*id, doc1, "Should return doc1 for title_embedding query")
            }
            EntityResult::Node(node) => assert_eq!(
                node.id, doc1,
                "Should return doc1 for title_embedding query"
            ),
            _ => panic!("Expected Node or NodeId result"),
        }

        // Test with a non-matching label
        let plan_no_match = PhysicalPlan {
            root: PhysicalOp::HnswSearch {
                embedding: vec![1.0f32, 0.0, 0.0, 0.0].into(),
                k: 1,
                label_filter: Some("NonExistentLabel".to_string()),
                property_key: Some("title_embedding".to_string()),
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan_no_match).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");
        assert!(rows.is_empty());
    }

    /// Test that VectorRerank uses property_key to rerank by the correct property.
    #[test]
    fn test_vector_rerank_multi_property() {
        let (current, historical, doc1, doc2) = create_multi_property_vector_storage();
        let executor = QueryExecutor::new(current, historical);

        // Start with both nodes, rerank by content_embedding similarity
        // Query embedding is similar to doc2's content_embedding
        let plan = PhysicalPlan {
            root: PhysicalOp::VectorRerank {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![doc1, doc2],
                }),
                embedding: vec![0.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0].into(),
                k: 2,
                property_key: Some("content_embedding".to_string()),
                metric: crate::core::vector::DistanceMetric::Cosine,
                threshold: None,
                score_alias: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 2);
        // doc1 should be first because its content_embedding [0,0,0,0,1,0,0,0] is most similar
        match &rows[0].entity {
            EntityResult::NodeId(id) => {
                assert_eq!(*id, doc1, "doc1 should rank first by content_embedding")
            }
            EntityResult::Node(node) => {
                assert_eq!(node.id, doc1, "doc1 should rank first by content_embedding")
            }
            _ => panic!("Expected Node or NodeId result"),
        }
    }

    // ==================== OptionalApply Tests ====================

    #[test]
    fn test_execute_optional_apply_matched() {
        use crate::query::planner::physical::OptionalPhysicalStep;

        let (current, historical, alice, bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        // Alice -KNOWS-> Bob exists: the optional traversal matches.
        let plan = PhysicalPlan {
            root: PhysicalOp::OptionalApply {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![alice],
                }),
                steps: vec![OptionalPhysicalStep::Traverse {
                    direction: crate::query::ir::Direction::Outgoing,
                    label: Some("KNOWS".to_string()),
                    min_depth: 1,
                    depth: 1,
                    temporal_context: None,
                }],
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.node_id(), Some(bob));
    }

    #[test]
    fn test_execute_optional_apply_unmatched_yields_null_row() {
        use crate::query::planner::physical::OptionalPhysicalStep;

        let (current, historical, alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        // No FOLLOWS edges exist: left-outer semantics require one null row
        // per input row, not zero rows.
        let plan = PhysicalPlan {
            root: PhysicalOp::OptionalApply {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![alice],
                }),
                steps: vec![OptionalPhysicalStep::Traverse {
                    direction: crate::query::ir::Direction::Outgoing,
                    label: Some("FOLLOWS".to_string()),
                    min_depth: 1,
                    depth: 1,
                    temporal_context: None,
                }],
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert!(rows[0].entity.is_null());
    }

    #[test]
    fn test_execute_optional_apply_filter_scoped_inside() {
        use crate::query::planner::physical::OptionalPhysicalStep;

        let (current, historical, alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        // The traversal matches Bob, but the filter inside the optional
        // segment eliminates him: the row survives as null.
        let plan = PhysicalPlan {
            root: PhysicalOp::OptionalApply {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![alice],
                }),
                steps: vec![
                    OptionalPhysicalStep::Traverse {
                        direction: crate::query::ir::Direction::Outgoing,
                        label: Some("KNOWS".to_string()),
                        min_depth: 1,
                        depth: 1,
                        temporal_context: None,
                    },
                    OptionalPhysicalStep::Filter(crate::query::Predicate::eq("name", "Zed")),
                ],
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert!(rows[0].entity.is_null());
    }

    #[test]
    fn test_execute_optional_apply_standalone_scan() {
        use crate::query::planner::physical::OptionalPhysicalStep;

        let (current, historical) = create_test_storage();
        let executor = QueryExecutor::new(current, historical);

        // Standalone (leading OPTIONAL MATCH) over an empty store: one null row.
        let plan = PhysicalPlan {
            root: PhysicalOp::OptionalApply {
                input: Box::new(PhysicalOp::Empty),
                steps: vec![OptionalPhysicalStep::Scan {
                    label: Some("Person".to_string()),
                }],
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert!(rows[0].entity.is_null());
    }

    #[test]
    fn test_execute_project() {
        let (current, historical, alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::Project {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![alice],
                }),
                properties: vec!["name".to_string()],
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        let node = rows[0].entity.as_node().unwrap();
        assert_eq!(
            node.properties.get("name").unwrap().as_str().unwrap(),
            "Alice"
        );
        // Embedding should be filtered out
        assert!(node.properties.get("embedding").is_none());
    }
}
