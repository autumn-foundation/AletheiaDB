//! Vector utilities for GallifreyDB.
//!
//! This module provides types and functions for working with dense vectors
//! (embeddings) used in semantic search and similarity operations.
//!
//! # Overview
//!
//! GallifreyDB supports storing vectors as property values on nodes via
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
//! Vectors in GallifreyDB are stored as `Arc<[f32]>` within [`crate::core::PropertyValue::Vector`].
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

use crate::core::property::MAX_VECTOR_DIMENSIONS;
use crate::utils::error::{Error, Result, VectorError};
use std::fmt;

// Internal imports
use self::simd::{dot_and_magnitudes, dot_product_sum, scale_in_place, squared_diff_sum};

// Sparse vector implementation (kept separate due to size)
pub mod sparse;
pub use sparse::*;

// SIMD implementation (internal)
pub(crate) mod simd;

#[cfg(test)]
mod tests;

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

/// Default tolerance for floating-point comparisons in normalization operations.
///
/// This tolerance (1e-6) is appropriate for most f32 operations where accumulated
/// floating-point errors are expected to be small. It's used as the default for
/// functions like [`crate::core::vector::is_normalized`] when checking if a vector has unit magnitude.
///
/// For stricter or looser comparisons, functions accept an explicit tolerance parameter.
pub const NORMALIZATION_TOLERANCE: f32 = 1e-6;

/// Squared magnitude threshold for detecting near-zero vectors.
///
/// Vectors with squared magnitude below this threshold are treated as zero vectors
/// in normalization operations. This prevents numerical instability from denormal
/// numbers and avoids division by very small values that could cause overflow.
///
/// Value: 1e-14 corresponds to magnitude ≈ 1e-7, providing safety margin for f32
/// precision (which has ~7 significant digits). This is more conservative than
/// 1e-20 (magnitude ≈ 1e-10) which is too close to f32's precision limits.
pub(crate) const SQUARED_MAGNITUDE_THRESHOLD: f32 = 1e-14;

/// Maximum allowed vector dimension.
///
/// Re-exported from [`crate::core::property::MAX_VECTOR_DIMENSIONS`] for convenience.
/// This limit (100,000) far exceeds typical embedding sizes and exists to prevent
/// DoS attacks via memory exhaustion during deserialization.
pub const MAX_DIMENSION: VectorDimension = VectorDimension::new(MAX_VECTOR_DIMENSIONS);

// ============================================================================
// Vector Validation
// ============================================================================

/// Validates that a vector contains no NaN or Infinity values.
///
/// This function scans all elements of the vector and returns an error if any
/// invalid floating-point values are found. NaN and Infinity values would cause
/// incorrect results in distance/similarity calculations and should be caught early.
///
/// # Arguments
///
/// * `v` - The vector slice to validate
///
/// # Returns
///
/// * `Ok(())` if all elements are valid finite numbers
/// * `Err(VectorError::ContainsNaN)` if any NaN values are found
/// * `Err(VectorError::ContainsInfinity)` if any Infinity values are found (checked after NaN)
///
/// # Note
///
/// NaN is checked first, so if a vector contains both NaN and Infinity values,
/// the NaN error will be returned. This is because NaN values are generally more
/// problematic (NaN != NaN, propagates through calculations).
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::validate_vector;
///
/// // Valid vector
/// let v = vec![1.0, 2.0, 3.0];
/// assert!(validate_vector(&v).is_ok());
///
/// // Vector with NaN
/// let v_nan = vec![1.0, f32::NAN, 3.0];
/// assert!(validate_vector(&v_nan).is_err());
///
/// // Vector with Infinity
/// let v_inf = vec![1.0, f32::INFINITY, 3.0];
/// assert!(validate_vector(&v_inf).is_err());
///
/// // Empty vector is valid
/// let empty: Vec<f32> = vec![];
/// assert!(validate_vector(&empty).is_ok());
/// ```
#[inline]
pub fn validate_vector(v: &[f32]) -> Result<()> {
    // Use a single pass to count both NaN and Infinity values for efficiency.
    // This is more efficient than iterating twice, especially for large vectors.
    let (nan_count, inf_count) = v.iter().fold((0usize, 0usize), |(nan, inf), &val| {
        if val.is_nan() {
            (nan + 1, inf)
        } else if val.is_infinite() {
            (nan, inf + 1)
        } else {
            (nan, inf)
        }
    });

    // Per the function's contract, NaN is checked first.
    if nan_count > 0 {
        return Err(Error::Vector(VectorError::ContainsNaN { count: nan_count }));
    }

    if inf_count > 0 {
        return Err(Error::Vector(VectorError::ContainsInfinity {
            count: inf_count,
        }));
    }

    Ok(())
}

