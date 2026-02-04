//! Fluent Query Builder
//!
//! Provides a type-safe, fluent API for constructing hybrid queries.
//! The builder uses phantom types to track query state at compile time,
//! preventing invalid query compositions.
//!
//! # Example
//!
//! ```rust
//! use gallifreydb::query::QueryBuilder;
//! use gallifreydb::core::NodeId;
//! use gallifreydb::core::temporal::Timestamp;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let alice_id = NodeId::new(1)?;
//! let bob_id = NodeId::new(2)?;
//! let node_id = NodeId::new(3)?;
//! let embedding = vec![0.1, 0.2, 0.3];
//! let valid_time = Timestamp::new(1000, 0)?;
//! let tx_time = Timestamp::new(2000, 0)?;
//!
//! // Build a graph + vector query
//! let query = QueryBuilder::new()
//!     .start(alice_id)
//!     .traverse("KNOWS")
//!     .rank_by_similarity(&embedding, 10)
//!     .build();
//!
//! // Build a temporal query
//! let query = QueryBuilder::new()
//!     .as_of(valid_time, tx_time)
//!     .start(node_id)
//!     .build();
//!
//! // Build a vector similarity query (simple)
//! let query = QueryBuilder::new()
//!     .start(alice_id)
//!     .similar_to(bob_id, 10)
//!     .build();
//!
//! // Build a vector similarity query (advanced with builder)
//! let query = QueryBuilder::new()
//!     .start(alice_id)
//!     .similar_to_builder(bob_id, 10)
//!         .property("custom_embedding")
//!         .label_filter("Person")
//!         .finish()
//!     .build();
//! # Ok(())
//! # }
//! ```

use std::marker::PhantomData;
use std::sync::Arc;

use crate::core::NodeId;
use crate::core::temporal::{TimeRange, Timestamp};
use crate::index::vector::DistanceMetric;

use super::QueryRunner;
use super::ir::{Predicate, QueryOp, TraversalDepth};
use super::plan::{IndexHint, QueryHints, TemporalContext};

/// A fully constructed query ready for execution.
#[derive(Debug, Clone)]
pub struct Query {
    /// Sequence of query operations
    pub(crate) ops: Vec<QueryOp>,
    /// Temporal context (if any)
    pub(crate) temporal_context: Option<TemporalContext>,
    /// Query hints for optimization
    pub(crate) hints: QueryHints,
}

impl Query {
    /// Check if this query has temporal context
    #[must_use]
    pub fn is_temporal(&self) -> bool {
        self.temporal_context.is_some()
    }

    /// Get the number of operations in this query
    #[must_use]
    pub fn operation_count(&self) -> usize {
        self.ops.len()
    }
}

/// Marker trait for query builder states.
pub trait QueryState: private::Sealed {}

mod private {
    pub trait Sealed {}
}

/// Query builder states for compile-time safety.
pub mod state {
    use super::private;

    /// Initial state - no source defined yet
    #[derive(Debug, Clone, Copy)]
    pub struct Initial;
    impl private::Sealed for Initial {}
    impl super::QueryState for Initial {}

    /// Have node source(s)
    #[derive(Debug, Clone, Copy)]
    pub struct HasNodes;
    impl private::Sealed for HasNodes {}
    impl super::QueryState for HasNodes {}

    /// Have vector search results
    #[derive(Debug, Clone, Copy)]
    pub struct HasVectorResults;
    impl private::Sealed for HasVectorResults {}
    impl super::QueryState for HasVectorResults {}

    /// Have graph traversal results
    #[derive(Debug, Clone, Copy)]
    pub struct HasTraversalResults;
    impl private::Sealed for HasTraversalResults {}
    impl super::QueryState for HasTraversalResults {}
}

/// Fluent query builder with compile-time state tracking.
///
/// The generic parameter `S` tracks the current state of the query,
/// which determines what operations are available.
#[derive(Debug, Clone)]
pub struct QueryBuilder<S: QueryState> {
    ops: Vec<QueryOp>,
    temporal_context: Option<TemporalContext>,
    hints: QueryHints,
    _phantom: PhantomData<S>,
}

impl QueryBuilder<state::Initial> {
    /// Create a new query builder in initial state
    #[must_use]
    pub fn new() -> Self {
        QueryBuilder {
            ops: Vec::new(),
            temporal_context: None,
            hints: QueryHints::default(),
            _phantom: PhantomData,
        }
    }

    /// Start from a specific node
    #[must_use]
    pub fn start(self, node_id: NodeId) -> QueryBuilder<state::HasNodes> {
        self.add_op(QueryOp::StartNode(node_id))
    }

    /// Start from multiple nodes
    #[must_use]
    pub fn start_from(self, node_ids: Vec<NodeId>) -> QueryBuilder<state::HasNodes> {
        self.add_op(QueryOp::StartNodes(node_ids))
    }

    /// Start with a vector similarity search (simple version).
    ///
    /// Uses the default "embedding" property and Cosine distance.
    /// For custom properties or metrics, use [`find_similar_builder()`](Self::find_similar_builder).
    #[must_use]
    pub fn find_similar(
        self,
        embedding: &[f32],
        k: usize,
    ) -> QueryBuilder<state::HasVectorResults> {
        self.add_op(QueryOp::VectorSearch {
            embedding: Arc::from(embedding),
            k,
            metric: DistanceMetric::Cosine,
            property_key: None,
        })
    }

    /// Start with a vector similarity search using a specific metric
    #[must_use]
    pub fn find_similar_with_metric(
        self,
        embedding: &[f32],
        k: usize,
        metric: DistanceMetric,
    ) -> QueryBuilder<state::HasVectorResults> {
        self.add_op(QueryOp::VectorSearch {
            embedding: Arc::from(embedding),
            k,
            metric,
            property_key: None,
        })
    }

    /// Create a builder for advanced VectorSearch configuration.
    ///
    /// Use this when you need to specify a custom embedding property key
    /// or distance metric. For simple cases, use [`find_similar()`](Self::find_similar).
    ///
    /// # Example
    ///
    /// ```rust
    /// use gallifreydb::query::QueryBuilder;
    /// use gallifreydb::index::vector::DistanceMetric;
    ///
    /// let embedding = vec![0.1, 0.2, 0.3];
    /// let query = QueryBuilder::new()
    ///     .find_similar_builder(&embedding, 10)
    ///         .property("title_embedding")
    ///         .metric(DistanceMetric::Euclidean)
    ///         .finish()
    ///     .build();
    /// ```
    pub fn find_similar_builder(self, embedding: &[f32], k: usize) -> FindSimilarBuilder {
        FindSimilarBuilder::new(embedding, k, self)
    }

