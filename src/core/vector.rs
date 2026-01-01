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
//! - **Similarity functions**: [`cosine_similarity`] for measuring vector similarity
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
//! # Type Safety
//!
//! [`VectorDimension`] is implemented as a newtype struct rather than a type alias.
//! This provides stronger type safety by preventing accidental interchange with
//! other `usize` values (e.g., byte counts, array indices).
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
use crate::utils::error::{Error, Result};
use std::fmt;

// ============================================================================
// Type Definitions
// ============================================================================

/// The dimension (number of elements) of a vector.
///
/// This is a newtype wrapper around `usize` that provides type safety when
/// working with vector dimensions throughout the codebase. Unlike a type alias,
/// this prevents accidental interchange with other `usize` values.
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
///     vec.len() == expected.as_usize()
/// }
///
/// let embedding = vec![0.1f32; 384];
/// assert!(check_dimensions(&embedding, VectorDimension::new(384)));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct VectorDimension(usize);

impl VectorDimension {
    /// Creates a new `VectorDimension` from a `usize` value.
    ///
    /// # Example
    ///
    /// ```rust
    /// use gallifreydb::core::vector::VectorDimension;
    ///
    /// let dim = VectorDimension::new(1536);
    /// assert_eq!(dim.as_usize(), 1536);
    /// ```
    #[inline]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the dimension as a `usize`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use gallifreydb::core::vector::VectorDimension;
    ///
    /// let dim = VectorDimension::new(384);
    /// let size: usize = dim.as_usize();
    /// assert_eq!(size, 384);
    /// ```
    #[inline]
    pub const fn as_usize(self) -> usize {
        self.0
    }

    /// Returns `true` if this dimension is zero.
    #[inline]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` if this dimension exceeds the maximum allowed.
    #[inline]
    pub const fn exceeds_max(self) -> bool {
        self.0 > MAX_VECTOR_DIMENSIONS
    }
}

impl fmt::Display for VectorDimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<usize> for VectorDimension {
    #[inline]
    fn from(value: usize) -> Self {
        Self(value)
    }
}

impl From<VectorDimension> for usize {
    #[inline]
    fn from(dim: VectorDimension) -> Self {
        dim.0
    }
}

// ============================================================================
// Constants
// ============================================================================

/// Maximum allowed vector dimension.
///
/// Re-exported from [`crate::core::property::MAX_VECTOR_DIMENSIONS`] for convenience.
/// This limit (100,000) far exceeds typical embedding sizes and exists to prevent
/// DoS attacks via memory exhaustion during deserialization.
pub const MAX_DIMENSION: VectorDimension = VectorDimension(MAX_VECTOR_DIMENSIONS);

// ============================================================================
// Similarity Functions
// ============================================================================