/// Checks that two vectors have matching dimensions.
///
/// Many vector operations require vectors of equal length. This function provides
/// a convenient way to validate dimension compatibility before performing operations.
///
/// # Arguments
///
/// * `a` - The first vector (its length is considered the "expected" dimension)
/// * `b` - The second vector (its length is compared against `a`)
///
/// # Returns
///
/// * `Ok(())` if both vectors have the same length
/// * `Err(VectorError::DimensionMismatch)` if lengths differ
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::check_dimensions_match;
///
/// let v1 = vec![1.0, 2.0, 3.0];
/// let v2 = vec![4.0, 5.0, 6.0];
/// let v3 = vec![1.0, 2.0];
///
/// // Same dimensions - OK
/// assert!(check_dimensions_match(&v1, &v2).is_ok());
///
/// // Different dimensions - Error
/// assert!(check_dimensions_match(&v1, &v3).is_err());
///
/// // Empty vectors match
/// let empty1: Vec<f32> = vec![];
/// let empty2: Vec<f32> = vec![];
/// assert!(check_dimensions_match(&empty1, &empty2).is_ok());
/// ```
#[inline]
pub fn check_dimensions_match(a: &[f32], b: &[f32]) -> Result<()> {
    if a.len() != b.len() {
        return Err(Error::Vector(VectorError::DimensionMismatch {
            expected: a.len(),
            actual: b.len(),
        }));
    }
    Ok(())
}

/// Validates a vector and checks that its dimension is within bounds.
///
/// This is a convenience function that combines validation (NaN/Infinity checking)
/// with dimension bounds checking. Useful when processing user-provided vectors
/// that need both validation and size constraints.
///
/// # Arguments
///
/// * `v` - The vector slice to validate
/// * `max_dimension` - The maximum allowed dimension (length)
///
/// # Returns
///
/// * `Ok(())` if the vector is valid and within dimension bounds
/// * `Err(VectorError::ContainsNaN)` if any NaN values are found
/// * `Err(VectorError::ContainsInfinity)` if any Infinity values are found
/// * `Err(VectorError::DimensionTooLarge)` if the vector exceeds max_dimension
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::validate_vector_with_bounds;
///
/// // Valid vector within bounds
/// let v = vec![1.0, 2.0, 3.0];
/// assert!(validate_vector_with_bounds(&v, 10).is_ok());
///
/// // Vector too large
/// assert!(validate_vector_with_bounds(&v, 2).is_err());
///
/// // Vector with invalid values
/// let v_nan = vec![1.0, f32::NAN];
/// assert!(validate_vector_with_bounds(&v_nan, 10).is_err());
/// ```
#[inline]
pub fn validate_vector_with_bounds(v: &[f32], max_dimension: usize) -> Result<()> {
    // Check dimension first (fast check)
    if v.len() > max_dimension {
        return Err(Error::Vector(VectorError::DimensionTooLarge {
            dimension: v.len(),
            max_allowed: max_dimension,
        }));
    }

    // Then validate contents
    validate_vector(v)
}

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
/// - NaN values in input will propagate to the output (result will be NaN)
/// - Inf values in input will propagate (may result in NaN depending on combination)
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
/// This implementation uses SIMD acceleration when available:
/// - **AVX2 + FMA**: Processes 8 floats at a time with fused multiply-add
/// - **SSE2**: Processes 4 floats at a time (baseline for x86_64)
/// - **Scalar**: Fallback for other platforms
///
/// All variants use a single-pass algorithm that computes the dot product
/// and both magnitudes simultaneously for better cache efficiency.
///
/// # Numerical Precision
///
/// This implementation uses standard f32 accumulation, which provides sufficient
/// precision for typical embedding use cases (dimensions up to ~10,000 with values
/// in the range [-100, 100]). For these cases, relative error is typically < 1e-5.
///
/// **Precision characteristics:**
/// - Typical embeddings (normalized, dim ≤ 4096): Excellent precision (< 1e-6 error)
/// - Large vectors (dim > 10,000): May accumulate ~1e-4 relative error
/// - Extreme magnitudes (values > 1e6): Consider normalizing inputs first
///
/// For applications requiring higher precision with extreme values, consider:
/// 1. Normalizing vectors to unit length before comparison
/// 2. Using f64 vectors with a custom implementation
/// 3. Implementing Kahan summation (not included due to performance overhead)
///
/// The result is always clamped to `[-1.0, 1.0]` to handle minor floating-point
/// inaccuracies that could produce values slightly outside this range.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Result<f32> {
    check_dimensions_match(a, b)?;

    // Handle empty vectors
    if a.is_empty() {
        return Ok(0.0);
    }

    // Use SIMD-accelerated computation when available
    let (dot, mag_a_sq, mag_b_sq) = dot_and_magnitudes(a, b);

    let magnitude = (mag_a_sq * mag_b_sq).sqrt();

    // Handle zero vectors
    if magnitude == 0.0 {
        return Ok(0.0);
    }

    // Compute the raw result before clamping
    let result = dot / magnitude;

    // Debug assertion to detect if clamping is hiding a significant numerical issue.
    // For correctly computed cosine similarity, values should only exceed [-1, 1]
    // by at most machine epsilon (~1e-7 for f32). Values exceeding by more than
    // 1e-5 may indicate a bug in the SIMD implementation or extreme input values.
    debug_assert!(
        result.is_nan() || result.abs() <= 1.0 + 1e-5,
        "Cosine similarity {} out of valid range before clamping. \
         This may indicate numerical issues with the input vectors.",
        result
    );

    // Clamp to handle minor floating-point inaccuracies that could produce
    // values slightly outside [-1.0, 1.0]
    Ok(result.clamp(-1.0, 1.0))
}

