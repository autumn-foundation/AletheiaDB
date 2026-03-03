//! Logical Query Plan
//!
//! Represents the structure of a query as a tree of logical operations.
//! The logical plan is optimized and then converted to a physical plan
//! for execution.

use std::sync::Arc;

use crate::core::NodeId;
use crate::core::temporal::{TimeRange, Timestamp};
use crate::index::vector::DistanceMetric;

use super::ir::{Direction, Predicate, TraversalDepth};

/// A logical query plan represented as a tree of operations.
#[derive(Debug, Clone)]
pub struct LogicalPlan {
    /// Root operation of the plan tree
    pub root: LogicalOp,
    /// Temporal context for the entire query (if any)
    pub temporal_context: Option<TemporalContext>,
    /// Hints for query optimization
    pub hints: QueryHints,
}

impl LogicalPlan {
    /// Create a new logical plan with the given root operation
    #[must_use]
    pub fn new(root: LogicalOp) -> Self {
        LogicalPlan {
            root,
            temporal_context: None,
            hints: QueryHints::default(),
        }
    }

    /// Add temporal context to the plan
    #[must_use]
    pub fn with_temporal_context(mut self, context: TemporalContext) -> Self {
        self.temporal_context = Some(context);
        self
    }

    /// Add query hints
    #[must_use]
    pub fn with_hints(mut self, hints: QueryHints) -> Self {
        self.hints = hints;
        self
    }

    /// Check if this query requires temporal storage access
    #[must_use]
    pub fn is_temporal(&self) -> bool {
        self.temporal_context.is_some()
    }

    /// Check if this query involves vector operations
    #[must_use]
    pub fn has_vector_ops(&self) -> bool {
        self.root.has_vector_ops()
    }

    /// Check if this query involves graph traversal
    #[must_use]
    pub fn has_traversal(&self) -> bool {
        self.root.has_traversal()
    }
}

/// Logical operation nodes in the query plan tree.
#[derive(Debug, Clone)]
pub enum LogicalOp {
    /// Leaf operation: data source (scan, lookup, search)
    Scan(ScanOp),

    /// Unary operation: transforms a single input
    Unary {
        /// The operation to apply
        op: UnaryOp,
        /// Input to this operation
        input: Box<LogicalOp>,
    },

    /// Binary operation: combines two inputs
    Binary {
        /// The operation to apply
        op: BinaryOp,
        /// Left input
        left: Box<LogicalOp>,
        /// Right input
        right: Box<LogicalOp>,
    },

    /// Empty result (used for optimization)
    Empty,
}

impl LogicalOp {
    /// Create a unary operation
    #[must_use]
    pub fn unary(op: UnaryOp, input: LogicalOp) -> Self {
        LogicalOp::Unary {
            op,
            input: Box::new(input),
        }
    }

    /// Create a binary operation
    #[must_use]
    pub fn binary(op: BinaryOp, left: LogicalOp, right: LogicalOp) -> Self {
        LogicalOp::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    /// Check if this operation or its children involve vector operations
    #[must_use]
    pub fn has_vector_ops(&self) -> bool {
        match self {
            LogicalOp::Scan(scan) => matches!(scan, ScanOp::VectorSearch { .. }),
            LogicalOp::Unary { op, input } => {
                matches!(op, UnaryOp::VectorRank { .. }) || input.has_vector_ops()
            }
            LogicalOp::Binary { left, right, .. } => {
                left.has_vector_ops() || right.has_vector_ops()
            }
            LogicalOp::Empty => false,
        }
    }

    /// Check if this operation or its children involve graph traversal
    #[must_use]
    pub fn has_traversal(&self) -> bool {
        match self {
            LogicalOp::Scan(_) => false,
            LogicalOp::Unary { op, input } => {
                matches!(op, UnaryOp::Traverse { .. }) || input.has_traversal()
            }
            LogicalOp::Binary { left, right, .. } => left.has_traversal() || right.has_traversal(),
            LogicalOp::Empty => false,
        }
    }
}

/// Scan operations (leaf nodes in the plan tree).
///
/// These represent data sources - the starting points for query execution.
#[derive(Debug, Clone)]
pub enum ScanOp {
    /// Direct node lookup by ID(s) - O(1) per node
    NodeLookup(Vec<NodeId>),

    /// Full node scan with optional label filter
    NodeScan {
        /// Optional label to filter by
        label: Option<String>,
        /// Estimated number of rows (for cost estimation)
        estimated_rows: Option<usize>,
    },

