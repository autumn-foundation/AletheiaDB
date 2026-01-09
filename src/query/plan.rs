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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct TemporalContext {
    /// Point-in-time query (as_of semantics)
    pub as_of: Option<(Timestamp, Timestamp)>,
    /// Time range query (between semantics)
    pub between: Option<TimeRange>,
}

impl TemporalContext {
    /// Create a point-in-time context
    #[must_use]
    pub fn as_of(valid_time: Timestamp, transaction_time: Timestamp) -> Self {
        TemporalContext {
            as_of: Some((valid_time, transaction_time)),
            between: None,
        }
    }

    /// Create a time range context
    #[must_use]
    pub fn between(range: TimeRange) -> Self {
        TemporalContext {
            as_of: None,
            between: Some(range),
        }
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
            .with_temporal_context(TemporalContext::as_of(1000, 1000));

        assert!(plan.is_temporal());
    }

    #[test]
    fn test_has_vector_ops() {
        let vector_scan = LogicalOp::Scan(ScanOp::VectorSearch {
            embedding: Arc::from([1.0f32; 384].as_slice()),
            k: 10,
            label_filter: None,
            metric: DistanceMetric::Cosine,
        });

        assert!(vector_scan.has_vector_ops());

        let vector_rank = LogicalOp::unary(
            UnaryOp::VectorRank {
                embedding: Arc::from([1.0f32; 384].as_slice()),
                top_k: Some(10),
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
    fn test_temporal_context() {
        let as_of = TemporalContext::as_of(1000, 2000);
        assert!(as_of.as_of.is_some());
        assert!(as_of.between.is_none());

        let between = TemporalContext::between(TimeRange::from(0));
        assert!(between.as_of.is_none());
        assert!(between.between.is_some());
    }
}