/// Computes cosine similarity between pre-normalized (unit) vectors.
///
/// This is an optimized version of [`cosine_similarity`] for vectors that have
/// already been L2-normalized (i.e., `||a|| = ||b|| = 1.0`). Since the magnitudes
/// are known to be 1.0, this function skips the magnitude computation entirely
/// and simply computes the dot product.
///
/// # Performance
///
/// This function provides approximately **2x speedup** compared to the general
/// [`cosine_similarity`] function because it:
/// - Skips computing `||a||²` and `||b||²`
/// - Skips the `sqrt()` call for the magnitude product
/// - Skips the division (or rather, divides by 1.0 implicitly)
///
/// # Arguments
///
/// * `a` - First unit vector (must be L2-normalized: `||a|| = 1.0`)
/// * `b` - Second unit vector (must be L2-normalized: `||b|| = 1.0`)
///
/// # Returns
///
/// * `Ok(f32)` - The cosine similarity (equivalent to dot product for unit vectors)
/// * `Err` - If vectors have different dimensions
///
/// # Panics (Debug Mode)
///
/// In debug builds, this function asserts that both vectors are approximately
/// unit length. If a vector's magnitude differs from 1.0 by more than 1e-4,
/// the assertion will fail.
///
/// # Safety Contract
///
/// The caller **must ensure** that both vectors are L2-normalized. If this
/// precondition is violated:
/// - Results will be mathematically incorrect
/// - The debug assertion will catch this in debug builds
/// - Release builds will silently produce wrong results
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::cosine_similarity_normalized;
///
/// // Pre-normalize vectors
/// let a = vec![1.0, 0.0, 0.0];  // Already unit length
/// let b_raw = vec![1.0, 1.0, 0.0];
/// let b_mag = (b_raw.iter().map(|x| x * x).sum::<f32>()).sqrt();
/// let b: Vec<f32> = b_raw.iter().map(|x| x / b_mag).collect();
///
/// let sim = cosine_similarity_normalized(&a, &b).unwrap();
/// // sim ≈ cos(45°) ≈ 0.707
/// assert!((sim - 0.707).abs() < 0.01);
/// ```
///
/// # When to Use
///
/// Use this function when:
/// - You pre-normalize vectors at ingestion time (common practice)
/// - You're doing many similarity comparisons against the same query vector
/// - Performance is critical and you can guarantee unit vectors
///
/// Use [`cosine_similarity`] instead when:
/// - Vectors may not be normalized
/// - You're unsure about vector magnitudes
/// - Correctness is more important than performance
#[inline]
pub fn cosine_similarity_normalized(a: &[f32], b: &[f32]) -> Result<f32> {
    check_dimensions_match(a, b)?;

    // Handle empty vectors
    if a.is_empty() {
        return Ok(0.0);
    }

    // Debug assertions to verify the precondition that vectors are normalized
    #[cfg(debug_assertions)]
    {
        let mag_a_sq: f32 = a.iter().map(|x| x * x).sum();
        let mag_b_sq: f32 = b.iter().map(|x| x * x).sum();

        debug_assert!(
            (mag_a_sq - 1.0).abs() < 1e-4,
            "First vector is not unit length: ||a||² = {} (expected 1.0). \
             Use cosine_similarity() for non-normalized vectors.",
            mag_a_sq
        );
        debug_assert!(
            (mag_b_sq - 1.0).abs() < 1e-4,
            "Second vector is not unit length: ||b||² = {} (expected 1.0). \
             Use cosine_similarity() for non-normalized vectors.",
            mag_b_sq
        );
    }

    // For unit vectors, cosine similarity = dot product
    // We reuse the SIMD infrastructure but only need the dot product
    let dot = dot_product_sum(a, b);

    // Clamp to handle floating-point inaccuracies
    Ok(dot.clamp(-1.0, 1.0))
}

// ============================================================================
// Distance Functions
// ============================================================================