    /// Scan all nodes, optionally filtered by label
    #[must_use]
    pub fn scan(self, label: Option<&str>) -> QueryBuilder<state::HasNodes> {
        self.add_op(QueryOp::ScanNodes {
            label: label.map(String::from),
        })
    }

    /// Scan nodes with a specific label
    #[must_use]
    pub fn scan_label(self, label: &str) -> QueryBuilder<state::HasNodes> {
        self.add_op(QueryOp::ScanNodes {
            label: Some(label.to_string()),
        })
    }
}

impl Default for QueryBuilder<state::Initial> {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryBuilder<state::HasNodes> {
    /// Traverse outgoing edges with a specific label
    #[must_use]
    pub fn traverse(self, label: &str) -> QueryBuilder<state::HasTraversalResults> {
        self.add_op(QueryOp::TraverseOut {
            label: Some(label.to_string()),
            depth: TraversalDepth::Exact(1),
        })
    }

    /// Traverse outgoing edges without label filter
    #[must_use]
    pub fn traverse_all(self) -> QueryBuilder<state::HasTraversalResults> {
        self.add_op(QueryOp::TraverseOut {
            label: None,
            depth: TraversalDepth::Exact(1),
        })
    }

    /// Multi-hop traversal with a specific label
    #[must_use]
    pub fn traverse_n(self, label: &str, depth: usize) -> QueryBuilder<state::HasTraversalResults> {
        self.add_op(QueryOp::TraverseOut {
            label: Some(label.to_string()),
            depth: TraversalDepth::Exact(depth),
        })
    }

    /// Traverse incoming edges
    #[must_use]
    pub fn traverse_in(self, label: &str) -> QueryBuilder<state::HasTraversalResults> {
        self.add_op(QueryOp::TraverseIn {
            label: Some(label.to_string()),
            depth: TraversalDepth::Exact(1),
        })
    }

    /// Traverse edges in both directions
    #[must_use]
    pub fn traverse_both(self, label: &str) -> QueryBuilder<state::HasTraversalResults> {
        self.add_op(QueryOp::TraverseBoth {
            label: Some(label.to_string()),
            depth: TraversalDepth::Exact(1),
        })
    }

    /// Rank current nodes by similarity to an embedding (simple version).
    ///
    /// Uses the default "embedding" property. For custom properties,
    /// use [`rank_by_similarity_builder()`](Self::rank_by_similarity_builder).
    #[must_use]
    pub fn rank_by_similarity(
        self,
        embedding: &[f32],
        top_k: usize,
    ) -> QueryBuilder<state::HasVectorResults> {
        self.add_op(QueryOp::RankBySimilarity {
            embedding: Arc::from(embedding),
            top_k: Some(top_k),
            property_key: None,
        })
    }

    /// Create a builder for advanced RankBySimilarity configuration.
    ///
    /// Use this when you need to specify a custom embedding property key.
    /// For simple cases, use [`rank_by_similarity()`](Self::rank_by_similarity).
    ///
    /// # Example
    ///
    /// ```rust
    /// use gallifreydb::query::QueryBuilder;
    /// use gallifreydb::core::NodeId;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let alice_id = NodeId::new(1)?;
    /// let embedding = vec![0.1, 0.2, 0.3];
    ///
    /// let query = QueryBuilder::new()
    ///     .start(alice_id)
    ///     .traverse("KNOWS")
    ///     .rank_by_similarity_builder(&embedding, 10)
    ///         .property("custom_embedding")
    ///         .finish()
    ///     .build();
    /// # Ok(())
    /// # }
    /// ```
    pub fn rank_by_similarity_builder(
        self,
        embedding: &[f32],
        top_k: usize,
    ) -> RankBySimilarityBuilder<state::HasNodes> {
        RankBySimilarityBuilder::new(embedding, top_k, self)
    }

    /// Find nodes similar to a source node's embedding (simple version).
    ///
    /// This is a convenience method that uses:
    /// - Default property key: "embedding"
    /// - No label filter
    ///
    /// For advanced options (custom property, label filtering), use
    /// [`similar_to_builder()`](Self::similar_to_builder).
    ///
    /// # Arguments
    /// * `source_node` - Node whose embedding to use for similarity search
    /// * `k` - Number of similar nodes to return (must be > 0)
    ///
    /// # Errors
    /// Returns `QueryError::ExecutionError` if:
    /// - Source node doesn't exist
    /// - Source node doesn't have an "embedding" property
    /// - The "embedding" property is not a vector type
    /// - No vector index is enabled for "embedding"
    ///
    /// # Performance
    /// Cost = O(1) node lookup + O(log N) HNSW search where N = indexed vectors
    ///
    /// # Panics
    /// Panics if k = 0
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use gallifreydb::GallifreyDB;
    /// # use gallifreydb::core::NodeId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = GallifreyDB::new()?;
    /// # let node_id = NodeId::new(1)?;
    /// # let bob_id = NodeId::new(2)?;
    /// let results = db.query()
    ///     .start(node_id)
    ///     .similar_to(bob_id, 10)
    ///     .execute(&db)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # See Also
    /// - [`similar_to_builder()`](Self::similar_to_builder) - Advanced configuration
    #[must_use]
    pub fn similar_to(
        self,
        source_node: NodeId,
        k: usize,
    ) -> QueryBuilder<state::HasVectorResults> {
        if k == 0 {
            panic!("k must be greater than 0");
        }
        self.add_op(QueryOp::SimilarTo {
            source_node,
            k,
            property_key: None,
            label_filter: None,
        })
    }