    /// Vector k-NN search using HNSW index
    VectorSearch {
        /// Query embedding
        embedding: Arc<[f32]>,
        /// Number of results to return
        k: usize,
        /// Optional label filter
        label_filter: Option<String>,
        /// Distance metric
        metric: DistanceMetric,
        /// Property key containing the embedding (None = "embedding")
        property_key: Option<String>,
    },

    /// Temporal point-in-time node lookup
    TemporalNodeLookup {
        /// Node IDs to look up
        node_ids: Vec<NodeId>,
        /// Valid time for the query
        valid_time: Timestamp,
        /// Transaction time for the query
        transaction_time: Timestamp,
    },

    /// Temporal vector search using historical snapshots
    TemporalVectorSearch {
        /// Query embedding
        embedding: Arc<[f32]>,
        /// Number of results to return
        k: usize,
        /// Timestamp for the historical query
        timestamp: Timestamp,
        /// Property key containing the embedding vector
        /// If None, uses the default/first indexed property
        property_key: Option<String>,
    },
    /// Find nodes similar to a specific node by extracting its embedding
    SimilarToNode {
        /// Source node whose embedding to use
        source_node: NodeId,
        /// Property key containing the embedding vector
        property_key: String,
        /// Number of results to return
        k: usize,
        /// Optional label filter for results
        label_filter: Option<String>,
    },

    /// Property-based node scan: nodes with a given label where property == value.
    ///
    /// This is the fused form of `NodeScan { label } + Filter(Eq { key, value })`,
    /// produced by the `FilterScanFusion` optimization rule. It delegates to
    /// `CurrentStorage::find_nodes_by_property` which avoids per-node predicate
    /// evaluation on non-matching nodes.
    PropertyScan {
        /// Label to filter by
        label: String,
        /// Property key to match
        key: String,
        /// Expected property value
        value: super::ir::PredicateValue,
    },
}

/// Unary operations that transform a single input.
#[derive(Debug, Clone)]
pub enum UnaryOp {
    /// Filter rows by predicate
    Filter(Predicate),

    /// Project specific properties
    Project(Vec<String>),

    /// Limit number of results
    Limit(usize),

    /// Skip a number of results
    Skip(usize),

    /// Graph traversal
    Traverse {
        /// Traversal direction
        direction: Direction,
        /// Optional edge label filter
        label: Option<String>,
        /// Depth specification
        depth: TraversalDepth,
    },

    /// Rank by vector similarity
    VectorRank {
        /// Target embedding for comparison
        embedding: Arc<[f32]>,
        /// Optional limit on ranked results
        top_k: Option<usize>,
        /// Property key containing the embedding (None = "embedding")
        property_key: Option<String>,
    },

    /// Sort by property or score
    Sort {
        /// Sort key
        key: SortKey,
        /// Sort order (true = descending)
        descending: bool,
    },

    /// Track temporal changes
    TemporalTrack {
        /// Time range to track within
        time_range: TimeRange,
    },

    /// Distinct/deduplicate results
    Distinct,

    /// Count results (aggregate)
    Count,
}

/// Binary operations that combine two inputs.
#[derive(Debug, Clone, PartialEq)]
pub enum BinaryOp {
    /// Set union of results
    Union,

    /// Set intersection of results
    Intersect,

    /// Set difference (left - right)
    Except,

    /// Join on matching property values
    Join {
        /// Property key from left input
        left_key: String,
        /// Property key from right input
        right_key: String,
    },
}

/// Key for sorting results.
#[derive(Debug, Clone, PartialEq)]
pub enum SortKey {
    /// Sort by a property value
    Property(String),
    /// Sort by similarity score
    Score,
    /// Sort by timestamp
    Timestamp,
}

/// Temporal context for a query.
///
/// Specifies when to evaluate the query in time.
/// Temporal query context supporting independent valid_time and transaction_time dimensions.
///
/// Enables true bi-temporal queries where each dimension can be queried independently:
/// - "What did we know at time T?" → `as_of_transaction_time(T)`
/// - "What was true at time T?" → `as_of_valid_time(T)`
/// - "What did we know at time T about what was true at time V?" → `as_of(V, T)`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TemporalContext {
    /// Point-in-time query for valid_time dimension
    pub valid_time_as_of: Option<Timestamp>,
    /// Point-in-time query for transaction_time dimension
    pub transaction_time_as_of: Option<Timestamp>,
    /// Time range query for valid_time dimension
    pub valid_time_between: Option<TimeRange>,
    /// Time range query for transaction_time dimension
    pub transaction_time_between: Option<TimeRange>,
    /// Include historical versions in results (for history queries)
    pub include_history: bool,
}

