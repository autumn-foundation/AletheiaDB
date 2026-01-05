//! Vector index abstraction for approximate nearest neighbor search.
//!
//! This module provides a trait-based abstraction for vector indexes, enabling multiple
//! implementation strategies (HNSW, IVF, etc.) while maintaining a consistent interface
//! for the query engine.
//!
//! For the complete vector search architecture and implementation plan, see
//! [`docs/VECTOR_SEARCH_DESIGN.md`](../../docs/VECTOR_SEARCH_DESIGN.md).
//!
//! # Overview
//!
//! The `VectorIndex` trait defines the core operations for managing and querying vector
//! embeddings:
//! - **Adding vectors**: Associate embeddings with node IDs
//! - **Removing vectors**: Delete embeddings from the index
//! - **Searching**: Find k-nearest neighbors by similarity
//! - **Filtered search**: Search with custom predicates
//! - **Distance metric**: Query which similarity metric is used
//!
//! # Implementation Strategies
//!
//! Different index implementations offer various trade-offs:
//!
//! | Strategy | Build Time | Query Time (avg) | Memory | Use Case |
//! |----------|-----------|------------------|---------|----------|
//! | HNSW | O(n log n) | O(log n) | High | General purpose, high recall |
//! | IVF | O(n) | O(√n) | Medium | Large datasets, approximate |
//! | Flat | O(1) | O(n) | Low | Small datasets, exact search |
//!
//! **Note**: HNSW worst-case query time is O(n), though average case is O(log n).
//! Performance also depends on vector dimensionality.
//!
//! # Input Validation Requirements
//!
//! Implementations must validate all inputs to prevent invalid state and DoS attacks:
//!
//! - **Vector validation**: Check for NaN/Infinity using `validate_vector()` from `core::vector`
//! - **Dimension matching**: Ensure all vectors match index dimensionality
//! - **k bounds**: Limit `k` parameter (e.g., max 10,000) to prevent excessive memory allocation
//! - **Empty vectors**: Reject zero-length vectors
//!
//! Violations should return specific errors (see Error Handling below).
//!
//! # Examples
//!
//! ```no_run
//! // Examples use `no_run` because no VectorIndex implementation exists yet.
//! // VS-022 will add the HNSW implementation using usearch.
//!
//! use gallifreydb::index::VectorIndex;
//! use gallifreydb::core::id::NodeId;
//!
//! fn search_similar_documents(
//!     index: &impl VectorIndex,
//!     query_embedding: &[f32],
//!     limit: usize
//! ) -> gallifreydb::utils::Result<Vec<(NodeId, f32)>> {
//!     // Find top-k most similar documents
//!     let results = index.search(query_embedding, limit)?;
//!
//!     // Results are sorted by similarity (highest first)
//!     for (node_id, similarity) in &results {
//!         println!("Node {:?}: similarity = {}", node_id, similarity);
//!     }
//!
//!     Ok(results)
//! }
//!
//! fn search_with_constraint(
//!     index: &impl VectorIndex,
//!     query: &[f32],
//!     allowed_ids: &[NodeId],
//!     k: usize
//! ) -> gallifreydb::utils::Result<Vec<(NodeId, f32)>> {
//!     // Search only within a subset of nodes
//!     index.search_with_filter(query, k, |id| allowed_ids.contains(id))
//! }
//! ```
//!
//! # Phase 2 Implementation
//!
//! Phase 2 of vector search will implement this trait using HNSW (Hierarchical
//! Navigable Small World) via the `usearch` crate, which provides:
//! - Typical query latency: 100µs-1ms (depends on dataset size and dimensionality)
//! - High recall (>95% for typical configurations)
//! - Memory-efficient graph structure
//! - Configurable index parameters (M, ef_construction, ef_search)
//! - Concurrent insertions via internal locking
//!
//! See [`docs/VECTOR_SEARCH_DESIGN.md`](../../docs/VECTOR_SEARCH_DESIGN.md) for complete architecture.

use crate::core::id::NodeId;
use crate::utils::Result;