    /// Create a builder for advanced SimilarTo configuration.
    ///
    /// Use this when you need to:
    /// - Specify a custom embedding property key
    /// - Filter results by label
    /// - (Future) Configure distance metric, exclusions, etc.
    ///
    /// For simple cases, use [`similar_to()`](Self::similar_to) instead.
    ///
    /// # Arguments
    /// * `source_node` - Node whose embedding to use for similarity search
    /// * `k` - Number of similar nodes to return (must be > 0)
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use gallifreydb::GallifreyDB;
    /// # use gallifreydb::core::NodeId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = GallifreyDB::new()?;
    /// # let alice_id = NodeId::new(1)?;
    /// # let bob_id = NodeId::new(2)?;
    /// // Find similar documents using custom embedding
    /// let results = db.query()
    ///     .start(alice_id)
    ///     .similar_to_builder(bob_id, 10)
    ///         .property("custom_embedding")
    ///         .label_filter("Document")
    ///         .finish()
    ///     .execute(&db)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Panics
    /// Panics if k = 0
    pub fn similar_to_builder(
        self,
        source_node: NodeId,
        k: usize,
    ) -> SimilarToBuilder<state::HasNodes> {
        SimilarToBuilder::new(source_node, k, self)
    }

    /// Filter results by predicate
    #[must_use]
    pub fn filter(self, predicate: Predicate) -> QueryBuilder<state::HasNodes> {
        self.add_op_same(QueryOp::Filter(predicate))
    }

    /// Filter by label
    #[must_use]
    pub fn with_label(self, label: &str) -> QueryBuilder<state::HasNodes> {
        self.add_op_same(QueryOp::FilterLabel(label.to_string()))
    }
}

impl QueryBuilder<state::HasTraversalResults> {
    /// Continue traversing with a specific label
    #[must_use]
    pub fn traverse(self, label: &str) -> QueryBuilder<state::HasTraversalResults> {
        self.add_op_same(QueryOp::TraverseOut {
            label: Some(label.to_string()),
            depth: TraversalDepth::Exact(1),
        })
    }

    /// Rank traversal results by similarity (simple version).
    ///
    /// Uses the default "embedding" property. For custom properties,
    /// use [`rank_by_similarity_builder()`](Self::rank_by_similarity_builder).
    #[must_use]
    pub fn rank_by_similarity(
        self,
        embedding: &[f32],
        top_k: usize,
    ) -> QueryBuilder<state::HasVectorResults> {
        self.add_op(QueryOp::RankBySimilarity {
            embedding: Arc::from(embedding),
            top_k: Some(top_k),
            property_key: None,
        })
    }

    /// Create a builder for advanced RankBySimilarity configuration.
    ///
    /// Use this when you need to specify a custom embedding property key.
    /// For simple cases, use [`rank_by_similarity()`](Self::rank_by_similarity).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let query = QueryBuilder::new()
    ///     .start(alice_id)
    ///     .traverse("KNOWS")
    ///     .traverse("WORKS_AT")
    ///     .rank_by_similarity_builder(&embedding, 10)
    ///         .property("custom_embedding")
    ///         .finish()
    ///     .build();
    /// ```
    pub fn rank_by_similarity_builder(
        self,
        embedding: &[f32],
        top_k: usize,
    ) -> RankBySimilarityBuilder<state::HasTraversalResults> {
        RankBySimilarityBuilder::new(embedding, top_k, self)
    }

    /// Filter traversal results
    #[must_use]
    pub fn filter(self, predicate: Predicate) -> QueryBuilder<state::HasTraversalResults> {
        self.add_op_same(QueryOp::Filter(predicate))
    }

    /// Filter by label
    #[must_use]
    pub fn with_label(self, label: &str) -> QueryBuilder<state::HasTraversalResults> {
        self.add_op_same(QueryOp::FilterLabel(label.to_string()))
    }
}

impl QueryBuilder<state::HasVectorResults> {
    /// Traverse from vector search results
    #[must_use]
    pub fn traverse(self, label: &str) -> QueryBuilder<state::HasTraversalResults> {
        self.add_op(QueryOp::TraverseOut {
            label: Some(label.to_string()),
            depth: TraversalDepth::Exact(1),
        })
    }

    /// Filter vector results
    #[must_use]
    pub fn filter(self, predicate: Predicate) -> QueryBuilder<state::HasVectorResults> {
        self.add_op_same(QueryOp::Filter(predicate))
    }

    /// Filter by label
    #[must_use]
    pub fn with_label(self, label: &str) -> QueryBuilder<state::HasVectorResults> {
        self.add_op_same(QueryOp::FilterLabel(label.to_string()))
    }
}

// Methods available in any state
impl<S: QueryState> QueryBuilder<S> {
    /// Set temporal context: query as of a specific point in time
    #[must_use]
    pub fn as_of(mut self, valid_time: Timestamp, transaction_time: Timestamp) -> Self {
        self.temporal_context = Some(TemporalContext::as_of(valid_time, transaction_time));
        self
    }

    /// Set temporal context: query as of a specific valid time only.
    ///
    /// Transaction time will resolve to "now" during query execution.
    /// This enables querying "what was valid at time T" regardless of when it was recorded.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // "Who did Alice know on 2024-01-15?"
    /// let results = db.query()
    ///     .as_of_valid_time(jan_15)
    ///     .start(alice_id)
    ///     .traverse("KNOWS")
    ///     .execute(&db)?;
    /// ```
    #[must_use]
    pub fn as_of_valid_time(mut self, valid_time: Timestamp) -> Self {
        let mut ctx = self.temporal_context.take().unwrap_or_default();
        ctx.valid_time_as_of = Some(valid_time);
        self.temporal_context = Some(ctx);
        self
    }

    /// Set temporal context: query as of a specific transaction time only.
    ///
    /// Valid time will resolve to "now" during query execution.
    /// This enables querying "what did we know at time T" regardless of when facts were valid.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // "What did we know about Alice on 2024-01-15?"
    /// let results = db.query()
    ///     .as_of_transaction_time(jan_15)
    ///     .start(alice_id)
    ///     .execute(&db)?;
    /// ```
    #[must_use]
    pub fn as_of_transaction_time(mut self, transaction_time: Timestamp) -> Self {
        let mut ctx = self.temporal_context.take().unwrap_or_default();
        ctx.transaction_time_as_of = Some(transaction_time);
        self.temporal_context = Some(ctx);
        self
    }

