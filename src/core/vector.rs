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
// SIMD Support
// ============================================================================

/// SIMD-accelerated vector operations for x86/x86_64 platforms.
///
/// Uses runtime feature detection to select the best available instruction set:
/// - AVX2 (256-bit vectors, 8 floats at a time)
/// - SSE2 (128-bit vectors, 4 floats at a time) - baseline for x86_64
/// - Scalar fallback for other platforms
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod simd {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    /// Computes dot product, magnitude_a², and magnitude_b² using AVX2.
    ///
    /// # Safety
    /// Caller must ensure AVX2 and FMA are available (checked via `is_x86_feature_detected!`).
    #[target_feature(enable = "avx2", enable = "fma")]
    #[inline]
    pub unsafe fn dot_and_magnitudes_avx2(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
        unsafe {
            let len = a.len();
            let chunks = len / 8;
            let remainder = len % 8;

            // Accumulators for 8 floats at a time
            let mut dot_acc = _mm256_setzero_ps();
            let mut mag_a_acc = _mm256_setzero_ps();
            let mut mag_b_acc = _mm256_setzero_ps();

            let a_ptr = a.as_ptr();
            let b_ptr = b.as_ptr();

            // Process 8 floats at a time
            for i in 0..chunks {
                let offset = i * 8;
                let va = _mm256_loadu_ps(a_ptr.add(offset));
                let vb = _mm256_loadu_ps(b_ptr.add(offset));

                // Fused multiply-add for dot product and magnitudes
                dot_acc = _mm256_fmadd_ps(va, vb, dot_acc);
                mag_a_acc = _mm256_fmadd_ps(va, va, mag_a_acc);
                mag_b_acc = _mm256_fmadd_ps(vb, vb, mag_b_acc);
            }

            // Horizontal sum of 256-bit vectors
            let dot = horizontal_sum_avx(dot_acc);
            let mag_a = horizontal_sum_avx(mag_a_acc);
            let mag_b = horizontal_sum_avx(mag_b_acc);

            // Handle remainder with scalar operations
            let mut dot_rem = 0.0f32;
            let mut mag_a_rem = 0.0f32;
            let mut mag_b_rem = 0.0f32;

            let start = chunks * 8;
            for i in 0..remainder {
                let ai = *a.get_unchecked(start + i);
                let bi = *b.get_unchecked(start + i);
                dot_rem += ai * bi;
                mag_a_rem += ai * ai;
                mag_b_rem += bi * bi;
            }

            (dot + dot_rem, mag_a + mag_a_rem, mag_b + mag_b_rem)
        }
    }

    /// Horizontal sum of 8 floats in AVX register.
    #[target_feature(enable = "avx2")]
    #[inline]
    unsafe fn horizontal_sum_avx(v: __m256) -> f32 {
        unsafe {
            // Add high 128 bits to low 128 bits
            let high = _mm256_extractf128_ps(v, 1);
            let low = _mm256_castps256_ps128(v);
            let sum128 = _mm_add_ps(high, low);

            // Continue with SSE horizontal add
            horizontal_sum_sse(sum128)
        }
    }

    /// Computes dot product, magnitude_a², and magnitude_b² using SSE2.
    ///
    /// # Safety
    /// Caller must ensure SSE2 is available (always true on x86_64).
    #[target_feature(enable = "sse2")]
    #[inline]
    pub unsafe fn dot_and_magnitudes_sse2(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
        unsafe {
            let len = a.len();
            let chunks = len / 4;
            let remainder = len % 4;

            // Accumulators for 4 floats at a time
            let mut dot_acc = _mm_setzero_ps();
            let mut mag_a_acc = _mm_setzero_ps();
            let mut mag_b_acc = _mm_setzero_ps();

            let a_ptr = a.as_ptr();
            let b_ptr = b.as_ptr();

            // Process 4 floats at a time
            for i in 0..chunks {
                let offset = i * 4;
                let va = _mm_loadu_ps(a_ptr.add(offset));
                let vb = _mm_loadu_ps(b_ptr.add(offset));

                // Multiply and accumulate
                dot_acc = _mm_add_ps(dot_acc, _mm_mul_ps(va, vb));
                mag_a_acc = _mm_add_ps(mag_a_acc, _mm_mul_ps(va, va));
                mag_b_acc = _mm_add_ps(mag_b_acc, _mm_mul_ps(vb, vb));
            }

            // Horizontal sum of 128-bit vectors
            let dot = horizontal_sum_sse(dot_acc);
            let mag_a = horizontal_sum_sse(mag_a_acc);
            let mag_b = horizontal_sum_sse(mag_b_acc);

            // Handle remainder with scalar operations
            let mut dot_rem = 0.0f32;
            let mut mag_a_rem = 0.0f32;
            let mut mag_b_rem = 0.0f32;

            let start = chunks * 4;
            for i in 0..remainder {
                let ai = *a.get_unchecked(start + i);
                let bi = *b.get_unchecked(start + i);
                dot_rem += ai * bi;
                mag_a_rem += ai * ai;
                mag_b_rem += bi * bi;
            }

            (dot + dot_rem, mag_a + mag_a_rem, mag_b + mag_b_rem)
        }
    }

    /// Horizontal sum of 4 floats in SSE register.
    #[target_feature(enable = "sse2")]
    #[inline]
    unsafe fn horizontal_sum_sse(v: __m128) -> f32 {
        // Sum pairs: [a+c, b+d, a+c, b+d]
        // SAFETY: We have #[target_feature(enable = "sse2")] so these intrinsics are safe
        let shuf = _mm_shuffle_ps(v, v, 0b10_11_00_01);
        let sum1 = _mm_add_ps(v, shuf);
        // Sum final pair
        let shuf2 = _mm_shuffle_ps(sum1, sum1, 0b00_00_11_10);
        let sum2 = _mm_add_ps(sum1, shuf2);
        _mm_cvtss_f32(sum2)
    }
}