/// Computes the squared Euclidean distance between two vectors.
///
/// The squared Euclidean distance is the sum of squared differences between
/// corresponding elements. This is often preferred over [`euclidean_distance`]
/// for comparisons because it avoids the expensive square root operation while
/// preserving ordering (if `d²(a,b) < d²(a,c)` then `d(a,b) < d(a,c)`).
///
/// # Formula
///
/// ```text
/// squared_euclidean_distance(a, b) = Σ(aᵢ - bᵢ)²
/// ```
///
/// # Arguments
///
/// * `a` - First vector
/// * `b` - Second vector (must have the same length as `a`)
///
/// # Returns
///
/// * `Ok(f32)` - The squared Euclidean distance (always non-negative)
/// * `Err` - If vectors have different dimensions
///
/// # When to Use
///
/// Use squared distance instead of regular distance when:
/// - Comparing distances (finding nearest neighbors)
/// - Performance is critical
/// - The actual distance value is not needed
///
/// Use [`euclidean_distance`] when:
/// - You need the actual distance value
/// - Combining with other non-squared metrics
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::squared_euclidean_distance;
///
/// // Distance from origin
/// let a = vec![3.0, 4.0];
/// let b = vec![0.0, 0.0];
/// let dist_sq = squared_euclidean_distance(&a, &b).unwrap();
/// assert!((dist_sq - 25.0).abs() < 1e-6); // 3² + 4² = 25
///
/// // Use for comparison without sqrt overhead
/// let c = vec![1.0, 1.0];
/// let dist_bc_sq = squared_euclidean_distance(&b, &c).unwrap();
/// // dist_bc < dist_ab because dist_bc_sq < dist_ab_sq
/// assert!(dist_bc_sq < dist_sq);
/// ```
///
/// # Performance
///
/// This implementation uses SIMD acceleration when available:
/// - **AVX2 + FMA**: Processes 8 floats at a time with fused multiply-add
/// - **SSE2**: Processes 4 floats at a time (baseline for x86_64)
/// - **Scalar**: Fallback for other platforms
#[inline]
pub fn squared_euclidean_distance(a: &[f32], b: &[f32]) -> Result<f32> {
    check_dimensions_match(a, b)?;

    // Handle empty vectors
    if a.is_empty() {
        return Ok(0.0);
    }

    // Use SIMD-accelerated computation when available
    Ok(squared_diff_sum(a, b))
}

/// Computes the Euclidean distance between two vectors.
///
/// The Euclidean distance (also known as L2 distance) measures the "straight-line"
/// distance between two points in Euclidean space. It is the most common distance
/// metric used in machine learning and data science.
///
/// # Formula
///
/// ```text
/// euclidean_distance(a, b) = √(Σ(aᵢ - bᵢ)²)
/// ```
///
/// # Arguments
///
/// * `a` - First vector
/// * `b` - Second vector (must have the same length as `a`)
///
/// # Returns
///
/// * `Ok(f32)` - The Euclidean distance (always non-negative)
/// * `Err` - If vectors have different dimensions
///
/// # Performance Note
///
/// If you only need to compare distances (e.g., finding the k nearest neighbors),
/// consider using [`squared_euclidean_distance`] instead, as it avoids the square
/// root operation while preserving distance ordering.
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::euclidean_distance;
///
/// // Classic 3-4-5 right triangle
/// let a = vec![0.0, 0.0];
/// let b = vec![3.0, 4.0];
/// let dist = euclidean_distance(&a, &b).unwrap();
/// assert!((dist - 5.0).abs() < 1e-6);
///
/// // Same point has distance 0
/// let c = vec![1.0, 2.0, 3.0];
/// let dist_same = euclidean_distance(&c, &c).unwrap();
/// assert!(dist_same.abs() < 1e-6);
/// ```
///
/// # Performance
///
/// This implementation uses SIMD acceleration when available:
/// - **AVX2 + FMA**: Processes 8 floats at a time with fused multiply-add
/// - **SSE2**: Processes 4 floats at a time (baseline for x86_64)
/// - **Scalar**: Fallback for other platforms
///
/// The square root is computed after the SIMD-accelerated sum.
#[inline]
pub fn euclidean_distance(a: &[f32], b: &[f32]) -> Result<f32> {
    squared_euclidean_distance(a, b).map(|sq| sq.sqrt())
}

// ============================================================================
// Dot Product
// ============================================================================