    /// Set temporal context: query across a valid time range.
    ///
    /// Transaction time will resolve to "now" during query execution.
    /// This enables querying "what was valid between time T1 and T2".
    ///
    /// # Arguments
    ///
    /// * `start` - Start of the valid time range (inclusive)
    /// * `end` - End of the valid time range (inclusive)
    ///
    /// # Example
    ///
    /// ```ignore
    /// // "Who did Alice know during Q1 2024?"
    /// let results = db.query()
    ///     .valid_time_between(jan_1, mar_31)
    ///     .start(alice_id)
    ///     .traverse("KNOWS")
    ///     .execute(&db)?;
    /// ```
    #[must_use]
    pub fn valid_time_between(mut self, start: Timestamp, end: Timestamp) -> Self {
        let mut ctx = self.temporal_context.take().unwrap_or_default();
        ctx.valid_time_between = Some(TimeRange::between(start, end).unwrap());
        self.temporal_context = Some(ctx);
        self
    }

    /// Set temporal context: query across a transaction time range.
    ///
    /// Valid time will resolve to "now" during query execution.
    /// This enables querying "what we knew between time T1 and T2".
    ///
    /// # Arguments
    ///
    /// * `start` - Start of the transaction time range (inclusive)
    /// * `end` - End of the transaction time range (inclusive)
    ///
    /// # Example
    ///
    /// ```ignore
    /// // "What did we learn about Alice during Q1 2024?"
    /// let results = db.query()
    ///     .transaction_time_between(jan_1, mar_31)
    ///     .start(alice_id)
    ///     .execute(&db)?;
    /// ```
    #[must_use]
    pub fn transaction_time_between(mut self, start: Timestamp, end: Timestamp) -> Self {
        let mut ctx = self.temporal_context.take().unwrap_or_default();
        ctx.transaction_time_between = Some(TimeRange::between(start, end).unwrap());
        self.temporal_context = Some(ctx);
        self
    }

    /// Set temporal context: query across a valid time range.
    ///
    /// This is an alias for [`valid_time_between()`](Self::valid_time_between).
    /// Transaction time will resolve to "now" during query execution.
    ///
    /// # Deprecated
    ///
    /// Prefer using [`valid_time_between()`](Self::valid_time_between) or
    /// [`transaction_time_between()`](Self::transaction_time_between) for clarity.
    #[must_use]
    pub fn between(mut self, start: Timestamp, end: Timestamp) -> Self {
        let mut ctx = self.temporal_context.take().unwrap_or_default();
        ctx.valid_time_between = Some(TimeRange::between(start, end).unwrap());
        self.temporal_context = Some(ctx);
        self
    }

    /// Limit the number of results
    #[must_use]
    pub fn limit(self, n: usize) -> Self {
        self.add_op_same(QueryOp::Limit(n))
    }

    /// Skip a number of results
    #[must_use]
    pub fn skip(self, n: usize) -> Self {
        self.add_op_same(QueryOp::Skip(n))
    }

    /// Add an optimization hint
    #[must_use]
    pub fn with_hint(mut self, hint: IndexHint) -> Self {
        self.hints.force_index = Some(hint);
        self
    }

    /// Enable parallel execution
    #[must_use]
    pub fn parallel(mut self) -> Self {
        self.hints.parallel = true;
        self
    }

    /// Include provenance metadata in query results.
    ///
    /// When enabled, query results will include additional metadata:
    /// - Timestamps for temporal queries (valid time and transaction time)
    /// - Traversal paths showing how each result was reached
    /// - Version information for historical queries
    ///
    /// This is useful for:
    /// - Audit trails and compliance
    /// - Understanding data lineage
    /// - Debugging complex queries
    /// - LLM reasoning about knowledge evolution
    ///
    /// # Example
    ///
    /// ```rust
    /// # use gallifreydb::query::QueryBuilder;
    /// # use gallifreydb::core::NodeId;
    /// # use gallifreydb::core::temporal::Timestamp;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let timestamp = Timestamp::new(1000, 0)?;
    /// # let tx_time = Timestamp::new(2000, 0)?;
    /// # let node_id = NodeId::new(1)?;
    /// let query = QueryBuilder::new()
    ///     .as_of(timestamp, tx_time)
    ///     .start(node_id)
    ///     .traverse("KNOWS")
    ///     .with_provenance()
    ///     .build();
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_provenance(mut self) -> Self {
        self.hints.include_provenance = true;
        self
    }

    /// Execute the query against the database.
    ///
    /// This is a convenience method that combines `build()` and `db.execute_query()`.
    /// It builds the query, optimizes it through the query planner, and executes it.
    ///
    /// # Arguments
    ///
    /// * `runner` - Reference to the database (or other query runner) to execute against
    ///
    /// # Returns
    ///
    /// `QueryResults` iterator that can be consumed to retrieve results
    ///
    /// # Errors
    ///
    /// Returns an error if query execution fails, including:
    /// - Invalid node/edge IDs
    /// - Missing vector index when required
    /// - Temporal reconstruction failures
    /// - Internal execution errors
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use gallifreydb::GallifreyDB;
    /// use gallifreydb::core::NodeId;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = GallifreyDB::new()?;
    /// let alice_id = NodeId::new(1)?;
    ///
    /// let results = db.query()
    ///     .start(alice_id)
    ///     .traverse("KNOWS")
    ///     .execute(&db)?;
    ///
    /// for row in results {
    ///     let row = row?;
    ///     println!("Found: {:?}", row.entity);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn execute(
        self,
        runner: &impl QueryRunner,
    ) -> crate::utils::error::Result<super::executor::QueryResults> {
        let query = self.build();
        runner.execute_query(query)
    }
    /// Build the final query
    #[must_use]
    pub fn build(self) -> Query {
        Query {
            ops: self.ops,
            temporal_context: self.temporal_context,
            hints: self.hints,
        }
    }

    // Internal helper to add op and change state
    fn add_op<T: QueryState>(mut self, op: QueryOp) -> QueryBuilder<T> {
        self.ops.push(op);
        QueryBuilder {
            ops: self.ops,
            temporal_context: self.temporal_context,
            hints: self.hints,
            _phantom: PhantomData,
        }
    }

    // Internal helper to add op and keep same state
    fn add_op_same(mut self, op: QueryOp) -> Self {
        self.ops.push(op);
        self
    }
}

