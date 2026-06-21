//! Unified vector-similarity query builder (Issue #352).
//!
//! Historically, AletheiaDB exposed several closely-related vector search methods
//! (`find_similar`, `find_similar_with_label`, `find_similar_by_embedding`,
//! `find_similar_by_embedding_with_label`, `find_similar_as_of`) whose names drifted
//! apart and which would multiply combinatorially as new filters were added.
//!
//! [`SimilarityQuery`] replaces that surface with a single extensible builder that is
//! passed to [`AletheiaDB::similarity_search`](crate::AletheiaDB::similarity_search).
//! Adding a new filter (e.g. a property selector or a score threshold) becomes a new
//! builder method rather than a new top-level database method.
//!
//! # Examples
//!
//! ```ignore
//! use aletheiadb::SimilarityQuery;
//!
//! // Most similar nodes to an existing node.
//! let similar = db.similarity_search(SimilarityQuery::from_node(node_id).k(10))?;
//!
//! // Most similar Documents to a raw embedding.
//! let similar = db.similarity_search(
//!     SimilarityQuery::from_embedding(&embedding)
//!         .k(10)
//!         .with_label("Document"),
//! )?;
//!
//! // Temporal search at a point in time.
//! let similar = db.similarity_search(
//!     SimilarityQuery::from_embedding(&embedding).k(10).at_time(timestamp),
//! )?;
//! ```

use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;

/// Default number of neighbours returned when [`SimilarityQuery::k`] is not set.
pub const DEFAULT_K: usize = 10;

/// The origin of a similarity search: either an existing node or a raw embedding.
#[derive(Debug, Clone, PartialEq)]
pub enum SimilaritySource {
    /// Search for nodes similar to an existing node's indexed embedding.
    Node(NodeId),
    /// Search for nodes similar to an externally-supplied embedding vector.
    Embedding(Vec<f32>),
}

/// A declarative, extensible description of a vector similarity search.
///
/// Construct with [`SimilarityQuery::from_node`] or [`SimilarityQuery::from_embedding`],
/// refine with the fluent setters, then pass it to
/// [`AletheiaDB::similarity_search`](crate::AletheiaDB::similarity_search).
#[must_use = "SimilarityQuery does nothing unless passed to db.similarity_search()"]
#[derive(Debug, Clone, PartialEq)]
pub struct SimilarityQuery {
    source: SimilaritySource,
    k: usize,
    label: Option<String>,
    timestamp: Option<Timestamp>,
}

impl SimilarityQuery {
    /// Search for nodes similar to an existing node's indexed embedding.
    ///
    /// The query node itself is excluded from the results.
    pub fn from_node(node_id: NodeId) -> Self {
        Self {
            source: SimilaritySource::Node(node_id),
            k: DEFAULT_K,
            label: None,
            timestamp: None,
        }
    }

    /// Search for nodes similar to an externally-supplied embedding vector.
    ///
    /// Useful for query embeddings that don't correspond to any node in the graph.
    pub fn from_embedding(embedding: impl Into<Vec<f32>>) -> Self {
        Self {
            source: SimilaritySource::Embedding(embedding.into()),
            k: DEFAULT_K,
            label: None,
            timestamp: None,
        }
    }

    /// Set the maximum number of results to return (defaults to [`DEFAULT_K`]).
    pub fn k(mut self, k: usize) -> Self {
        self.k = k;
        self
    }

    /// Restrict results to nodes carrying the given label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Perform the search against the temporal vector index as of `timestamp`.
    pub fn at_time(mut self, timestamp: impl Into<Timestamp>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
    }

    /// The configured search origin.
    pub(crate) fn source(&self) -> &SimilaritySource {
        &self.source
    }

    /// The configured result limit.
    pub(crate) fn limit(&self) -> usize {
        self.k
    }

    /// The configured label filter, if any.
    pub(crate) fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// The configured temporal point, if any.
    pub(crate) fn timestamp(&self) -> Option<Timestamp> {
        self.timestamp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_embedding_sets_defaults() {
        let q = SimilarityQuery::from_embedding(vec![0.1, 0.2, 0.3]);
        assert_eq!(q.limit(), DEFAULT_K);
        assert_eq!(q.label(), None);
        assert_eq!(q.timestamp(), None);
        assert_eq!(
            q.source(),
            &SimilaritySource::Embedding(vec![0.1, 0.2, 0.3])
        );
    }

    #[test]
    fn setters_are_fluent() {
        let ts = crate::core::temporal::time::now();
        let q = SimilarityQuery::from_embedding(&[1.0f32, 0.0][..])
            .k(7)
            .with_label("Person")
            .at_time(ts);
        assert_eq!(q.limit(), 7);
        assert_eq!(q.label(), Some("Person"));
        assert_eq!(q.timestamp(), Some(ts));
    }
}