/// Distance metric used for similarity computation.
///
/// Different metrics are suitable for different use cases:
/// - **Cosine**: Measures angle between vectors, ignores magnitude (semantic similarity)
/// - **Euclidean**: Measures straight-line distance (spatial data, clustering)
/// - **DotProduct**: Inner product, preserves magnitude (MaxSim, ColBERT)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    /// Cosine similarity: measures angle between vectors, range [-1, 1]
    Cosine,
    /// Euclidean distance (L2): measures straight-line distance, range [0, ∞)
    Euclidean,
    /// Dot product: inner product of vectors, range (-∞, ∞)
    DotProduct,
}

/// Trait for vector indexes supporting approximate k-nearest neighbor search.
///
/// This trait abstracts over different vector index implementations, allowing
/// GallifreyDB to support multiple ANN (Approximate Nearest Neighbor) algorithms
/// while maintaining a consistent query interface.
///
/// # Invariants
///
/// Implementations must maintain these invariants:
/// - All vectors in the index have the same dimensionality (returned by `dimensions()`)
/// - `search()` returns at most `k` results, sorted by similarity (descending)
/// - `search_with_filter()` only returns results where the predicate returns `true`
/// - `len()` returns the exact number of vectors currently in the index
/// - Adding the same NodeId twice replaces the previous vector
/// - The distance metric never changes after index creation
///
/// # Thread Safety
///
/// Implementations must be thread-safe for both concurrent reads and writes.
/// The trait methods take `&self` (not `&mut self`) to enable concurrent operations.
/// Implementations should use interior mutability (e.g., internal locks, atomics) to
/// coordinate concurrent access. For example, usearch supports concurrent insertions
/// through internal locking while maintaining `&self` semantics.
///
/// # Performance Expectations
///
/// For HNSW implementation (Phase 2):
/// - **Insert**: O(log n) average case with configurable ef_construction parameter
/// - **Search**: O(log n) average case, O(n) worst case, with configurable ef_search
/// - **Memory**: O(n * M * d) where M is connections per node, d is dimensions
/// - **Query latency**: 100µs-1ms for typical datasets (<10M vectors, 384-1536 dims)
/// - **Dimensionality impact**: Linear scaling with dimensions for both time and memory
///
/// # Error Handling
///
/// Methods return specific error variants for validation failures:
/// - `Error::Vector(VectorError::DimensionMismatch { expected, actual })` - Wrong dimensions
/// - `Error::Vector(VectorError::ContainsNaN { count })` - Vector contains NaN values
/// - `Error::Vector(VectorError::ContainsInfinity { count })` - Vector contains Infinity
/// - `Error::Vector(VectorError::DimensionTooLarge { ... })` - Vector too large
pub trait VectorIndex: Send + Sync {
    /// Adds a vector to the index, associating it with the given node ID.
    ///
    /// If a vector with the same `id` already exists, it will be replaced.
    /// The vector dimensions must match the index's configured dimensionality.
    ///
    /// # Arguments
    ///
    /// * `id` - The node ID to associate with this vector
    /// * `vector` - The embedding vector (must match index dimensions)
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the vector was successfully added
    /// - `Err(Error::Vector(VectorError::DimensionMismatch))` if dimensions don't match
    /// - `Err(Error::Vector(VectorError::ContainsNaN))` if vector contains NaN
    /// - `Err(Error::Vector(VectorError::ContainsInfinity))` if vector contains Infinity
    ///
    /// # Examples
    ///
    /// ```no_run
    /// // Example uses `no_run` - no implementation exists until VS-022
    /// # use gallifreydb::index::VectorIndex;
    /// # use gallifreydb::core::id::NodeId;
    /// # fn example(index: &impl VectorIndex) -> gallifreydb::utils::Result<()> {
    /// let node_id = NodeId::new(123).unwrap();
    /// let embedding = vec![0.1, 0.2, 0.3, 0.4];
    /// index.add(node_id, &embedding)?;
    /// # Ok(())
    /// # }
    /// ```
    fn add(&self, id: NodeId, vector: &[f32]) -> Result<()>;

