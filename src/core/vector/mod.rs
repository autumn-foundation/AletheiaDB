//! Vector utilities for AletheiaDB.
//!
//! This module provides types and functions for working with vector embeddings
//! used in semantic search and similarity operations.
//!
//! # Overview
//!
//! AletheiaDB supports storing vectors as property values on nodes via
//! [`crate::core::PropertyValue`]. This enables:
//! - **Dense Vectors**: Standard embeddings (e.g., from OpenAI, BERT) for semantic search.
//! - **Sparse Vectors**: High-dimensional sparse data (e.g., BM25, SPLADE) for keyword search.
//!
//! This module provides the low-level utilities needed to work with these vectors,
//! including similarity functions, normalization, and validation.
//!
//! # Usage
//!
//! ## Dense Vectors
//!
//! ```rust
//! use aletheiadb::core::PropertyValue;
//! use aletheiadb::core::vector::{cosine_similarity, normalize};
//!
//! // Create a vector property (automatically stored as Arc<[f32]>)
//! let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
//! let prop = PropertyValue::vector(&embedding);
//!
//! // Access the vector data
//! if let Some(vec) = prop.as_vector() {
//!     assert_eq!(vec.len(), 4);
//!
//!     // Perform vector operations
//!     let normalized = normalize(vec);
//!     let similarity = cosine_similarity(vec, &normalized).unwrap();
//!     assert!(similarity > 0.0);
//! }
//! ```
//!
//! ## Sparse Vectors
//!
//! Sparse vectors are efficient for high-dimensional data where most values are zero.
//! They store only indices and values.
//!
//! ```rust
//! use aletheiadb::core::PropertyValue;
//! use aletheiadb::core::vector::SparseVec;
//!
//! // Create a sparse vector: dim=1000, non-zero at indices 10 and 42
//! let sparse = SparseVec::new(
//!     vec![10, 42],           // Indices (must be sorted)
//!     vec![0.5, 0.8],         // Values
//!     1000                    // Total dimension
//! ).expect("Invalid sparse vector");
//!
//! let prop = PropertyValue::sparse_vector(sparse);
//!
//! // Access the sparse vector
//! if let Some(sv) = prop.as_sparse_vector() {
//!     assert_eq!(sv.nnz(), 2);
//!     assert_eq!(sv.dimension(), 1000);
//! }
//! ```
//!
//! # Design Notes
//!
//! Vectors in AletheiaDB are designed for efficiency:
//!
//! - **Storage**: Stored as `Arc<[f32]>` (dense) or `Arc<SparseVec>` (sparse) for cheap cloning and zero-copy sharing across versions.
//! - **Memory**: `f32` precision provides the best balance of accuracy and memory usage for ML embeddings.
//! - **SIMD**: Operations like cosine similarity are accelerated using AVX2/SSE2/NEON when available.
//!
//! # Implemented Functions
//!
//! - **[`cosine_similarity`]**: Measures angle between vectors, range `[-1, 1]`
//! - **[`euclidean_distance`]**: L2 distance between vectors
//! - **[`dot_product`]**: Inner product, useful for pre-normalized vectors
//! - **[`magnitude`]**: L2 norm of a vector
//! - **[`normalize`]**: Returns new unit vector
//!
//! For complete API of sparse vectors, see [`SparseVec`].

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

/// Serialization utilities for vectors.
pub mod serialization;

#[cfg(test)]
mod sentry_tests;

#[cfg(test)]
mod sentry_sparse_consistency_tests;

#[cfg(test)]
mod sentry_sparse_tests;

#[cfg(test)]
mod tests;

pub use constants::*;
pub use metric::*;
pub use ops::*;
pub use serialization::*;
pub use sparse::*;
pub use types::*;
pub use validation::*;