/// Computes the cosine similarity between two vectors.
///
/// Cosine similarity measures the cosine of the angle between two vectors,
/// returning a value in the range `[-1.0, 1.0]`:
/// - `1.0`: Vectors point in the same direction (identical orientation)
/// - `0.0`: Vectors are orthogonal (perpendicular)
/// - `-1.0`: Vectors point in opposite directions
///
/// # Formula
///
/// ```text
/// cosine_similarity(a, b) = (a · b) / (||a|| × ||b||)
/// ```
///
/// where `a · b` is the dot product and `||a||` is the L2 norm (magnitude).
///
/// # Arguments
///
/// * `a` - First vector
/// * `b` - Second vector (must have the same length as `a`)
///
/// # Returns
///
/// * `Ok(f32)` - The cosine similarity in range `[-1.0, 1.0]`
/// * `Err` - If vectors have different dimensions
///
/// # Special Cases
///
/// - If either vector is a zero vector (all zeros), returns `0.0`
/// - NaN/Inf values in input will propagate to the output
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::cosine_similarity;
///
/// // Identical vectors have similarity 1.0
/// let a = vec![1.0, 0.0, 0.0];
/// let b = vec![1.0, 0.0, 0.0];
/// let sim = cosine_similarity(&a, &b).unwrap();
/// assert!((sim - 1.0).abs() < 1e-6);
///
/// // Orthogonal vectors have similarity 0.0
/// let a = vec![1.0, 0.0];
/// let b = vec![0.0, 1.0];
/// let sim = cosine_similarity(&a, &b).unwrap();
/// assert!(sim.abs() < 1e-6);
///
/// // Opposite vectors have similarity -1.0
/// let a = vec![1.0, 0.0];
/// let b = vec![-1.0, 0.0];
/// let sim = cosine_similarity(&a, &b).unwrap();
/// assert!((sim + 1.0).abs() < 1e-6);
/// ```
///
/// # Performance
///
/// This implementation uses a single-pass algorithm that computes the dot product
/// and both magnitudes simultaneously for better cache efficiency.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Result<f32> {
    if a.len() != b.len() {
        return Err(Error::Query(crate::utils::error::QueryError::InvalidParameter {
            parameter: "vectors".to_string(),
            reason: format!(
                "dimension mismatch: {} vs {}",
                a.len(),
                b.len()
            ),
        }));
    }

    // Handle empty vectors
    if a.is_empty() {
        return Ok(0.0);
    }

    // Single-pass computation of dot product and magnitudes
    let (dot, mag_a_sq, mag_b_sq) = a.iter().zip(b.iter()).fold(
        (0.0f32, 0.0f32, 0.0f32),
        |(dot, mag_a, mag_b), (&ai, &bi)| {
            (dot + ai * bi, mag_a + ai * ai, mag_b + bi * bi)
        },
    );

    let magnitude = (mag_a_sq * mag_b_sq).sqrt();

    // Handle zero vectors
    if magnitude == 0.0 {
        return Ok(0.0);
    }

    Ok(dot / magnitude)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_dimension_new() {
        let dim = VectorDimension::new(1536);
        assert_eq!(dim.as_usize(), 1536);
    }

    #[test]
    fn test_vector_dimension_from_usize() {
        let dim: VectorDimension = 384.into();
        assert_eq!(dim.as_usize(), 384);
    }

    #[test]
    fn test_vector_dimension_into_usize() {
        let dim = VectorDimension::new(768);
        let size: usize = dim.into();
        assert_eq!(size, 768);
    }

    #[test]
    fn test_max_dimension_constant() {
        assert_eq!(MAX_DIMENSION.as_usize(), 100_000);
        assert_eq!(MAX_DIMENSION.as_usize(), MAX_VECTOR_DIMENSIONS);
    }

    #[test]
    fn test_dimension_comparison() {
        let small = VectorDimension::new(384);
        let large = VectorDimension::new(1536);

        assert!(small < large);
        assert!(large <= MAX_DIMENSION);
    }

    #[test]
    fn test_dimension_equality() {
        let dim1 = VectorDimension::new(512);
        let dim2 = VectorDimension::new(512);
        let dim3 = VectorDimension::new(1024);

        assert_eq!(dim1, dim2);
        assert_ne!(dim1, dim3);
    }

    #[test]
    fn test_dimension_display() {
        let dim = VectorDimension::new(1536);
        assert_eq!(format!("{}", dim), "1536");
    }

    #[test]
    fn test_dimension_debug() {
        let dim = VectorDimension::new(384);
        assert_eq!(format!("{:?}", dim), "VectorDimension(384)");
    }

    #[test]
    fn test_is_zero() {
        assert!(VectorDimension::new(0).is_zero());
        assert!(!VectorDimension::new(1).is_zero());
    }

    #[test]
    fn test_exceeds_max() {
        assert!(!VectorDimension::new(1000).exceeds_max());
        assert!(!MAX_DIMENSION.exceeds_max());
        assert!(VectorDimension::new(100_001).exceeds_max());
    }

    #[test]
    fn test_default() {
        let dim = VectorDimension::default();
        assert_eq!(dim.as_usize(), 0);
    }

    #[test]
    fn test_copy_semantics() {
        let dim1 = VectorDimension::new(256);
        let dim2 = dim1; // Copy, not move
        assert_eq!(dim1, dim2); // Both still valid
    }

    #[test]
    fn test_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(VectorDimension::new(384));
        set.insert(VectorDimension::new(768));
        set.insert(VectorDimension::new(384)); // Duplicate

        assert_eq!(set.len(), 2);
    }

    // ========================================================================
    // Cosine Similarity Tests
    // ========================================================================

    #[test]
    fn test_cosine_similarity_identical_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!((sim - 1.0).abs() < 1e-6, "Identical vectors should have similarity 1.0");
    }

    #[test]
    fn test_cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!((sim + 1.0).abs() < 1e-6, "Opposite vectors should have similarity -1.0");
    }

    #[test]
    fn test_cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(sim.abs() < 1e-6, "Orthogonal vectors should have similarity 0.0");
    }

    #[test]
    fn test_cosine_similarity_3d_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(sim.abs() < 1e-6);

        let c = vec![0.0, 0.0, 1.0];
        let sim_ac = cosine_similarity(&a, &c).unwrap();
        assert!(sim_ac.abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_known_angle() {
        // 45 degree angle: cos(45°) ≈ 0.7071
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        let expected = 1.0 / 2.0_f32.sqrt(); // cos(45°)
        assert!((sim - expected).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_dimension_mismatch() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        let result = cosine_similarity(&a, &b);
        assert!(result.is_err());
    }

    #[test]
    fn test_cosine_similarity_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert_eq!(sim, 0.0, "Zero vector should result in similarity 0.0");
    }

    #[test]
    fn test_cosine_similarity_both_zero_vectors() {
        let a = vec![0.0, 0.0];
        let b = vec![0.0, 0.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_cosine_similarity_unit_vectors() {
        // Unit vectors should give same result as non-unit
        let a = vec![3.0, 4.0]; // magnitude 5
        let b = vec![4.0, 3.0]; // magnitude 5
        let sim1 = cosine_similarity(&a, &b).unwrap();

        // Normalize manually
        let a_norm = vec![3.0 / 5.0, 4.0 / 5.0];
        let b_norm = vec![4.0 / 5.0, 3.0 / 5.0];
        let sim2 = cosine_similarity(&a_norm, &b_norm).unwrap();

        assert!((sim1 - sim2).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_negative_values() {
        let a = vec![-1.0, -2.0, 3.0];
        let b = vec![1.0, -2.0, -3.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        // dot = -1 + 4 - 9 = -6
        // mag_a = sqrt(1 + 4 + 9) = sqrt(14)
        // mag_b = sqrt(1 + 4 + 9) = sqrt(14)
        // sim = -6 / 14 ≈ -0.4286
        let expected = -6.0 / 14.0;
        assert!((sim - expected).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_single_element() {
        let a = vec![5.0];
        let b = vec![3.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!((sim - 1.0).abs() < 1e-6, "Parallel 1D vectors should have similarity 1.0");

        let c = vec![-3.0];
        let sim_neg = cosine_similarity(&a, &c).unwrap();
        assert!((sim_neg + 1.0).abs() < 1e-6, "Anti-parallel 1D vectors should have similarity -1.0");
    }

    #[test]
    fn test_cosine_similarity_symmetry() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![4.0, 3.0, 2.0, 1.0];
        let sim_ab = cosine_similarity(&a, &b).unwrap();
        let sim_ba = cosine_similarity(&b, &a).unwrap();
        assert!((sim_ab - sim_ba).abs() < 1e-6, "Cosine similarity should be symmetric");
    }

    #[test]
    fn test_cosine_similarity_range() {
        // Various test cases to ensure result is always in [-1, 1]
        let test_cases = vec![
            (vec![1.0, 0.0], vec![1.0, 0.0]),
            (vec![1.0, 0.0], vec![-1.0, 0.0]),
            (vec![1.0, 0.0], vec![0.0, 1.0]),
            (vec![1.0, 1.0, 1.0], vec![2.0, 3.0, 4.0]),
            (vec![-1.0, -2.0], vec![3.0, 4.0]),
        ];

        for (a, b) in test_cases {
            let sim = cosine_similarity(&a, &b).unwrap();
            assert!(
                sim >= -1.0 && sim <= 1.0,
                "Similarity {} is out of range [-1, 1] for vectors {:?} and {:?}",
                sim, a, b
            );
        }
    }
}

// ============================================================================
// Property-Based Tests
// ============================================================================

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    // Strategy to generate non-empty vectors of the same length
    fn same_length_vectors(max_len: usize) -> impl Strategy<Value = (Vec<f32>, Vec<f32>)> {
        (1..=max_len).prop_flat_map(|len| {
            (
                prop::collection::vec(-100.0f32..100.0f32, len),
                prop::collection::vec(-100.0f32..100.0f32, len),
            )
        })
    }

    proptest! {
        #[test]
        fn prop_cosine_similarity_is_symmetric(
            (a, b) in same_length_vectors(100)
        ) {
            let sim_ab = cosine_similarity(&a, &b).unwrap();
            let sim_ba = cosine_similarity(&b, &a).unwrap();
            prop_assert!((sim_ab - sim_ba).abs() < 1e-5);
        }

        #[test]
        fn prop_cosine_similarity_in_range(
            (a, b) in same_length_vectors(100)
        ) {
            let sim = cosine_similarity(&a, &b).unwrap();
            // Handle NaN case (can occur with extreme values)
            if !sim.is_nan() {
                prop_assert!(sim >= -1.0 - 1e-6 && sim <= 1.0 + 1e-6,
                    "Similarity {} out of range for {:?} and {:?}", sim, a, b);
            }
        }

        #[test]
        fn prop_cosine_similarity_self_is_one(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            // Skip zero vectors
            let magnitude_sq: f32 = a.iter().map(|x| x * x).sum();
            if magnitude_sq > 1e-10 {
                let sim = cosine_similarity(&a, &a).unwrap();
                prop_assert!((sim - 1.0).abs() < 1e-5,
                    "Self-similarity should be 1.0, got {} for {:?}", sim, a);
            }
        }

        #[test]
        fn prop_cosine_similarity_negation_flips_sign(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            // Skip zero vectors
            let magnitude_sq: f32 = a.iter().map(|x| x * x).sum();
            if magnitude_sq > 1e-10 {
                let neg_a: Vec<f32> = a.iter().map(|x| -x).collect();
                let sim = cosine_similarity(&a, &neg_a).unwrap();
                prop_assert!((sim + 1.0).abs() < 1e-5,
                    "Negation similarity should be -1.0, got {} for {:?}", sim, a);
            }
        }

        #[test]
        fn prop_cosine_similarity_scale_invariant(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..50usize),
            b in prop::collection::vec(-100.0f32..100.0f32, 1..50usize),
            scale in 0.1f32..10.0f32
        ) {
            // Ensure vectors have same length
            let len = a.len().min(b.len());
            let a = &a[..len];
            let b = &b[..len];

            // Skip if either vector is near-zero
            let mag_a: f32 = a.iter().map(|x| x * x).sum();
            let mag_b: f32 = b.iter().map(|x| x * x).sum();
            if mag_a > 1e-10 && mag_b > 1e-10 {
                let sim1 = cosine_similarity(a, b).unwrap();

                let scaled_a: Vec<f32> = a.iter().map(|x| x * scale).collect();
                let sim2 = cosine_similarity(&scaled_a, b).unwrap();

                // Cosine similarity should be scale-invariant
                if !sim1.is_nan() && !sim2.is_nan() {
                    prop_assert!((sim1 - sim2).abs() < 1e-4,
                        "Scale invariance failed: {} vs {} for scale {}", sim1, sim2, scale);
                }
            }
        }
    }
}