/// Computes the dot product (inner product) of two vectors.
///
/// The dot product is the sum of element-wise products: `Σ(aᵢ × bᵢ)`.
/// It is a fundamental operation used in:
/// - Cosine similarity (when vectors are normalized)
/// - Linear algebra operations
/// - Neural network computations
/// - Projection calculations
///
/// # Formula
///
/// ```text
/// dot_product(a, b) = Σ(aᵢ × bᵢ) = a₀×b₀ + a₁×b₁ + ... + aₙ×bₙ
/// ```
///
/// # Arguments
///
/// * `a` - First vector
/// * `b` - Second vector (must have the same length as `a`)
///
/// # Returns
///
/// * `Ok(f32)` - The dot product (can be positive, negative, or zero)
/// * `Err` - If vectors have different dimensions
///
/// # Properties
///
/// - **Commutativity**: `dot(a, b) = dot(b, a)`
/// - **Self dot product**: `dot(a, a) = ||a||²` (squared magnitude)
/// - **Orthogonal vectors**: `dot(a, b) = 0` when vectors are perpendicular
/// - **Parallel vectors**: `dot(a, b) = ||a|| × ||b||` (same direction)
///   or `-||a|| × ||b||` (opposite direction)
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::dot_product;
///
/// // Basic dot product
/// let a = vec![1.0, 2.0, 3.0];
/// let b = vec![4.0, 5.0, 6.0];
/// let result = dot_product(&a, &b).unwrap();
/// // 1×4 + 2×5 + 3×6 = 4 + 10 + 18 = 32
/// assert!((result - 32.0).abs() < 1e-6);
///
/// // Self dot product equals squared magnitude
/// let v = vec![3.0, 4.0];
/// let self_dot = dot_product(&v, &v).unwrap();
/// assert!((self_dot - 25.0).abs() < 1e-6); // 3² + 4² = 25
///
/// // Orthogonal vectors
/// let x = vec![1.0, 0.0];
/// let y = vec![0.0, 1.0];
/// let ortho = dot_product(&x, &y).unwrap();
/// assert!(ortho.abs() < 1e-6);
/// ```
///
/// # Performance
///
/// This implementation uses SIMD acceleration when available:
/// - **AVX2 + FMA**: Processes 8 floats at a time with fused multiply-add
/// - **SSE2**: Processes 4 floats at a time (baseline for x86_64)
/// - **Scalar**: Fallback for other platforms
///
/// This dedicated dot product function is more efficient than
/// [`cosine_similarity`] when you only need the dot product and not the
/// magnitudes (e.g., when working with pre-normalized vectors).
#[inline]
pub fn dot_product(a: &[f32], b: &[f32]) -> Result<f32> {
    check_dimensions_match(a, b)?;

    // Handle empty vectors
    if a.is_empty() {
        return Ok(0.0);
    }

    // Use SIMD-accelerated computation when available
    Ok(dot_product_sum(a, b))
}

// ============================================================================
// Normalization
// ============================================================================