    /// Removes a vector from the index by node ID.
    ///
    /// If the ID does not exist in the index, this is a no-op (returns Ok).
    ///
    /// # Arguments
    ///
    /// * `id` - The node ID to remove
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the vector was removed or didn't exist
    /// - `Err(_)` if the underlying index encounters an error
    ///
    /// # Examples
    ///
    /// ```no_run
    /// // Example uses `no_run` - no implementation exists until VS-022
    /// # use gallifreydb::index::VectorIndex;
    /// # use gallifreydb::core::id::NodeId;
    /// # fn example(index: &impl VectorIndex) -> gallifreydb::utils::Result<()> {
    /// let node_id = NodeId::new(123).unwrap();
    /// index.remove(node_id)?;
    /// # Ok(())
    /// # }
    /// ```
    fn remove(&self, id: NodeId) -> Result<()>;

    /// Searches for the k-nearest neighbors of the query vector.
    ///
    /// Returns up to `k` results sorted by similarity in descending order
    /// (highest similarity first). The similarity score interpretation depends on the
    /// configured distance metric (query via `distance_metric()`).
    ///
    /// # Arguments
    ///
    /// * `query` - The query embedding vector (must match index dimensions)
    /// * `k` - Maximum number of results to return (implementations may cap this,
    ///   e.g., at 10,000 to prevent DoS)
    ///
    /// # Returns
    ///
    /// A vector of (NodeId, similarity) pairs, sorted by similarity (descending).
    /// May contain fewer than `k` results if the index has fewer vectors.
    ///
    /// # Errors
    ///
    /// - `Err(Error::Vector(VectorError::DimensionMismatch))` if query dimensions don't match
    /// - `Err(Error::Vector(VectorError::ContainsNaN))` if query contains NaN
    /// - `Err(Error::Vector(VectorError::ContainsInfinity))` if query contains Infinity
    ///
    /// # Examples
    ///
    /// ```no_run
    /// // Example uses `no_run` - no implementation exists until VS-022
    /// # use gallifreydb::index::VectorIndex;
    /// # fn example(index: &impl VectorIndex) -> gallifreydb::utils::Result<()> {
    /// let query = vec![0.5, 0.3, 0.1, 0.9];
    /// let results = index.search(&query, 10)?;
    ///
    /// for (node_id, similarity) in results {
    ///     println!("Found node {:?} with similarity {}", node_id, similarity);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>>;

    /// Searches for k-nearest neighbors with a filter predicate.
    ///
    /// Like `search()`, but only returns results where `predicate(node_id)` returns `true`.
    /// This enables filtered search without materializing the full result set.
    ///
    /// # Arguments
    ///
    /// * `query` - The query embedding vector (must match index dimensions)
    /// * `k` - Maximum number of results to return
    /// * `predicate` - Filter function that returns true for nodes to include
    ///
    /// # Returns
    ///
    /// A vector of (NodeId, similarity) pairs where the predicate returned true,
    /// sorted by similarity (descending).
    ///
    /// # Errors
    ///
    /// Same as `search()`.
    ///
    /// # Performance
    ///
    /// **WARNING**: Low-selectivity filters (<1%) can degrade to nearly linear scan performance.
    /// The implementation must examine many candidates to find `k` results that satisfy
    /// the predicate. For example, with 1% selectivity, finding 100 results may require
    /// examining ~10,000 candidates.
    ///
    /// **Alternatives for better performance**:
    /// - Maintain separate indexes per category if filtering is common
    /// - Use hybrid graph+vector queries to pre-filter via graph traversal
    /// - Consider the `search()` method and filter results in-memory if selectivity is very low
    ///
    /// # Examples
    ///
    /// ```no_run
    /// // Example uses `no_run` - no implementation exists until VS-022
    /// # use gallifreydb::index::VectorIndex;
    /// # use gallifreydb::core::id::NodeId;
    /// # use std::collections::HashSet;
    /// # fn example(index: &impl VectorIndex) -> gallifreydb::utils::Result<()> {
    /// let query = vec![0.5, 0.3, 0.1, 0.9];
    /// let allowed = HashSet::from([NodeId::new(1).unwrap(), NodeId::new(5).unwrap(), NodeId::new(10).unwrap()]);
    ///
    /// // Only search within allowed nodes
    /// let results = index.search_with_filter(&query, 5, |id| allowed.contains(id))?;
    /// # Ok(())
    /// # }
    /// ```
    fn search_with_filter<F>(
        &self,
        query: &[f32],
        k: usize,
        predicate: F,
    ) -> Result<Vec<(NodeId, f32)>>
    where
        F: Fn(&NodeId) -> bool + Send + Sync;

