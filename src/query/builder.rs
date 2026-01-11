//! Fluent Query Builder
//!
//! Provides a type-safe, fluent API for constructing hybrid queries.
//! The builder uses phantom types to track query state at compile time,
//! preventing invalid query compositions.
//!
//! # Example
//!
//! ```rust,ignore
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
//! ```

use std::marker::PhantomData;
use std::sync::Arc;

use crate::core::NodeId;
use crate::core::temporal::{TimeRange, Timestamp};
use crate::index::vector::DistanceMetric;

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

    /// Start with a vector similarity search
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
        })
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

    /// Rank current nodes by similarity to an embedding
    #[must_use]
    pub fn rank_by_similarity(
        self,
        embedding: &[f32],
        top_k: usize,
    ) -> QueryBuilder<state::HasVectorResults> {
        self.add_op(QueryOp::RankBySimilarity {
            embedding: Arc::from(embedding),
            top_k: Some(top_k),
        })
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
    /// ```ignore
    /// let results = db.query()
    ///     .start(node_id)
    ///     .similar_to(bob_id, 10)
    ///     .execute()?;
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
    /// ```ignore
    /// // Find similar documents using custom embedding
    /// let results = db.query()
    ///     .start(alice_id)
    ///     .similar_to_builder(bob_id, 10)
    ///         .property("custom_embedding")
    ///         .label_filter("Document")
    ///         .finish()
    ///     .execute()?;
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

    /// Rank traversal results by similarity
    #[must_use]
    pub fn rank_by_similarity(
        self,
        embedding: &[f32],
        top_k: usize,
    ) -> QueryBuilder<state::HasVectorResults> {
        self.add_op(QueryOp::RankBySimilarity {
            embedding: Arc::from(embedding),
            top_k: Some(top_k),
        })
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

    /// Set temporal context: query across a time range
    #[must_use]
    pub fn between(mut self, start: Timestamp, end: Timestamp) -> Self {
        self.temporal_context = Some(TemporalContext::between(
            TimeRange::between(start, end).unwrap(),
        ));
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
/// ```ignore
/// let query = QueryBuilder::new()
///     .start(alice_id)
///     .similar_to_builder(bob_id, 10)
///         .property("custom_embedding")
///         .label_filter("Person")
///         .finish()
///     .build();
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
    /// ```ignore
    /// .similar_to_builder(node_id, 10)
    ///     .property("custom_embedding")
    ///     .finish()
    /// ```
    pub fn property(mut self, key: impl Into<String>) -> Self {
        self.property_key = Some(key.into());
        self
    }

    /// Filter results to only include nodes with this label.
    ///
    /// # Example
    ///
    /// ```ignore
    /// .similar_to_builder(node_id, 10)
    ///     .label_filter("Document")
    ///     .finish()
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
            .as_of(1000, 2000)
            .start(test_node_id())
            .build();

        assert!(query.is_temporal());
        assert!(query.temporal_context.as_ref().unwrap().as_of.is_some());
    }

    #[test]
    fn test_temporal_between() {
        let query = QueryBuilder::new()
            .between(1000, 2000)
            .start(test_node_id())
            .build();

        assert!(query.is_temporal());
        assert!(query.temporal_context.as_ref().unwrap().between.is_some());
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
            .as_of(1000, 2000)
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
}