/// Computes the magnitude (L2 norm) of a vector.
///
/// The magnitude is the square root of the sum of squared elements:
/// `||v|| = sqrt(v₀² + v₁² + ... + vₙ²)`
///
/// # Arguments
///
/// * `v` - The vector to compute the magnitude of
///
/// # Returns
///
/// The magnitude as a non-negative f32. Returns 0.0 for empty vectors.
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::magnitude;
///
/// // Classic 3-4-5 right triangle
/// let v = vec![3.0, 4.0];
/// let mag = magnitude(&v);
/// assert!((mag - 5.0).abs() < 1e-6);
///
/// // Unit vector has magnitude 1
/// let unit = vec![1.0, 0.0, 0.0];
/// assert!((magnitude(&unit) - 1.0).abs() < 1e-6);
/// ```
///
/// # Performance
///
/// This function uses SIMD-accelerated dot product internally:
/// `magnitude(v) = sqrt(dot_product(v, v))`
#[inline(always)]
pub fn magnitude(v: &[f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    dot_product_sum(v, v).sqrt()
}

/// Computes the squared magnitude of a vector.
///
/// This is equivalent to `magnitude(v).powi(2)` but avoids the square root,
/// making it faster for comparisons where the actual magnitude isn't needed.
///
/// # Arguments
///
/// * `v` - The vector to compute the squared magnitude of
///
/// # Returns
///
/// The squared magnitude as a non-negative f32. Returns 0.0 for empty vectors.
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::squared_magnitude;
///
/// let v = vec![3.0, 4.0];
/// let sq_mag = squared_magnitude(&v);
/// assert!((sq_mag - 25.0).abs() < 1e-6); // 3² + 4² = 25
/// ```
#[inline(always)]
pub fn squared_magnitude(v: &[f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    dot_product_sum(v, v)
}

/// Normalizes a vector to unit length (L2 normalization).
///
/// Creates a new vector with the same direction but magnitude 1.0.
/// For zero vectors (magnitude = 0), returns a zero vector of the same length.
///
/// # Arguments
///
/// * `v` - The vector to normalize
///
/// # Returns
///
/// A new `Vec<f32>` with unit length, or a zero vector if the input is zero.
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::{normalize, magnitude};
///
/// let v = vec![3.0, 4.0];
/// let unit = normalize(&v);
///
/// // Normalized vector has magnitude 1
/// assert!((magnitude(&unit) - 1.0).abs() < 1e-6);
///
/// // Direction is preserved: [3, 4] -> [0.6, 0.8]
/// assert!((unit[0] - 0.6).abs() < 1e-6);
/// assert!((unit[1] - 0.8).abs() < 1e-6);
///
/// // Zero vector stays zero
/// let zero = vec![0.0, 0.0];
/// let normalized_zero = normalize(&zero);
/// assert_eq!(normalized_zero, vec![0.0, 0.0]);
/// ```
///
/// # Performance
///
/// This function allocates a new vector. For in-place normalization without
/// allocation, use [`normalize_in_place`].
/// Uses SIMD-accelerated scalar multiplication (AVX2/SSE2) for optimal performance.
///
/// # Note on Dimension Validation
///
/// Unlike two-vector functions like [`cosine_similarity`], normalization functions
/// do not validate against `MAX_VECTOR_DIMENSIONS`. This is intentional because:
/// - Single-vector operations don't have dimension mismatch issues
/// - Dimension limits are enforced at storage time (see [`crate::core::PropertyValue::vector`])
/// - Additional checks would add overhead without safety benefit
#[inline]
pub fn normalize(v: &[f32]) -> Vec<f32> {
    let sq_mag = squared_magnitude(v);
    // Use squared magnitude threshold to avoid denormal number issues.
    // See SQUARED_MAGNITUDE_THRESHOLD for details.
    if sq_mag < SQUARED_MAGNITUDE_THRESHOLD {
        // Return zero vector of same length
        return vec![0.0; v.len()];
    }
    // Copy then scale in place using SIMD
    // Compute 1/sqrt(sq_mag) directly to avoid intermediate variable
    let mut result: Vec<f32> = v.to_vec();
    let inv_mag = 1.0 / sq_mag.sqrt();
    scale_in_place(&mut result, inv_mag);
    result
}

/// Normalizes a vector to unit length in place.
///
/// Modifies the vector in place to have magnitude 1.0.
/// For zero vectors (magnitude = 0), leaves the vector unchanged.
///
/// # Arguments
///
/// * `v` - The vector to normalize (modified in place)
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::{normalize_in_place, magnitude};
///
/// let mut v = vec![3.0, 4.0];
/// normalize_in_place(&mut v);
///
/// // Now has magnitude 1
/// assert!((magnitude(&v) - 1.0).abs() < 1e-6);
///
/// // Direction is preserved: [3, 4] -> [0.6, 0.8]
/// assert!((v[0] - 0.6).abs() < 1e-6);
/// assert!((v[1] - 0.8).abs() < 1e-6);
/// ```
///
/// # Performance
///
/// This function modifies the vector in place without allocation, making it
/// more efficient than [`normalize`] when a new vector isn't needed.
/// Uses SIMD-accelerated scalar multiplication (AVX2/SSE2) for optimal performance.
#[inline]
pub fn normalize_in_place(v: &mut [f32]) {
    let sq_mag = squared_magnitude(v);
    // Use squared magnitude threshold to avoid denormal number issues.
    // See SQUARED_MAGNITUDE_THRESHOLD for details.
    if sq_mag < SQUARED_MAGNITUDE_THRESHOLD {
        // Leave zero/near-zero vector unchanged
        return;
    }
    // Compute 1/sqrt(sq_mag) directly to avoid intermediate variable
    let inv_mag = 1.0 / sq_mag.sqrt();
    scale_in_place(v, inv_mag);
}

/// Checks if a vector is normalized (has magnitude approximately 1.0).
///
/// This is useful for validating that vectors are properly normalized before
/// using optimized functions like [`cosine_similarity_normalized`].
///
/// # Arguments
///
/// * `v` - The vector to check
/// * `tolerance` - Maximum allowed deviation from 1.0 (e.g., 1e-6)
///
/// # Returns
///
/// `true` if the magnitude is within `tolerance` of 1.0, `false` otherwise.
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::{is_normalized, normalize};
///
/// let v = vec![3.0, 4.0];
/// assert!(!is_normalized(&v, 1e-6));
///
/// let unit = normalize(&v);
/// assert!(is_normalized(&unit, 1e-6));
/// ```
#[inline]
pub fn is_normalized(v: &[f32], tolerance: f32) -> bool {
    debug_assert!(
        (0.0..1.0).contains(&tolerance),
        "tolerance must be in range [0.0, 1.0), got {}",
        tolerance
    );
    // Use squared_magnitude to avoid sqrt for better numerical stability
    // |magnitude - 1.0| <= tolerance  ⟺  (1-tolerance)² <= ||v||² <= (1+tolerance)²
    let sq_mag = squared_magnitude(v);
    let lower = (1.0 - tolerance).max(0.0).powi(2);
    let upper = (1.0 + tolerance).powi(2);
    sq_mag >= lower && sq_mag <= upper
}

/// Checks if a vector is normalized using the default tolerance.
///
/// This is a convenience wrapper around [`is_normalized`] that uses
/// [`crate::core::vector::NORMALIZATION_TOLERANCE`] (1e-6) as the tolerance value.
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::{is_normalized_default, normalize};
///
/// let v = vec![3.0, 4.0];
/// assert!(!is_normalized_default(&v));
///
/// let unit = normalize(&v);
/// assert!(is_normalized_default(&unit));
/// ```
#[inline]
pub fn is_normalized_default(v: &[f32]) -> bool {
    is_normalized(v, NORMALIZATION_TOLERANCE)
}

// ============================================================================
// Distance Metric Enum
// ============================================================================

/// Specifies which distance or similarity metric to use for vector operations.
///
/// This enum provides a unified interface for computing distances and similarities
/// between vectors, dispatching to the appropriate underlying function based on the
/// selected metric.
///
/// # Choosing a Metric
///
/// | Metric | Best For | Range | Notes |
/// |--------|----------|-------|-------|
/// | [`Cosine`](DistanceMetric::Cosine) | Semantic similarity, text embeddings | [-1, 1] similarity, [0, 2] distance | Scale-invariant, most common for embeddings |
/// | [`Euclidean`](DistanceMetric::Euclidean) | Spatial data, image features | [0, ∞) distance | Sensitive to vector magnitude |
/// | [`DotProduct`](DistanceMetric::DotProduct) | Pre-normalized vectors, MaxIP search | (-∞, ∞) | Fastest; requires normalized vectors for cosine-like behavior |
///
/// # Example
///
/// ```rust
/// use gallifreydb::core::vector::DistanceMetric;
///
/// let a = vec![1.0, 0.0, 0.0];
/// let b = vec![0.0, 1.0, 0.0];
///
/// // Using cosine similarity (orthogonal vectors = 0 similarity)
/// let similarity = DistanceMetric::Cosine.compute_similarity(&a, &b).unwrap();
/// assert!((similarity - 0.0).abs() < 1e-6);
///
/// // Using euclidean distance
/// let distance = DistanceMetric::Euclidean.compute_distance(&a, &b).unwrap();
/// assert!((distance - std::f32::consts::SQRT_2).abs() < 1e-6);
/// ```
///
/// # Performance
///
/// All metrics use SIMD acceleration (AVX2/SSE2) when available. For maximum
/// performance with large-scale similarity search:
///
/// 1. Pre-normalize vectors with [`crate::core::vector::normalize`] or [`crate::core::vector::normalize_in_place`]
/// 2. Use [`DotProduct`](DistanceMetric::DotProduct) metric (single SIMD operation)
/// 3. Store normalized vectors to avoid repeated normalization
///
/// # Future Enhancements
///
/// - **Serialization**: When serde is added as a dependency, this enum will
///   support `#[serde(rename_all = "snake_case")]` for JSON/config serialization
/// - **Batch operations**: `compute_distances_batch()` for SIMD-optimized
///   multi-vector distance computation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DistanceMetric {
    /// Cosine similarity/distance.
    ///
    /// Measures the cosine of the angle between two vectors, making it
    /// **scale-invariant** (only direction matters, not magnitude).
    ///
    /// - **Similarity**: Range [-1, 1] where 1 = identical direction,
    ///   0 = orthogonal, -1 = opposite direction
    /// - **Distance**: Computed as `1 - similarity`, range [0, 2]
    ///
    /// # When to Use
    ///
    /// - Text embeddings (word2vec, BERT, OpenAI embeddings)
    /// - Semantic similarity where magnitude doesn't matter
    /// - When vectors may have different scales
    ///
    /// # Implementation Note
    ///
    /// Uses [`cosine_similarity`] internally, which handles zero vectors
    /// by returning 0.0 similarity.
    #[default]
    Cosine,

    /// Euclidean (L2) distance.
    ///
    /// Measures the straight-line distance between two points in vector space.
    /// Also known as the L2 norm of the difference vector.
    ///
    /// - **Distance**: Range [0, ∞) where 0 = identical vectors
    /// - **Similarity**: Computed as `1 / (1 + distance)`, range (0, 1]
    ///
    /// # When to Use
    ///
    /// - Spatial data (coordinates, positions)
    /// - Image feature vectors
    /// - When absolute magnitude differences matter
    /// - K-means clustering (uses squared Euclidean internally)
    ///
    /// # Implementation Note
    ///
    /// Uses [`euclidean_distance`] internally, which uses SIMD-accelerated
    /// squared difference computation.
    Euclidean,

    /// Inner (dot) product.
    ///
    /// Computes the sum of element-wise products. For normalized vectors,
    /// this equals cosine similarity but is faster (single SIMD operation).
    ///
    /// - **Raw value**: Range (-∞, ∞)
    /// - **Similarity**: Raw dot product value (higher = more similar)
    /// - **Distance**: Computed as `1 - dot_product`, which is meaningful
    ///   only for normalized vectors
    ///
    /// # When to Use
    ///
    /// - **Maximum Inner Product Search (MIPS)**: When you want the highest
    ///   dot product, not necessarily the closest vector
    /// - **Pre-normalized vectors**: Equivalent to cosine but faster
    /// - **Learned embeddings**: Some models are trained with dot product loss
    ///
    /// # Important
    ///
    /// For non-normalized vectors, dot product is **not** a proper distance
    /// metric (doesn't satisfy triangle inequality). Use [`Cosine`](DistanceMetric::Cosine)
    /// for general similarity or ensure vectors are normalized first.
    DotProduct,
}

impl DistanceMetric {
    /// Computes the distance between two vectors using this metric.
    ///
    /// Lower values indicate more similar vectors.
    ///
    /// # Returns
    ///
    /// - [`Cosine`](DistanceMetric::Cosine): `1 - cosine_similarity`, range [0, 2]
    /// - [`Euclidean`](DistanceMetric::Euclidean): L2 distance, range [0, ∞)
    /// - [`DotProduct`](DistanceMetric::DotProduct): `1 - dot_product` (meaningful only for normalized vectors)
    ///
    /// # Errors
    ///
    /// Returns an error if the vectors have different lengths.
    ///
    /// # Example
    ///
    /// ```rust
    /// use gallifreydb::core::vector::DistanceMetric;
    ///
    /// let a = vec![1.0, 0.0];
    /// let b = vec![1.0, 0.0];
    ///
    /// // Identical vectors have zero distance
    /// assert!((DistanceMetric::Cosine.compute_distance(&a, &b).unwrap() - 0.0).abs() < 1e-6);
    /// assert!((DistanceMetric::Euclidean.compute_distance(&a, &b).unwrap() - 0.0).abs() < 1e-6);
    /// ```
    #[inline]
    pub fn compute_distance(&self, a: &[f32], b: &[f32]) -> Result<f32> {
        match self {
            DistanceMetric::Cosine => cosine_similarity(a, b).map(|sim| 1.0 - sim),
            DistanceMetric::Euclidean => euclidean_distance(a, b),
            DistanceMetric::DotProduct => dot_product(a, b).map(|dp| 1.0 - dp),
        }
    }

    /// Computes the similarity between two vectors using this metric.
    ///
    /// Higher values indicate more similar vectors.
    ///
    /// # Returns
    ///
    /// - [`Cosine`](DistanceMetric::Cosine): Cosine similarity, range [-1, 1]
    /// - [`Euclidean`](DistanceMetric::Euclidean): `1 / (1 + distance)`, range (0, 1]
    /// - [`DotProduct`](DistanceMetric::DotProduct): Raw dot product, range (-∞, ∞)
    ///
    /// # Errors
    ///
    /// Returns an error if the vectors have different lengths.
    ///
    /// # Example
    ///
    /// ```rust
    /// use gallifreydb::core::vector::DistanceMetric;
    ///
    /// let a = vec![1.0, 0.0];
    /// let b = vec![1.0, 0.0];
    ///
    /// // Identical vectors have maximum similarity
    /// assert!((DistanceMetric::Cosine.compute_similarity(&a, &b).unwrap() - 1.0).abs() < 1e-6);
    /// assert!((DistanceMetric::Euclidean.compute_similarity(&a, &b).unwrap() - 1.0).abs() < 1e-6);
    /// ```
    #[inline]
    pub fn compute_similarity(&self, a: &[f32], b: &[f32]) -> Result<f32> {
        match self {
            DistanceMetric::Cosine => cosine_similarity(a, b),
            DistanceMetric::Euclidean => euclidean_distance(a, b).map(|dist| 1.0 / (1.0 + dist)),
            DistanceMetric::DotProduct => dot_product(a, b),
        }
    }

    /// Returns a human-readable name for this metric.
    ///
    /// # Example
    ///
    /// ```rust
    /// use gallifreydb::core::vector::DistanceMetric;
    ///
    /// assert_eq!(DistanceMetric::Cosine.name(), "cosine");
    /// assert_eq!(DistanceMetric::Euclidean.name(), "euclidean");
    /// assert_eq!(DistanceMetric::DotProduct.name(), "dot_product");
    /// ```
    #[inline]
    pub const fn name(&self) -> &'static str {
        match self {
            DistanceMetric::Cosine => "cosine",
            DistanceMetric::Euclidean => "euclidean",
            DistanceMetric::DotProduct => "dot_product",
        }
    }

    /// Returns whether this metric requires normalized vectors for optimal results.
    ///
    /// - [`Cosine`](DistanceMetric::Cosine): No (handles normalization internally)
    /// - [`Euclidean`](DistanceMetric::Euclidean): No (works with any vectors)
    /// - [`DotProduct`](DistanceMetric::DotProduct): Yes (otherwise not a proper similarity)
    ///
    /// # Example
    ///
    /// ```rust
    /// use gallifreydb::core::vector::DistanceMetric;
    ///
    /// assert!(!DistanceMetric::Cosine.requires_normalized_vectors());
    /// assert!(!DistanceMetric::Euclidean.requires_normalized_vectors());
    /// assert!(DistanceMetric::DotProduct.requires_normalized_vectors());
    /// ```
    #[inline]
    pub const fn requires_normalized_vectors(&self) -> bool {
        matches!(self, DistanceMetric::DotProduct)
    }
}

impl fmt::Display for DistanceMetric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}
