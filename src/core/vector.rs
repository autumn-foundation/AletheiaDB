//! Vector utilities for GallifreyDB.
//!
//! This module provides types and functions for working with dense vectors
//! (embeddings) used in semantic search and similarity operations.
//!
//! # Overview
//!
//! GallifreyDB supports storing vectors as property values on nodes via
//! [`PropertyValue::Vector`]. This module provides the utilities needed
//! to work with those vectors effectively:
//!
//! - **Type definitions**: [`VectorDimension`] for expressing vector sizes
//! - **Similarity functions**: (future) Cosine, Euclidean, dot product
//! - **Normalization**: (future) L2 normalization for cosine similarity
//! - **Validation**: (future) Dimension checking and NaN/Inf detection
//!
//! # Usage
//!
//! ```rust
//! use gallifreydb::core::vector::VectorDimension;
//! use gallifreydb::core::PropertyValue;
//!
//! // Create a vector property
//! let embedding: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4];
//! let dim: VectorDimension = embedding.len();
//! let prop = PropertyValue::vector(embedding);
//!
//! // Access the vector
//! if let Some(vec) = prop.as_vector() {
//!     assert_eq!(vec.len(), dim);
//! }
//! ```
//!
//! # Design Notes
//!
//! Vectors in GallifreyDB are stored as `Arc<[f32]>` within [`PropertyValue::Vector`].
//! This design enables:
//!
//! - **Efficient cloning**: Multiple versions can share the same vector data
//! - **Memory efficiency**: f32 provides good precision with half the memory of f64
//! - **Temporal compatibility**: Unchanged vectors across versions share storage
//!
//! For similarity computations, vectors should typically be L2-normalized to enable
//! fast cosine similarity via dot product.
//!
//! # Future Additions
//!
//! This module will be expanded to include:
//!
//! - Similarity functions (cosine, euclidean, dot product)
//! - L2 normalization utilities
//! - Dimension validation helpers
//! - Sparse vector support
//!
//! See `docs/VECTOR_SEARCH_DESIGN.md` for the complete design.

use crate::core::property::MAX_VECTOR_DIMENSIONS;

// ============================================================================
// Type Definitions
// ============================================================================

/// The dimension (number of elements) of a vector.
///
/// This is a type alias for `usize` to provide semantic clarity when working
/// with vector dimensions throughout the codebase.
///
/// # Common Embedding Dimensions
///
/// | Model | Dimensions |
/// |-------|------------|
/// | OpenAI text-embedding-3-small | 1536 |
/// | OpenAI text-embedding-3-large | 3072 |
/// | Cohere embed-v3 | 1024 |
/// | Sentence Transformers (all-MiniLM) | 384 |
/// | BGE models | 768-1024 |
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::VectorDimension;
///
/// fn check_dimensions(vec: &[f32], expected: VectorDimension) -> bool {
///     vec.len() == expected
/// }
///
/// let embedding = vec![0.1f32; 384];
/// assert!(check_dimensions(&embedding, 384));
/// ```
pub type VectorDimension = usize;

// ============================================================================
// Constants
// ============================================================================

/// Maximum allowed vector dimension.
///
/// Re-exported from [`crate::core::property::MAX_VECTOR_DIMENSIONS`] for convenience.
/// This limit (100,000) far exceeds typical embedding sizes and exists to prevent
/// DoS attacks via memory exhaustion during deserialization.
pub const MAX_DIMENSION: VectorDimension = MAX_VECTOR_DIMENSIONS;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_dimension_is_usize() {
        let dim: VectorDimension = 1536;
        let as_usize: usize = dim;
        assert_eq!(as_usize, 1536);
    }

    #[test]
    fn test_max_dimension_constant() {
        assert_eq!(MAX_DIMENSION, 100_000);
        assert_eq!(MAX_DIMENSION, MAX_VECTOR_DIMENSIONS);
    }

    #[test]
    fn test_dimension_arithmetic() {
        let dim1: VectorDimension = 384;
        let dim2: VectorDimension = 768;

        // VectorDimension supports all usize operations
        assert_eq!(dim1 + dim2, 1152);
        assert_eq!(dim2 - dim1, 384);
        assert_eq!(dim1 * 2, 768);
    }

    #[test]
    fn test_dimension_comparison() {
        let small: VectorDimension = 384;
        let large: VectorDimension = 1536;

        assert!(small < large);
        assert!(large <= MAX_DIMENSION);
    }
}