impl TemporalContext {
    /// Create a point-in-time context for both dimensions (traditional bi-temporal query).
    ///
    /// # Example
    /// ```ignore
    /// // "What did we know at tx_time about what was valid at valid_time?"
    /// let ctx = TemporalContext::as_of(valid_time, tx_time);
    /// ```
    #[must_use]
    pub fn as_of(valid_time: Timestamp, transaction_time: Timestamp) -> Self {
        TemporalContext {
            valid_time_as_of: Some(valid_time),
            transaction_time_as_of: Some(transaction_time),
            valid_time_between: None,
            transaction_time_between: None,
            include_history: false,
        }
    }

    /// Create a context querying only the valid_time dimension.
    ///
    /// Transaction time defaults to "now" (most recent recorded state).
    ///
    /// # Example
    /// ```ignore
    /// // "What was valid/true at this time?" (using current database state)
    /// let ctx = TemporalContext::as_of_valid_time(timestamp);
    /// ```
    #[must_use]
    pub fn as_of_valid_time(timestamp: Timestamp) -> Self {
        TemporalContext {
            valid_time_as_of: Some(timestamp),
            transaction_time_as_of: None,
            valid_time_between: None,
            transaction_time_between: None,
            include_history: false,
        }
    }

    /// Create a context querying only the transaction_time dimension.
    ///
    /// Valid time defaults to "now" (current validity).
    ///
    /// # Example
    /// ```ignore
    /// // "What did we know/record at this time?" (database state as of tx_time)
    /// let ctx = TemporalContext::as_of_transaction_time(timestamp);
    /// ```
    #[must_use]
    pub fn as_of_transaction_time(timestamp: Timestamp) -> Self {
        TemporalContext {
            valid_time_as_of: None,
            transaction_time_as_of: Some(timestamp),
            valid_time_between: None,
            transaction_time_between: None,
            include_history: false,
        }
    }

    /// Create a context for querying a valid_time range.
    ///
    /// # Example
    /// ```ignore
    /// // "What was valid between start and end?"
    /// let ctx = TemporalContext::valid_time_between(range);
    /// ```
    #[must_use]
    pub fn valid_time_between(range: TimeRange) -> Self {
        TemporalContext {
            valid_time_as_of: None,
            transaction_time_as_of: None,
            valid_time_between: Some(range),
            transaction_time_between: None,
            include_history: false,
        }
    }

    /// Create a context for querying a transaction_time range.
    ///
    /// # Example
    /// ```ignore
    /// // "What did we record between start and end?"
    /// let ctx = TemporalContext::transaction_time_between(range);
    /// ```
    #[must_use]
    pub fn transaction_time_between(range: TimeRange) -> Self {
        TemporalContext {
            valid_time_as_of: None,
            transaction_time_as_of: None,
            valid_time_between: None,
            transaction_time_between: Some(range),
            include_history: false,
        }
    }