    /// Returns the number of vectors currently in the index.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// // Example uses `no_run` - no implementation exists until VS-022
    /// # use gallifreydb::index::VectorIndex;
    /// # fn example(index: &impl VectorIndex) {
    /// println!("Index contains {} vectors", index.len());
    /// # }
    /// ```
    #[must_use]
    fn len(&self) -> usize;

    /// Returns the dimensionality of vectors in this index.
    ///
    /// All vectors added to the index must have this many dimensions.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// // Example uses `no_run` - no implementation exists until VS-022
    /// # use gallifreydb::index::VectorIndex;
    /// # fn example(index: &impl VectorIndex) {
    /// let dims = index.dimensions();
    /// println!("This index accepts {}-dimensional vectors", dims);
    /// # }
    /// ```
    #[must_use]
    fn dimensions(&self) -> usize;

    /// Returns the distance metric used by this index.
    ///
    /// The distance metric determines how similarity scores are computed and
    /// how to interpret the `f32` values returned by `search()` methods.
    ///
    /// # Metric Interpretation
    ///
    /// - **Cosine**: Returns cosine similarity in range [-1, 1]. Higher is more similar.
    ///   Typical threshold for "similar": >0.7
    /// - **Euclidean**: Returns negative squared L2 distance in range (-∞, 0]. Higher is more similar.
    ///   (More negative = more distant, closer to 0 = more similar)
    ///   Threshold depends on embedding magnitude and dimensionality.
    /// - **DotProduct**: Returns inner product in range (-∞, ∞). Higher is more similar.
    ///   Threshold depends on embedding normalization.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// // Example uses `no_run` - no implementation exists until VS-022
    /// # use gallifreydb::index::{VectorIndex, DistanceMetric};
    /// # fn example(index: &impl VectorIndex) {
    /// match index.distance_metric() {
    ///     DistanceMetric::Cosine => println!("Using cosine similarity"),
    ///     DistanceMetric::Euclidean => println!("Using Euclidean distance"),
    ///     DistanceMetric::DotProduct => println!("Using dot product"),
    /// }
    /// # }
    /// ```
    #[must_use]
    fn distance_metric(&self) -> DistanceMetric;

    /// Returns true if the index is empty.
    ///
    /// Equivalent to `self.len() == 0`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// // Example uses `no_run` - no implementation exists until VS-022
    /// # use gallifreydb::index::VectorIndex;
    /// # fn example(index: &impl VectorIndex) {
    /// if index.is_empty() {
    ///     println!("No vectors indexed yet");
    /// }
    /// # }
    /// ```
    #[must_use]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: VectorIndex is NOT object-safe due to the generic `search_with_filter` method.
    // This is acceptable because:
    // 1. The trait will be used with concrete types (HnswIndex), not trait objects
    // 2. The generic method is necessary for zero-cost predicate abstraction
    // 3. We can use `impl VectorIndex` or `Box<impl VectorIndex>` if needed
    //
    // If object safety becomes necessary, we could:
    // - Move search_with_filter to a separate trait
    // - Use Box<dyn Fn> for the predicate (runtime cost)

    #[test]
    fn test_distance_metric_debug() {
        // Verify DistanceMetric derives work correctly
        let metric = DistanceMetric::Cosine;
        assert_eq!(format!("{:?}", metric), "Cosine");
        assert_eq!(metric, DistanceMetric::Cosine);
        assert_ne!(metric, DistanceMetric::Euclidean);
    }
}

// HNSW implementation
pub mod hnsw;

// Re-export HNSW types for convenience
pub use hnsw::{HnswConfig, HnswIndex, HnswIndexBuilder};