/// Builder for configuring SimilarTo operations with optional parameters.
///
/// Created by calling [`QueryBuilder::similar_to_builder()`].
///
/// # Example
///
/// ```rust
/// # use gallifreydb::query::QueryBuilder;
/// # use gallifreydb::core::NodeId;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let alice_id = NodeId::new(1)?;
/// # let bob_id = NodeId::new(2)?;
/// let query = QueryBuilder::new()
///     .start(alice_id)
///     .similar_to_builder(bob_id, 10)
///         .property("custom_embedding")
///         .label_filter("Person")
///         .finish()
///     .build();
/// # Ok(())
/// # }
/// ```
#[must_use = "builders do nothing unless you call finish()"]
pub struct SimilarToBuilder<S: QueryState> {
    source_node: NodeId,
    k: usize,
    property_key: Option<String>,
    label_filter: Option<String>,
    query_builder: QueryBuilder<S>,
}

impl<S: QueryState> SimilarToBuilder<S> {
    fn new(source_node: NodeId, k: usize, query_builder: QueryBuilder<S>) -> Self {
        if k == 0 {
            panic!("k must be greater than 0");
        }
        SimilarToBuilder {
            source_node,
            k,
            property_key: None,
            label_filter: None,
            query_builder,
        }
    }

    /// Specify which vector property to use for similarity.
    ///
    /// Default: "embedding"
    ///
    /// # Example
    ///
    /// ```rust
    /// # use gallifreydb::query::QueryBuilder;
    /// # use gallifreydb::core::NodeId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let node_id = NodeId::new(1)?;
    /// QueryBuilder::new()
    ///     .start(node_id)
    ///     .similar_to_builder(node_id, 10)
    ///     .property("custom_embedding")
    ///     .finish();
    /// # Ok(())
    /// # }
    /// ```
    pub fn property(mut self, key: impl Into<String>) -> Self {
        self.property_key = Some(key.into());
        self
    }

    /// Filter results to only include nodes with this label.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use gallifreydb::query::QueryBuilder;
    /// # use gallifreydb::core::NodeId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let node_id = NodeId::new(1)?;
    /// QueryBuilder::new()
    ///     .start(node_id)
    ///     .similar_to_builder(node_id, 10)
    ///     .label_filter("Document")
    ///     .finish();
    /// # Ok(())
    /// # }
    /// ```
    pub fn label_filter(mut self, label: impl Into<String>) -> Self {
        self.label_filter = Some(label.into());
        self
    }

    /// Finish building and add the SimilarTo operation to the query.
    pub fn finish(self) -> QueryBuilder<state::HasVectorResults> {
        self.query_builder.add_op(QueryOp::SimilarTo {
            source_node: self.source_node,
            k: self.k,
            property_key: self.property_key,
            label_filter: self.label_filter,
        })
    }
}

/// Builder for configuring RankBySimilarity operations with optional parameters.
///
/// Created by calling [`QueryBuilder::rank_by_similarity_builder()`].
///
/// # Example
///
/// ```rust
/// # use gallifreydb::query::QueryBuilder;
/// # use gallifreydb::core::NodeId;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let alice_id = NodeId::new(1)?;
/// # let embedding = vec![0.1, 0.2, 0.3];
/// let query = QueryBuilder::new()
///     .start(alice_id)
///     .traverse("KNOWS")
///     .rank_by_similarity_builder(&embedding, 10)
///         .property("custom_embedding")
///         .finish()
///     .build();
/// # Ok(())
/// # }
/// ```
#[must_use = "builders do nothing unless you call finish()"]
pub struct RankBySimilarityBuilder<S: QueryState> {
    embedding: Arc<[f32]>,
    top_k: usize,
    property_key: Option<String>,
    query_builder: QueryBuilder<S>,
}

impl<S: QueryState> RankBySimilarityBuilder<S> {
    fn new(embedding: &[f32], top_k: usize, query_builder: QueryBuilder<S>) -> Self {
        RankBySimilarityBuilder {
            embedding: Arc::from(embedding),
            top_k,
            property_key: None,
            query_builder,
        }
    }

    /// Specify which vector property to use for ranking.
    ///
    /// Default: "embedding"
    ///
    /// # Example
    ///
    /// ```rust
    /// # use gallifreydb::query::QueryBuilder;
    /// # use gallifreydb::core::NodeId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let alice_id = NodeId::new(1)?;
    /// # let embedding = vec![0.1, 0.2, 0.3];
    /// QueryBuilder::new()
    ///     .start(alice_id)
    ///     .traverse("KNOWS")
    ///     .rank_by_similarity_builder(&embedding, 10)
    ///     .property("custom_embedding")
    ///     .finish();
    /// # Ok(())
    /// # }
    /// ```
    pub fn property(mut self, key: impl Into<String>) -> Self {
        self.property_key = Some(key.into());
        self
    }

    /// Finish building and add the RankBySimilarity operation to the query.
    pub fn finish(self) -> QueryBuilder<state::HasVectorResults> {
        self.query_builder.add_op(QueryOp::RankBySimilarity {
            embedding: self.embedding,
            top_k: Some(self.top_k),
            property_key: self.property_key,
        })
    }
}

/// Builder for configuring VectorSearch (find_similar) operations with optional parameters.
///
/// Created by calling [`QueryBuilder::find_similar_builder()`].
///
/// # Example
///
/// ```rust
/// # use gallifreydb::query::QueryBuilder;
/// # use gallifreydb::index::vector::DistanceMetric;
/// # fn main() {
/// # let embedding = vec![0.1, 0.2, 0.3];
/// let query = QueryBuilder::new()
///     .find_similar_builder(&embedding, 10)
///         .property("title_embedding")
///         .metric(DistanceMetric::Euclidean)
///         .finish()
///     .build();
/// # }
/// ```
#[must_use = "builders do nothing unless you call finish()"]
pub struct FindSimilarBuilder {
    embedding: Arc<[f32]>,
    k: usize,
    metric: DistanceMetric,
    property_key: Option<String>,
    query_builder: QueryBuilder<state::Initial>,
}

impl FindSimilarBuilder {
    fn new(embedding: &[f32], k: usize, query_builder: QueryBuilder<state::Initial>) -> Self {
        FindSimilarBuilder {
            embedding: Arc::from(embedding),
            k,
            metric: DistanceMetric::Cosine,
            property_key: None,
            query_builder,
        }
    }

