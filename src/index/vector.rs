//! Vector index abstraction for approximate nearest neighbor search.
//!
//! This module provides a trait-based abstraction for vector indexes, enabling multiple
//! implementation strategies (HNSW, IVF, etc.) while maintaining a consistent interface
//! for the query engine.
//!
//! # Overview
//!
//! The `VectorIndex` trait defines the core operations for managing and querying vector
//! embeddings:
//! - **Adding vectors**: Associate embeddings with node IDs
//! - **Removing vectors**: Delete embeddings from the index
//! - **Searching**: Find k-nearest neighbors by similarity
//! - **Filtered search**: Search with custom predicates
//!
//! # Implementation Strategies
//!
//! Different index implementations offer various trade-offs:
//!
//! | Strategy | Build Time | Query Time | Memory | Use Case |
//! |----------|-----------|------------|---------|----------|
//! | HNSW | O(n log n) | O(log n) | High | General purpose, high recall |
//! | IVF | O(n) | O(√n) | Medium | Large datasets, approximate |
//! | Flat | O(1) | O(n) | Low | Small datasets, exact search |
//!
//! # Examples
//!
//! ```no_run
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
//! - Sub-millisecond query latency
//! - High recall (>95% for typical configurations)
//! - Memory-efficient graph structure
//! - Configurable index parameters (M, ef_construction, ef_search)
//!
//! See [VECTOR_SEARCH_DESIGN.md](../../docs/VECTOR_SEARCH_DESIGN.md) for complete architecture.

use crate::core::id::NodeId;
use crate::utils::Result;

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
/// - **Insert**: O(log n) with configurable ef_construction parameter
/// - **Search**: O(log n) with configurable ef_search parameter
/// - **Memory**: O(n * M) where M is the number of connections per node
/// - **Query latency**: Sub-millisecond for typical datasets (<10M vectors)
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
    /// - `Err(Error::Vector(VectorError::DimensionMismatch))` if vector dimensions don't match
    /// - `Err(Error::Vector(VectorError::InvalidVector))` if vector contains NaN/Infinity
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use gallifreydb::index::VectorIndex;
    /// # use gallifreydb::core::id::NodeId;
    /// # fn example(index: &impl VectorIndex) -> gallifreydb::utils::Result<()> {
    /// let node_id = NodeId::new(123);
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
    /// # use gallifreydb::index::VectorIndex;
    /// # use gallifreydb::core::id::NodeId;
    /// # fn example(index: &impl VectorIndex) -> gallifreydb::utils::Result<()> {
    /// let node_id = NodeId::new(123);
    /// index.remove(node_id)?;
    /// # Ok(())
    /// # }
    /// ```
    fn remove(&self, id: NodeId) -> Result<()>;

    /// Searches for the k-nearest neighbors of the query vector.
    ///
    /// Returns up to `k` results sorted by similarity in descending order
    /// (highest similarity first). The similarity score depends on the configured
    /// distance metric (cosine, Euclidean, etc.).
    ///
    /// # Arguments
    ///
    /// * `query` - The query embedding vector (must match index dimensions)
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A vector of (NodeId, similarity) pairs, sorted by similarity (descending).
    /// May contain fewer than `k` results if the index has fewer vectors.
    ///
    /// # Errors
    ///
    /// - `Err(Error::Vector(VectorError::DimensionMismatch))` if query dimensions don't match
    /// - `Err(Error::Vector(VectorError::InvalidVector))` if query contains NaN/Infinity
    ///
    /// # Examples
    ///
    /// ```no_run
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
    /// The implementation may need to examine more than `k` candidates to find `k`
    /// results that satisfy the predicate. Consider the selectivity of your predicate
    /// when choosing `k`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use gallifreydb::index::VectorIndex;
    /// # use gallifreydb::core::id::NodeId;
    /// # use std::collections::HashSet;
    /// # fn example(index: &impl VectorIndex) -> gallifreydb::utils::Result<()> {
    /// let query = vec![0.5, 0.3, 0.1, 0.9];
    /// let allowed = HashSet::from([NodeId::new(1), NodeId::new(5), NodeId::new(10)]);
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
    /// # use gallifreydb::index::VectorIndex;
    /// # fn example(index: &impl VectorIndex) {
    /// println!("Index contains {} vectors", index.len());
    /// # }
    /// ```
    fn len(&self) -> usize;

    /// Returns the dimensionality of vectors in this index.
    ///
    /// All vectors added to the index must have this many dimensions.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use gallifreydb::index::VectorIndex;
    /// # fn example(index: &impl VectorIndex) {
    /// let dims = index.dimensions();
    /// println!("This index accepts {}-dimensional vectors", dims);
    /// # }
    /// ```
    fn dimensions(&self) -> usize;

    /// Returns true if the index is empty.
    ///
    /// Equivalent to `self.len() == 0`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use gallifreydb::index::VectorIndex;
    /// # fn example(index: &impl VectorIndex) {
    /// if index.is_empty() {
    ///     println!("No vectors indexed yet");
    /// }
    /// # }
    /// ```
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    // Placeholder test to verify module structure
    #[test]
    fn test_vector_index_trait_exists() {
        // This test just ensures the trait compiles
        // Actual implementations will be tested in their own modules
    }
}
