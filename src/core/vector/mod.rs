//! Vector utilities for AletheiaDB.
//!
//! This module provides types and functions for working with dense vectors
//! (embeddings) used in semantic search and similarity operations.
//!
//! # Overview
//!
//! AletheiaDB supports storing vectors as property values on nodes via
//! [`crate::core::PropertyValue::Vector`]. This module provides the utilities needed
//! to work with those vectors effectively:
//!
//! - **Type definitions**: [`VectorDimension`] for expressing vector sizes
//! - **Similarity functions**: [`cosine_similarity`], [`cosine_similarity_normalized`]
//! - **Distance functions**: [`euclidean_distance`], [`squared_euclidean_distance`]
//! - **Inner product**: [`dot_product`] for pre-normalized vectors or projections
//! - **Normalization**: [`magnitude`], [`squared_magnitude`], [`normalize`], [`normalize_in_place`], [`is_normalized`]
//! - **Validation**: [`validate_vector`], [`check_dimensions_match`] for NaN/Inf detection and dimension checking
//!
//! # Usage
//!
//! ```rust
//! use aletheiadb::core::vector::VectorDimension;
//! use aletheiadb::core::PropertyValue;
//!
//! // Create a vector property
//! let embedding: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4];
//! let dim = VectorDimension::new(embedding.len());
//! let prop = PropertyValue::vector(embedding);
//!
//! // Access the vector
//! if let Some(vec) = prop.as_vector() {
//!     assert_eq!(vec.len(), dim.as_usize());
//! }
//! ```
//!
//! # Design Notes
//!
//! Vectors in AletheiaDB are stored as `Arc<[f32]>` within [`crate::core::PropertyValue::Vector`].
//! This design enables:
//!
//! - **Efficient cloning**: Multiple versions can share the same vector data
//! - **Memory efficiency**: f32 provides good precision with half the memory of f64
//! - **Temporal compatibility**: Unchanged vectors across versions share storage
//!
//! For similarity computations, vectors should typically be L2-normalized to enable
//! fast cosine similarity via dot product.
//!
//! # Type Safety
//!
//! [`VectorDimension`] is implemented as a newtype struct rather than a type alias.
//! This provides stronger type safety by preventing accidental interchange with
//! other `usize` values (e.g., byte counts, array indices).
//!
//! # Implemented Functions
//!
//! - **[`cosine_similarity`]**: Measures angle between vectors, range `[-1, 1]`
//! - **[`cosine_similarity_normalized`]**: Optimized for pre-normalized (unit) vectors
//! - **[`euclidean_distance`]**: L2 distance between vectors
//! - **[`squared_euclidean_distance`]**: Squared L2 distance (faster for comparisons)
//! - **[`dot_product`]**: Inner product, useful for pre-normalized vectors
//! - **[`magnitude`]**: L2 norm of a vector
//! - **[`squared_magnitude`]**: Squared L2 norm (faster for comparisons)
//! - **[`normalize`]**: Returns new unit vector with magnitude 1.0
//! - **[`normalize_in_place`]**: Normalizes vector in place
//! - **[`is_normalized`]**: Checks if vector has unit magnitude
//!
//! All functions use SIMD acceleration (AVX2/SSE2) when available.
//!
//! # Future Additions
//!
//! This module will be expanded to include:
//!
//! - Manhattan distance
//! - Dimension validation helpers
//! - Sparse vector support
//!
//! See `docs/VECTOR_SEARCH_DESIGN.md` for the complete design.

/// Constants and thresholds.
pub mod constants;
/// Distance metrics for vector comparison.
pub mod metric;
/// High-level vector operations (similarity, normalization, etc.).
pub mod ops;
/// Sparse vector implementation.
pub mod sparse;
/// Type definitions for vector dimensions.
pub mod types;
/// Validation utilities for vectors.
pub mod validation;

pub(crate) mod simd;

#[cfg(test)]
mod sentry_tests;

#[cfg(test)]
mod simd_verification;

#[cfg(test)]
mod tests;

pub use constants::*;
pub use metric::*;
pub use ops::*;
pub use sparse::*;
pub use types::*;
pub use validation::*;