    /// Create a time range context (backward compatibility - uses valid_time dimension).
    ///
    /// **Deprecated**: Use `valid_time_between()` or `transaction_time_between()` for clarity.
    #[must_use]
    #[deprecated(
        since = "0.1.0",
        note = "Use valid_time_between() or transaction_time_between() instead"
    )]
    pub fn between(range: TimeRange) -> Self {
        Self::valid_time_between(range)
    }

    /// Set whether to include historical versions in results.
    ///
    /// When enabled, queries return all versions within the temporal context
    /// rather than just the current state.
    #[must_use]
    pub fn with_history(mut self, include: bool) -> Self {
        self.include_history = include;
        self
    }

    /// Resolve temporal dimensions by filling missing dimensions with current time.
    ///
    /// Returns `(valid_time, transaction_time)` where:
    /// - Missing `valid_time_as_of` → `time::now()`
    /// - Missing `transaction_time_as_of` → `time::now()`
    ///
    /// # Example
    /// ```ignore
    /// let ctx = TemporalContext::as_of_valid_time(ts);
    /// let (vt, tt) = ctx.resolve_now();
    /// // vt == ts, tt == now()
    /// ```
    #[must_use]
    pub fn resolve_now(&self) -> (Timestamp, Timestamp) {
        use crate::core::temporal::time;

        let now = time::now();
        let valid_time = self.valid_time_as_of.unwrap_or(now);
        let transaction_time = self.transaction_time_as_of.unwrap_or(now);

        (valid_time, transaction_time)
    }

    // =========================================================================
    // Backward Compatibility Helpers
    // =========================================================================

    /// Get point-in-time timestamps as a tuple (backward compatibility).
    ///
    /// Returns `Some((valid_time, transaction_time))` only if both dimensions
    /// are set as point-in-time queries.
    #[must_use]
    pub fn as_of_tuple(&self) -> Option<(Timestamp, Timestamp)> {
        match (self.valid_time_as_of, self.transaction_time_as_of) {
            (Some(vt), Some(tt)) => Some((vt, tt)),
            _ => None,
        }
    }

    /// Check if this context has any temporal constraints.
    #[must_use]
    pub fn is_temporal(&self) -> bool {
        self.valid_time_as_of.is_some()
            || self.transaction_time_as_of.is_some()
            || self.valid_time_between.is_some()
            || self.transaction_time_between.is_some()
    }
}

/// Hints for query optimization.
#[derive(Debug, Clone, Default)]
pub struct QueryHints {
    /// User-provided cardinality estimate
    pub estimated_cardinality: Option<usize>,
    /// Force specific index usage
    pub force_index: Option<IndexHint>,
    /// Disabled optimizations
    pub disabled_optimizations: Vec<String>,
    /// Enable parallel execution
    pub parallel: bool,
    /// Include provenance metadata (timestamps, paths, version info) in results
    pub include_provenance: bool,
}