    /// Specify which vector property to search.
    ///
    /// Default: "embedding"
    ///
    /// # Example
    ///
    /// ```rust
    /// # use gallifreydb::query::QueryBuilder;
    /// # fn main() {
    /// # let embedding = vec![0.1, 0.2, 0.3];
    /// QueryBuilder::new()
    ///     .find_similar_builder(&embedding, 10)
    ///     .property("body_embedding")
    ///     .finish();
    /// # }
    /// ```
    pub fn property(mut self, key: impl Into<String>) -> Self {
        self.property_key = Some(key.into());
        self
    }

    /// Specify the distance metric to use.
    ///
    /// Default: Cosine
    ///
    /// # Example
    ///
    /// ```rust
    /// # use gallifreydb::query::QueryBuilder;
    /// # use gallifreydb::index::vector::DistanceMetric;
    /// # fn main() {
    /// # let embedding = vec![0.1, 0.2, 0.3];
    /// QueryBuilder::new()
    ///     .find_similar_builder(&embedding, 10)
    ///     .metric(DistanceMetric::Euclidean)
    ///     .finish();
    /// # }
    /// ```
    pub fn metric(mut self, metric: DistanceMetric) -> Self {
        self.metric = metric;
        self
    }

    /// Finish building and add the VectorSearch operation to the query.
    pub fn finish(self) -> QueryBuilder<state::HasVectorResults> {
        self.query_builder.add_op(QueryOp::VectorSearch {
            embedding: self.embedding,
            k: self.k,
            metric: self.metric,
            property_key: self.property_key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::NodeId;

    fn test_node_id() -> NodeId {
        NodeId::new(1).unwrap()
    }

    fn test_embedding() -> [f32; 4] {
        [0.1, 0.2, 0.3, 0.4]
    }

    #[test]
    fn test_simple_node_query() {
        let query = QueryBuilder::new().start(test_node_id()).build();

        assert_eq!(query.operation_count(), 1);
        assert!(!query.is_temporal());
    }

    #[test]
    fn test_traverse_query() {
        let query = QueryBuilder::new()
            .start(test_node_id())
            .traverse("KNOWS")
            .build();

        assert_eq!(query.operation_count(), 2);
    }

    #[test]
    fn test_traverse_and_rank() {
        let embedding = test_embedding();
        let query = QueryBuilder::new()
            .start(test_node_id())
            .traverse("KNOWS")
            .rank_by_similarity(&embedding, 10)
            .build();

        assert_eq!(query.operation_count(), 3);
    }

    #[test]
    fn test_vector_search() {
        let embedding = test_embedding();
        let query = QueryBuilder::new().find_similar(&embedding, 10).build();

        assert_eq!(query.operation_count(), 1);
    }

    #[test]
    fn test_temporal_context() {
        let query = QueryBuilder::new()
            .as_of(1000.into(), 2000.into())
            .start(test_node_id())
            .build();

        assert!(query.is_temporal());
        assert!(
            query
                .temporal_context
                .as_ref()
                .unwrap()
                .as_of_tuple()
                .is_some()
        );
    }

    #[test]
    fn test_temporal_between() {
        let query = QueryBuilder::new()
            .between(1000.into(), 2000.into())
            .start(test_node_id())
            .build();

        assert!(query.is_temporal());
        assert!(
            query
                .temporal_context
                .as_ref()
                .unwrap()
                .valid_time_between
                .is_some()
        );
    }

    #[test]
    fn test_limit_and_skip() {
        let query = QueryBuilder::new()
            .start(test_node_id())
            .skip(10)
            .limit(20)
            .build();

        assert_eq!(query.operation_count(), 3);
    }

    #[test]
    fn test_filter() {
        let query = QueryBuilder::new()
            .start(test_node_id())
            .filter(Predicate::eq("name", "Alice"))
            .build();

        assert_eq!(query.operation_count(), 2);
    }

    #[test]
    fn test_multi_hop_traversal() {
        let query = QueryBuilder::new()
            .start(test_node_id())
            .traverse("KNOWS")
            .traverse("WORKS_AT")
            .build();

        assert_eq!(query.operation_count(), 3);
    }

    #[test]
    fn test_scan_with_label() {
        let query = QueryBuilder::new().scan_label("Person").build();

        assert_eq!(query.operation_count(), 1);
    }

    #[test]
    fn test_full_hybrid_query() {
        let embedding = test_embedding();

        // "Who did Alice know in 2023 that was similar to Bob?"
        let query = QueryBuilder::new()
            .as_of(1000.into(), 2000.into())
            .start(test_node_id())
            .traverse("KNOWS")
            .rank_by_similarity(&embedding, 10)
            .build();

        assert!(query.is_temporal());
        assert_eq!(query.operation_count(), 3);
    }

    #[test]
    fn test_hints() {
        let query = QueryBuilder::new()
            .start(test_node_id())
            .with_hint(IndexHint::UseVectorIndex)
            .parallel()
            .build();

        assert_eq!(query.hints.force_index, Some(IndexHint::UseVectorIndex));
        assert!(query.hints.parallel);
    }

    #[test]
    fn test_chained_filters() {
        let query = QueryBuilder::new()
            .start(test_node_id())
            .traverse("KNOWS")
            .filter(Predicate::eq("status", "active"))
            .with_label("Person")
            .build();

        assert_eq!(query.operation_count(), 4);
    }

    #[test]
    fn test_similar_to_simple() {
        let query = QueryBuilder::new()
            .start(test_node_id())
            .similar_to(test_node_id(), 10)
            .build();

        assert_eq!(query.operation_count(), 2);
        match &query.ops[1] {
            QueryOp::SimilarTo {
                source_node: _,
                k,
                property_key,
                label_filter,
            } => {
                assert_eq!(*k, 10);
                assert!(property_key.is_none(), "Should use default property");
                assert!(label_filter.is_none(), "Should have no label filter");
            }
            _ => panic!("Expected SimilarTo operation"),
        }
    }

    #[test]
    fn test_similar_to_builder_with_property() {
        let query = QueryBuilder::new()
            .start(test_node_id())
            .similar_to_builder(test_node_id(), 10)
            .property("custom_embedding")
            .finish()
            .build();

        match &query.ops[1] {
            QueryOp::SimilarTo {
                property_key,
                label_filter,
                ..
            } => {
                assert_eq!(property_key.as_deref(), Some("custom_embedding"));
                assert!(label_filter.is_none());
            }
            _ => panic!("Expected SimilarTo operation"),
        }
    }

    #[test]
    fn test_similar_to_builder_with_label() {
        let query = QueryBuilder::new()
            .start(test_node_id())
            .similar_to_builder(test_node_id(), 10)
            .label_filter("Document")
            .finish()
            .build();

        match &query.ops[1] {
            QueryOp::SimilarTo {
                property_key,
                label_filter,
                ..
            } => {
                assert!(property_key.is_none());
                assert_eq!(label_filter.as_deref(), Some("Document"));
            }
            _ => panic!("Expected SimilarTo operation"),
        }
    }

    #[test]
    fn test_similar_to_builder_with_all_options() {
        let query = QueryBuilder::new()
            .start(test_node_id())
            .similar_to_builder(test_node_id(), 10)
            .property("custom_embedding")
            .label_filter("Person")
            .finish()
            .build();

        match &query.ops[1] {
            QueryOp::SimilarTo {
                property_key,
                label_filter,
                ..
            } => {
                assert_eq!(property_key.as_deref(), Some("custom_embedding"));
                assert_eq!(label_filter.as_deref(), Some("Person"));
            }
            _ => panic!("Expected SimilarTo operation"),
        }
    }

    #[test]
    fn test_similar_to_builder_fluent_chaining() {
        // Verify builder can be chained with other query operations
        let query = QueryBuilder::new()
            .start(test_node_id())
            .similar_to_builder(test_node_id(), 10)
            .property("embedding")
            .label_filter("Person")
            .finish()
            .filter(Predicate::gt("score", 0.8))
            .limit(5)
            .build();

        assert_eq!(query.operation_count(), 4); // start + similar_to + filter + limit
    }

    #[test]
    #[should_panic(expected = "k must be greater than 0")]
    fn test_similar_to_validates_k() {
        let _ = QueryBuilder::new()
            .start(test_node_id())
            .similar_to(test_node_id(), 0); // Should panic
    }

    #[test]
    #[should_panic(expected = "k must be greater than 0")]
    fn test_similar_to_builder_validates_k() {
        let _ = QueryBuilder::new()
            .start(test_node_id())
            .similar_to_builder(test_node_id(), 0); // Should panic
    }

    // ==================== Provenance Tests ====================

    #[test]
    fn test_with_provenance() {
        let query = QueryBuilder::new()
            .start(test_node_id())
            .with_provenance()
            .build();

        assert!(query.hints.include_provenance);
    }

    #[test]
    fn test_with_provenance_fluent_chaining() {
        let query = QueryBuilder::new()
            .start(test_node_id())
            .traverse("KNOWS")
            .with_provenance()
            .limit(10)
            .build();

        assert!(query.hints.include_provenance);
        assert_eq!(query.operation_count(), 3); // start + traverse + limit
    }

    #[test]
    fn test_without_provenance_default() {
        let query = QueryBuilder::new().start(test_node_id()).build();

        assert!(!query.hints.include_provenance);
    }

    // ==================== RankBySimilarity Property Tests (Issue #389) ====================

    #[test]
    fn test_rank_by_similarity_simple_uses_default_property() {
        let embedding = test_embedding();
        let query = QueryBuilder::new()
            .start(test_node_id())
            .traverse("KNOWS")
            .rank_by_similarity(&embedding, 10)
            .build();

        match &query.ops[2] {
            QueryOp::RankBySimilarity {
                top_k,
                property_key,
                ..
            } => {
                assert_eq!(*top_k, Some(10));
                assert!(property_key.is_none(), "Should use default property");
            }
            _ => panic!("Expected RankBySimilarity operation"),
        }
    }

    #[test]
    fn test_rank_by_similarity_builder_with_property() {
        let embedding = test_embedding();
        let query = QueryBuilder::new()
            .start(test_node_id())
            .traverse("KNOWS")
            .rank_by_similarity_builder(&embedding, 10)
            .property("custom_embedding")
            .finish()
            .build();

        match &query.ops[2] {
            QueryOp::RankBySimilarity { property_key, .. } => {
                assert_eq!(property_key.as_deref(), Some("custom_embedding"));
            }
            _ => panic!("Expected RankBySimilarity operation"),
        }
    }

    #[test]
    fn test_rank_by_similarity_builder_fluent_chaining() {
        let embedding = test_embedding();
        let query = QueryBuilder::new()
            .start(test_node_id())
            .traverse("KNOWS")
            .rank_by_similarity_builder(&embedding, 10)
            .property("title_embedding")
            .finish()
            .filter(Predicate::gt("score", 0.8))
            .limit(5)
            .build();

        assert_eq!(query.operation_count(), 5); // start + traverse + rank + filter + limit
    }

    // ==================== VectorSearch Property Tests (Issue #389) ====================

    #[test]
    fn test_find_similar_simple_uses_default() {
        let embedding = test_embedding();
        let query = QueryBuilder::new().find_similar(&embedding, 10).build();

        match &query.ops[0] {
            QueryOp::VectorSearch { property_key, .. } => {
                assert!(property_key.is_none(), "Should use default property");
            }
            _ => panic!("Expected VectorSearch operation"),
        }
    }

    #[test]
    fn test_find_similar_builder_with_property() {
        let embedding = test_embedding();
        let query = QueryBuilder::new()
            .find_similar_builder(&embedding, 10)
            .property("body_embedding")
            .finish()
            .build();

        match &query.ops[0] {
            QueryOp::VectorSearch { property_key, .. } => {
                assert_eq!(property_key.as_deref(), Some("body_embedding"));
            }
            _ => panic!("Expected VectorSearch operation"),
        }
    }

    #[test]
    fn test_find_similar_builder_with_metric() {
        use crate::index::vector::DistanceMetric;

        let embedding = test_embedding();
        let query = QueryBuilder::new()
            .find_similar_builder(&embedding, 10)
            .metric(DistanceMetric::Euclidean)
            .finish()
            .build();

        match &query.ops[0] {
            QueryOp::VectorSearch { metric, .. } => {
                assert_eq!(*metric, DistanceMetric::Euclidean);
            }
            _ => panic!("Expected VectorSearch operation"),
        }
    }

    // ==================== Phase 7: Independent Dimension Query Methods ====================

    #[test]
    fn test_as_of_valid_time_only() {
        use crate::core::hlc::HybridTimestamp;

        let ts = HybridTimestamp::new(1000, 0).unwrap();
        let query = QueryBuilder::new()
            .as_of_valid_time(ts)
            .start(test_node_id())
            .build();

        assert!(query.is_temporal());
        let ctx = query.temporal_context.as_ref().unwrap();
        assert_eq!(ctx.valid_time_as_of, Some(ts));
        assert_eq!(ctx.transaction_time_as_of, None); // Not set
    }

    #[test]
    fn test_as_of_transaction_time_only() {
        use crate::core::hlc::HybridTimestamp;

        let ts = HybridTimestamp::new(1000, 0).unwrap();
        let query = QueryBuilder::new()
            .as_of_transaction_time(ts)
            .start(test_node_id())
            .build();

        assert!(query.is_temporal());
        let ctx = query.temporal_context.as_ref().unwrap();
        assert_eq!(ctx.transaction_time_as_of, Some(ts));
        assert_eq!(ctx.valid_time_as_of, None); // Not set
    }

    #[test]
    fn test_valid_time_between_method() {
        use crate::core::hlc::HybridTimestamp;

        let start = HybridTimestamp::new(1000, 0).unwrap();
        let end = HybridTimestamp::new(2000, 0).unwrap();

        let query = QueryBuilder::new()
            .valid_time_between(start, end)
            .start(test_node_id())
            .build();

        assert!(query.is_temporal());
        let ctx = query.temporal_context.as_ref().unwrap();
        assert!(ctx.valid_time_between.is_some());
        assert!(ctx.transaction_time_between.is_none());

        let range = ctx.valid_time_between.as_ref().unwrap();
        assert_eq!(range.start(), start);
        assert_eq!(range.end(), end);
    }

    #[test]
    fn test_transaction_time_between_method() {
        use crate::core::hlc::HybridTimestamp;

        let start = HybridTimestamp::new(1000, 0).unwrap();
        let end = HybridTimestamp::new(2000, 0).unwrap();

        let query = QueryBuilder::new()
            .transaction_time_between(start, end)
            .start(test_node_id())
            .build();

        assert!(query.is_temporal());
        let ctx = query.temporal_context.as_ref().unwrap();
        assert!(ctx.transaction_time_between.is_some());
        assert!(ctx.valid_time_between.is_none());

        let range = ctx.transaction_time_between.as_ref().unwrap();
        assert_eq!(range.start(), start);
        assert_eq!(range.end(), end);
    }

    #[test]
    fn test_combine_both_dimensions_with_as_of() {
        use crate::core::hlc::HybridTimestamp;

        let valid_ts = HybridTimestamp::new(1000, 0).unwrap();
        let tx_ts = HybridTimestamp::new(2000, 0).unwrap();

        // Using the combined as_of method (backward compatibility test)
        let query = QueryBuilder::new()
            .as_of(valid_ts, tx_ts)
            .start(test_node_id())
            .build();

        assert!(query.is_temporal());
        let ctx = query.temporal_context.as_ref().unwrap();
        assert_eq!(ctx.valid_time_as_of, Some(valid_ts));
        assert_eq!(ctx.transaction_time_as_of, Some(tx_ts));
    }

    #[test]
    fn test_combine_both_dimensions_independently() {
        use crate::core::hlc::HybridTimestamp;

        let valid_ts = HybridTimestamp::new(1000, 0).unwrap();
        let tx_ts = HybridTimestamp::new(2000, 0).unwrap();

        // Using separate methods should achieve same result
        let query = QueryBuilder::new()
            .as_of_valid_time(valid_ts)
            .as_of_transaction_time(tx_ts)
            .start(test_node_id())
            .build();

        assert!(query.is_temporal());
        let ctx = query.temporal_context.as_ref().unwrap();
        assert_eq!(ctx.valid_time_as_of, Some(valid_ts));
        assert_eq!(ctx.transaction_time_as_of, Some(tx_ts));
    }

    #[test]
    fn test_temporal_methods_fluent_chaining() {
        use crate::core::hlc::HybridTimestamp;

        let valid_ts = HybridTimestamp::new(1000, 0).unwrap();

        // Should work with all query operations
        let query = QueryBuilder::new()
            .as_of_valid_time(valid_ts)
            .start(test_node_id())
            .traverse("KNOWS")
            .filter(Predicate::eq("status", "active"))
            .limit(10)
            .build();

        assert!(query.is_temporal());
        assert_eq!(query.operation_count(), 4); // start + traverse + filter + limit
    }

    #[test]
    fn test_temporal_method_overwrites_previous() {
        use crate::core::hlc::HybridTimestamp;

        let ts1 = HybridTimestamp::new(1000, 0).unwrap();
        let ts2 = HybridTimestamp::new(2000, 0).unwrap();

        // Later call should overwrite earlier
        let query = QueryBuilder::new()
            .as_of_valid_time(ts1)
            .as_of_valid_time(ts2) // This should overwrite ts1
            .start(test_node_id())
            .build();

        let ctx = query.temporal_context.as_ref().unwrap();
        assert_eq!(ctx.valid_time_as_of, Some(ts2)); // Should be ts2, not ts1
    }

    #[test]
    fn test_mixed_range_and_point_queries() {
        use crate::core::hlc::HybridTimestamp;

        let point_ts = HybridTimestamp::new(1500, 0).unwrap();
        let range_start = HybridTimestamp::new(1000, 0).unwrap();
        let range_end = HybridTimestamp::new(2000, 0).unwrap();

        // Valid time as point, transaction time as range
        let query = QueryBuilder::new()
            .as_of_valid_time(point_ts)
            .transaction_time_between(range_start, range_end)
            .start(test_node_id())
            .build();

        let ctx = query.temporal_context.as_ref().unwrap();
        assert_eq!(ctx.valid_time_as_of, Some(point_ts));
        assert!(ctx.transaction_time_between.is_some());

        let tx_range = ctx.transaction_time_between.as_ref().unwrap();
        assert_eq!(tx_range.start(), range_start);
        assert_eq!(tx_range.end(), range_end);
    }
}