/// Scalar fallback for computing dot product and magnitudes.
///
/// Used on non-x86 platforms or ancient x86 CPUs without SSE2.
#[inline]
#[cfg_attr(
    all(
        any(target_arch = "x86", target_arch = "x86_64"),
        not(miri)
    ),
    allow(dead_code)
)]
fn dot_and_magnitudes_scalar(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    a.iter().zip(b.iter()).fold(
        (0.0f32, 0.0f32, 0.0f32),
        |(dot, mag_a, mag_b), (&ai, &bi)| {
            (dot + ai * bi, mag_a + ai * ai, mag_b + bi * bi)
        },
    )
}

/// Computes dot product and both squared magnitudes using the best available
/// SIMD instructions.
///
/// Uses runtime feature detection to select:
/// - AVX2 with FMA on x86/x86_64 when available
/// - SSE2 on x86/x86_64 as fallback (baseline for x86_64)
/// - Scalar implementation on other platforms
#[inline]
fn dot_and_magnitudes(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    #[cfg(target_arch = "x86_64")]
    {
        // Use runtime detection for best available instruction set
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            // SAFETY: We just verified AVX2 and FMA are available
            return unsafe { simd::dot_and_magnitudes_avx2(a, b) };
        }

        // SAFETY: SSE2 is always available on x86_64 (baseline requirement)
        unsafe { simd::dot_and_magnitudes_sse2(a, b) }
    }

    #[cfg(target_arch = "x86")]
    {
        // Use runtime detection for best available instruction set
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            // SAFETY: We just verified AVX2 and FMA are available
            return unsafe { simd::dot_and_magnitudes_avx2(a, b) };
        }

        if is_x86_feature_detected!("sse2") {
            // SAFETY: We just verified SSE2 is available
            return unsafe { simd::dot_and_magnitudes_sse2(a, b) };
        }

        // Fall through to scalar on ancient x86 without SSE2
        return dot_and_magnitudes_scalar(a, b);
    }

    // Fallback for non-x86 platforms
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    dot_and_magnitudes_scalar(a, b)
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

    // Use SIMD-accelerated computation when available
    let (dot, mag_a_sq, mag_b_sq) = dot_and_magnitudes(a, b);

    let magnitude = (mag_a_sq * mag_b_sq).sqrt();

    // Handle zero vectors
    if magnitude == 0.0 {
        return Ok(0.0);
    }

    // Clamp to handle floating-point inaccuracies that could produce
    // values slightly outside [-1.0, 1.0]
    Ok((dot / magnitude).clamp(-1.0, 1.0))
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
                (-1.0..=1.0).contains(&sim),
                "Similarity {} is out of range [-1, 1] for vectors {:?} and {:?}",
                sim, a, b
            );
        }
    }

    // ========================================================================
    // NaN/Inf Propagation Tests
    // ========================================================================

    #[test]
    fn test_cosine_similarity_nan_propagation() {
        let a = vec![f32::NAN, 1.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(sim.is_nan(), "NaN in input should propagate to output");
    }

    #[test]
    fn test_cosine_similarity_nan_in_second_vector() {
        let a = vec![1.0, 1.0];
        let b = vec![1.0, f32::NAN];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(sim.is_nan(), "NaN in second vector should propagate to output");
    }

    #[test]
    fn test_cosine_similarity_inf_propagation() {
        let a = vec![f32::INFINITY, 1.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        // Inf * finite = Inf, and the computation will result in Inf/Inf = NaN
        // or a valid number depending on the exact math
        assert!(
            sim.is_nan() || sim.is_infinite() || (-1.0..=1.0).contains(&sim),
            "Inf should propagate in some form, got {}",
            sim
        );
    }

    #[test]
    fn test_cosine_similarity_neg_inf_propagation() {
        let a = vec![f32::NEG_INFINITY, 1.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(
            sim.is_nan() || sim.is_infinite() || (-1.0..=1.0).contains(&sim),
            "Negative Inf should propagate in some form, got {}",
            sim
        );
    }

    #[test]
    fn test_cosine_similarity_both_inf_same_sign() {
        let a = vec![f32::INFINITY, 0.0];
        let b = vec![f32::INFINITY, 0.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        // Inf * Inf = Inf, sqrt(Inf * Inf) = Inf, Inf/Inf = NaN
        assert!(sim.is_nan(), "Inf/Inf should be NaN, got {}", sim);
    }
}

// ============================================================================
// Property-Based Tests
// ============================================================================

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    // ========================================================================
    // Tolerance Choice Documentation
    // ========================================================================
    //
    // We use 1e-5 absolute tolerance for property tests. This was chosen based on:
    //
    // 1. **f32 precision limits**: f32 has ~7 decimal digits of precision.
    //    For values in [-100, 100] range with up to 100 dimensions, accumulated
    //    error from multiply-add operations is bounded by roughly:
    //    - Per operation: ~1e-7 relative error (f32 epsilon)
    //    - 100 operations: ~1e-5 accumulated error (sqrt(n) * epsilon for random errors)
    //
    // 2. **SIMD vs scalar consistency**: Different code paths (AVX2, SSE2, scalar)
    //    may produce slightly different results due to operation ordering.
    //    1e-5 tolerance accounts for these differences.
    //
    // 3. **Mathematical invariants**: Cosine similarity is bounded to [-1, 1],
    //    so 1e-5 represents a relative error of 0.001% at worst.
    //
    // For applications requiring tighter bounds, normalize vectors first
    // (which reduces magnitude-related accumulation errors).
    // ========================================================================

    /// Tolerance for property-based tests.
    ///
    /// See module-level documentation for rationale behind this choice.
    const PROPTEST_TOLERANCE: f32 = 1e-5;

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
            prop_assert!((sim_ab - sim_ba).abs() < PROPTEST_TOLERANCE);
        }

        #[test]
        fn prop_cosine_similarity_in_range(
            (a, b) in same_length_vectors(100)
        ) {
            let sim = cosine_similarity(&a, &b).unwrap();
            // Handle NaN case (can occur with extreme values)
            if !sim.is_nan() {
                // Result is clamped, so should always be in [-1, 1]
                // Allow tiny tolerance for floating-point edge cases
                let min = -1.0 - PROPTEST_TOLERANCE;
                let max = 1.0 + PROPTEST_TOLERANCE;
                prop_assert!((min..=max).contains(&sim),
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
                prop_assert!((sim - 1.0).abs() < PROPTEST_TOLERANCE,
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
                prop_assert!((sim + 1.0).abs() < PROPTEST_TOLERANCE,
                    "Negation similarity should be -1.0, got {} for {:?}", sim, a);
            }
        }

        #[test]
        fn prop_cosine_similarity_scale_invariant(
            (a, b) in same_length_vectors(50),
            scale in 0.1f32..10.0f32
        ) {
            // Skip if either vector is near-zero
            let mag_a: f32 = a.iter().map(|x| x * x).sum();
            let mag_b: f32 = b.iter().map(|x| x * x).sum();
            if mag_a > 1e-10 && mag_b > 1e-10 {
                let sim1 = cosine_similarity(&a, &b).unwrap();

                let scaled_a: Vec<f32> = a.iter().map(|x| x * scale).collect();
                let sim2 = cosine_similarity(&scaled_a, &b).unwrap();

                // Cosine similarity should be scale-invariant
                // Use slightly larger tolerance for scale operations due to additional
                // floating-point operations from scaling
                if !sim1.is_nan() && !sim2.is_nan() {
                    prop_assert!((sim1 - sim2).abs() < PROPTEST_TOLERANCE * 10.0,
                        "Scale invariance failed: {} vs {} for scale {}", sim1, sim2, scale);
                }
            }
        }
    }
}