/// Index hint for forcing specific index usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexHint {
    /// Use the HNSW vector index
    UseVectorIndex,
    /// Use the temporal index
    UseTemporalIndex,
    /// Use current storage (fast path)
    UseCurrentStorage,
    /// Use historical storage
    UseHistoricalStorage,
    /// Force brute-force vector search (skip HNSW)
    UseBruteForce,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logical_plan_creation() {
        let plan = LogicalPlan::new(LogicalOp::Scan(ScanOp::NodeLookup(vec![])));

        assert!(!plan.is_temporal());
        assert!(!plan.has_vector_ops());
        assert!(!plan.has_traversal());
    }

    #[test]
    fn test_logical_plan_with_temporal() {
        let plan = LogicalPlan::new(LogicalOp::Scan(ScanOp::NodeLookup(vec![])))
            .with_temporal_context(TemporalContext::as_of(1000.into(), 1000.into()));

        assert!(plan.is_temporal());
    }

    #[test]
    fn test_has_vector_ops() {
        let vector_scan = LogicalOp::Scan(ScanOp::VectorSearch {
            embedding: Arc::from([1.0f32; 384].as_slice()),
            k: 10,
            label_filter: None,
            metric: DistanceMetric::Cosine,
            property_key: None,
        });

        assert!(vector_scan.has_vector_ops());

        let vector_rank = LogicalOp::unary(
            UnaryOp::VectorRank {
                embedding: Arc::from([1.0f32; 384].as_slice()),
                top_k: Some(10),
                property_key: None,
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![])),
        );

        assert!(vector_rank.has_vector_ops());
    }

    #[test]
    fn test_has_traversal() {
        let traverse = LogicalOp::unary(
            UnaryOp::Traverse {
                direction: Direction::Outgoing,
                label: None,
                depth: TraversalDepth::Exact(1),
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![])),
        );

        assert!(traverse.has_traversal());
    }

    #[test]
    fn test_temporal_context_backward_compat() {
        let as_of = TemporalContext::as_of(1000.into(), 2000.into());
        assert_eq!(as_of.valid_time_as_of, Some(1000.into()));
        assert_eq!(as_of.transaction_time_as_of, Some(2000.into()));
        assert_eq!(as_of.valid_time_between, None);
        assert_eq!(as_of.transaction_time_between, None);

        #[allow(deprecated)]
        let between = TemporalContext::between(TimeRange::from(0.into()));
        assert_eq!(between.valid_time_as_of, None);
        assert_eq!(between.transaction_time_as_of, None);
        assert!(between.valid_time_between.is_some());
    }

    // =========================================================================
    // Phase 6: Enhanced TemporalContext Tests
    // =========================================================================

    #[test]
    fn test_temporal_context_as_of_valid_time_only() {
        use crate::core::temporal::time;

        let ts = time::now();
        let ctx = TemporalContext::as_of_valid_time(ts);

        assert_eq!(ctx.valid_time_as_of, Some(ts));
        assert_eq!(ctx.transaction_time_as_of, None);
        assert_eq!(ctx.valid_time_between, None);
        assert_eq!(ctx.transaction_time_between, None);
        assert!(!ctx.include_history);
    }

    #[test]
    fn test_temporal_context_as_of_transaction_time_only() {
        use crate::core::temporal::time;

        let ts = time::now();
        let ctx = TemporalContext::as_of_transaction_time(ts);

        assert_eq!(ctx.transaction_time_as_of, Some(ts));
        assert_eq!(ctx.valid_time_as_of, None);
        assert_eq!(ctx.valid_time_between, None);
        assert_eq!(ctx.transaction_time_between, None);
        assert!(!ctx.include_history);
    }

    #[test]
    fn test_temporal_context_as_of_both_dimensions() {
        use crate::core::temporal::time;

        let vt = time::now();
        let tt = time::now();
        let ctx = TemporalContext::as_of(vt, tt);

        assert_eq!(ctx.valid_time_as_of, Some(vt));
        assert_eq!(ctx.transaction_time_as_of, Some(tt));
        assert_eq!(ctx.valid_time_between, None);
        assert_eq!(ctx.transaction_time_between, None);
    }

    #[test]
    fn test_temporal_context_valid_time_between() {
        use crate::core::hlc::HybridTimestamp;

        let start = HybridTimestamp::new(1000, 0).unwrap();
        let end = HybridTimestamp::new(2000, 0).unwrap();
        let range = TimeRange::new(start, end).unwrap();

        let ctx = TemporalContext::valid_time_between(range);

        assert_eq!(ctx.valid_time_between, Some(range));
        assert_eq!(ctx.transaction_time_between, None);
        assert_eq!(ctx.valid_time_as_of, None);
        assert_eq!(ctx.transaction_time_as_of, None);
    }

    #[test]
    fn test_temporal_context_transaction_time_between() {
        use crate::core::hlc::HybridTimestamp;

        let start = HybridTimestamp::new(1000, 0).unwrap();
        let end = HybridTimestamp::new(2000, 0).unwrap();
        let range = TimeRange::new(start, end).unwrap();

        let ctx = TemporalContext::transaction_time_between(range);

        assert_eq!(ctx.transaction_time_between, Some(range));
        assert_eq!(ctx.valid_time_between, None);
        assert_eq!(ctx.valid_time_as_of, None);
        assert_eq!(ctx.transaction_time_as_of, None);
    }

    #[test]
    fn test_temporal_context_resolve_now_fills_missing() {
        use crate::core::hlc::HybridTimestamp;

        let ts = HybridTimestamp::new(1000, 0).unwrap();
        let ctx = TemporalContext::as_of_valid_time(ts);

        let resolved = ctx.resolve_now();

        // Valid time should be preserved
        assert_eq!(resolved.0, ts);

        // Transaction time should be approximately now
        assert!(resolved.1.wallclock() > ts.wallclock());
    }

    #[test]
    fn test_temporal_context_resolve_now_preserves_both() {
        use crate::core::hlc::HybridTimestamp;

        let vt = HybridTimestamp::new(1000, 0).unwrap();
        let tt = HybridTimestamp::new(2000, 0).unwrap();
        let ctx = TemporalContext::as_of(vt, tt);

        let resolved = ctx.resolve_now();

        // Both should be preserved
        assert_eq!(resolved.0, vt);
        assert_eq!(resolved.1, tt);
    }

    #[test]
    fn test_temporal_context_with_history() {
        use crate::core::temporal::time;

        let ts = time::now();
        let ctx = TemporalContext::as_of_valid_time(ts).with_history(true);

        assert!(ctx.include_history);
        assert_eq!(ctx.valid_time_as_of, Some(ts));
    }

    #[test]
    fn test_temporal_context_default_is_empty() {
        let ctx = TemporalContext::default();

        assert_eq!(ctx.valid_time_as_of, None);
        assert_eq!(ctx.transaction_time_as_of, None);
        assert_eq!(ctx.valid_time_between, None);
        assert_eq!(ctx.transaction_time_between, None);
        assert!(!ctx.include_history);
    }
}
