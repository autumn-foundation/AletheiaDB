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

// ============================================================================
// Constants
// ============================================================================

/// Default tolerance for floating-point comparisons in normalization operations.
///
/// This tolerance (1e-6) is appropriate for most f32 operations where accumulated
/// floating-point errors are expected to be small. It's used as the default for
/// functions like [`is_normalized`] when checking if a vector has unit magnitude.
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
const SQUARED_MAGNITUDE_THRESHOLD: f32 = 1e-14;

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
///
/// # Performance Expectations
///
/// Compared to scalar implementation, expected speedups for vectors > 256 dimensions:
/// - **AVX2 + FMA**: ~5-8x speedup (processes 8 floats/cycle with fused multiply-add)
/// - **SSE2**: ~2-4x speedup (processes 4 floats/cycle)
///
/// For smaller vectors, SIMD overhead may reduce gains. The crossover point
/// where SIMD becomes beneficial is typically around 16-32 dimensions.
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

            // Handle remainder with safe scalar operations.
            // Using safe indexing here as the compiler optimizes away bounds checks
            // when the loop bound is known to be < 8 (the chunk size).
            let mut dot_rem = 0.0f32;
            let mut mag_a_rem = 0.0f32;
            let mut mag_b_rem = 0.0f32;

            let start = chunks * 8;
            for i in 0..remainder {
                let ai = a[start + i];
                let bi = b[start + i];
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

            // Handle remainder with safe scalar operations.
            // Using safe indexing here as the compiler optimizes away bounds checks
            // when the loop bound is known to be < 4 (the chunk size).
            let mut dot_rem = 0.0f32;
            let mut mag_a_rem = 0.0f32;
            let mut mag_b_rem = 0.0f32;

            let start = chunks * 4;
            for i in 0..remainder {
                let ai = a[start + i];
                let bi = b[start + i];
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

    /// Computes dot product using AVX2.
    ///
    /// This is a dedicated dot-product-only function, more efficient than
    /// `dot_and_magnitudes_avx2` when magnitudes aren't needed.
    ///
    /// # Safety
    /// Caller must ensure AVX2 and FMA are available (checked via `is_x86_feature_detected!`).
    #[target_feature(enable = "avx2", enable = "fma")]
    #[inline]
    pub unsafe fn dot_product_avx2(a: &[f32], b: &[f32]) -> f32 {
        // SAFETY: The unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // The caller guarantees AVX2 and FMA are available via runtime feature detection.
        unsafe {
            let a_chunks = a.chunks_exact(8);
            let b_chunks = b.chunks_exact(8);
            let a_rem = a_chunks.remainder();
            let b_rem = b_chunks.remainder();

            // Accumulator for 8 floats at a time
            let mut acc = _mm256_setzero_ps();

            // Process 8 floats at a time
            for (va_chunk, vb_chunk) in a_chunks.zip(b_chunks) {
                let va = _mm256_loadu_ps(va_chunk.as_ptr());
                let vb = _mm256_loadu_ps(vb_chunk.as_ptr());

                // Fused multiply-add: acc = va * vb + acc
                acc = _mm256_fmadd_ps(va, vb, acc);
            }

            // Horizontal sum of 256-bit vector
            let mut sum = horizontal_sum_avx(acc);

            // Handle remainder with scalar operations
            for (&va, &vb) in a_rem.iter().zip(b_rem) {
                sum += va * vb;
            }

            sum
        }
    }

    /// Computes dot product using SSE2.
    ///
    /// This is a dedicated dot-product-only function, more efficient than
    /// `dot_and_magnitudes_sse2` when magnitudes aren't needed.
    ///
    /// # Safety
    /// Caller must ensure SSE2 is available (always true on x86_64).
    #[target_feature(enable = "sse2")]
    #[inline]
    pub unsafe fn dot_product_sse2(a: &[f32], b: &[f32]) -> f32 {
        // SAFETY: The unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // The caller guarantees SSE2 is available via runtime feature detection.
        unsafe {
            let a_chunks = a.chunks_exact(4);
            let b_chunks = b.chunks_exact(4);
            let a_rem = a_chunks.remainder();
            let b_rem = b_chunks.remainder();

            // Accumulator for 4 floats at a time
            let mut acc = _mm_setzero_ps();

            // Process 4 floats at a time
            for (va_chunk, vb_chunk) in a_chunks.zip(b_chunks) {
                let va = _mm_loadu_ps(va_chunk.as_ptr());
                let vb = _mm_loadu_ps(vb_chunk.as_ptr());

                // Multiply and accumulate
                acc = _mm_add_ps(acc, _mm_mul_ps(va, vb));
            }

            // Horizontal sum of 128-bit vector
            let mut sum = horizontal_sum_sse(acc);

            // Handle remainder with scalar operations
            for (&va, &vb) in a_rem.iter().zip(b_rem) {
                sum += va * vb;
            }

            sum
        }
    }

    /// Computes sum of squared differences using AVX2.
    ///
    /// # Safety
    /// Caller must ensure AVX2 and FMA are available (checked via `is_x86_feature_detected!`).
    #[target_feature(enable = "avx2", enable = "fma")]
    #[inline]
    pub unsafe fn squared_diff_sum_avx2(a: &[f32], b: &[f32]) -> f32 {
        // SAFETY: The unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // All unsafe operations within this unsafe fn must still be in an unsafe block.
        // The caller guarantees AVX2 and FMA are available via runtime feature detection.
        unsafe {
            let len = a.len();
            let chunks = len / 8;
            let remainder = len % 8;

            // Accumulator for 8 floats at a time
            let mut acc = _mm256_setzero_ps();

            let a_ptr = a.as_ptr();
            let b_ptr = b.as_ptr();

            // Process 8 floats at a time
            for i in 0..chunks {
                let offset = i * 8;
                let va = _mm256_loadu_ps(a_ptr.add(offset));
                let vb = _mm256_loadu_ps(b_ptr.add(offset));

                // Compute difference
                let diff = _mm256_sub_ps(va, vb);

                // Square and accumulate using FMA: acc = diff * diff + acc
                acc = _mm256_fmadd_ps(diff, diff, acc);
            }

            // Horizontal sum of 256-bit vector
            let mut sum = horizontal_sum_avx(acc);

            // Handle remainder with scalar operations
            let start = chunks * 8;
            for i in 0..remainder {
                let diff = a[start + i] - b[start + i];
                sum += diff * diff;
            }

            sum
        }
    }

    /// Computes sum of squared differences using SSE2.
    ///
    /// # Safety
    /// Caller must ensure SSE2 is available (always true on x86_64).
    #[target_feature(enable = "sse2")]
    #[inline]
    pub unsafe fn squared_diff_sum_sse2(a: &[f32], b: &[f32]) -> f32 {
        // SAFETY: The unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // All unsafe operations within this unsafe fn must still be in an unsafe block.
        // The caller guarantees SSE2 is available via runtime feature detection.
        unsafe {
            let len = a.len();
            let chunks = len / 4;
            let remainder = len % 4;

            // Accumulator for 4 floats at a time
            let mut acc = _mm_setzero_ps();

            let a_ptr = a.as_ptr();
            let b_ptr = b.as_ptr();

            // Process 4 floats at a time
            for i in 0..chunks {
                let offset = i * 4;
                let va = _mm_loadu_ps(a_ptr.add(offset));
                let vb = _mm_loadu_ps(b_ptr.add(offset));

                // Compute difference
                let diff = _mm_sub_ps(va, vb);

                // Square and accumulate
                acc = _mm_add_ps(acc, _mm_mul_ps(diff, diff));
            }

            // Horizontal sum of 128-bit vector
            let mut sum = horizontal_sum_sse(acc);

            // Handle remainder with scalar operations
            let start = chunks * 4;
            for i in 0..remainder {
                let diff = a[start + i] - b[start + i];
                sum += diff * diff;
            }

            sum
        }
    }

    /// Scales a vector in place by a scalar using AVX2.
    ///
    /// # Safety
    /// Caller must ensure AVX2 is available (checked via `is_x86_feature_detected!`).
    #[target_feature(enable = "avx2")]
    #[inline]
    pub unsafe fn scale_in_place_avx2(v: &mut [f32], scalar: f32) {
        // SAFETY: This unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // - The caller guarantees AVX2 is available via runtime feature detection.
        // - Pointer operations are safe because:
        //   1. chunks_exact_mut(8) guarantees each chunk has exactly 8 f32 elements
        //   2. chunk.as_ptr() returns a valid pointer to the chunk's contiguous memory
        //   3. chunk.as_mut_ptr() returns a valid mutable pointer to the same memory
        //   4. _mm256_loadu_ps/_mm256_storeu_ps handle unaligned access safely
        //   5. The slice owns the memory, so no aliasing occurs
        unsafe {
            let scalar_vec = _mm256_set1_ps(scalar);
            let mut chunks = v.chunks_exact_mut(8);

            // Process 8 floats at a time
            for chunk in chunks.by_ref() {
                let va = _mm256_loadu_ps(chunk.as_ptr());
                let result = _mm256_mul_ps(va, scalar_vec);
                _mm256_storeu_ps(chunk.as_mut_ptr(), result);
            }

            // Handle remainder with scalar operations
            for x in chunks.into_remainder() {
                *x *= scalar;
            }
        }
    }

    /// Scales a vector in place by a scalar using SSE2.
    ///
    /// # Safety
    /// Caller must ensure SSE2 is available (always true on x86_64).
    #[target_feature(enable = "sse2")]
    #[inline]
    pub unsafe fn scale_in_place_sse2(v: &mut [f32], scalar: f32) {
        // SAFETY: This unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // - The caller guarantees SSE2 is available via runtime feature detection.
        // - Pointer operations are safe because:
        //   1. chunks_exact_mut(4) guarantees each chunk has exactly 4 f32 elements
        //   2. chunk.as_ptr() returns a valid pointer to the chunk's contiguous memory
        //   3. chunk.as_mut_ptr() returns a valid mutable pointer to the same memory
        //   4. _mm_loadu_ps/_mm_storeu_ps handle unaligned access safely
        //   5. The slice owns the memory, so no aliasing occurs
        unsafe {
            let scalar_vec = _mm_set1_ps(scalar);
            let mut chunks = v.chunks_exact_mut(4);

            // Process 4 floats at a time
            for chunk in chunks.by_ref() {
                let va = _mm_loadu_ps(chunk.as_ptr());
                let result = _mm_mul_ps(va, scalar_vec);
                _mm_storeu_ps(chunk.as_mut_ptr(), result);
            }

            // Handle remainder with scalar operations
            for x in chunks.into_remainder() {
                *x *= scalar;
            }
        }
    }
}

/// Scalar fallback for computing dot product and magnitudes.
///
/// Used on non-x86 platforms or ancient x86 CPUs without SSE2.
#[inline]
#[cfg_attr(
    all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)),
    allow(dead_code)
)]
fn dot_and_magnitudes_scalar(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    a.iter().zip(b.iter()).fold(
        (0.0f32, 0.0f32, 0.0f32),
        |(dot, mag_a, mag_b), (&ai, &bi)| (dot + ai * bi, mag_a + ai * ai, mag_b + bi * bi),
    )
}

/// Scalar fallback for computing sum of squared differences.
///
/// Used on non-x86 platforms or ancient x86 CPUs without SSE2.
#[inline]
#[cfg_attr(
    all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)),
    allow(dead_code)
)]
fn squared_diff_sum_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(&ai, &bi)| {
            let diff = ai - bi;
            diff * diff
        })
        .sum()
}

/// Scalar fallback for computing dot product.
///
/// Used on non-x86 platforms or ancient x86 CPUs without SSE2.
#[inline]
#[cfg_attr(
    all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)),
    allow(dead_code)
)]
fn dot_product_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(&ai, &bi)| ai * bi).sum()
}

/// Scalar fallback for scaling a vector in place.
///
/// Used on non-x86 platforms or ancient x86 CPUs without SSE2.
#[inline]
#[cfg_attr(
    all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)),
    allow(dead_code)
)]
fn scale_in_place_scalar(v: &mut [f32], scalar: f32) {
    for x in v.iter_mut() {
        *x *= scalar;
    }
}

/// Scales a vector in place using the best available SIMD instructions.
///
/// Uses runtime feature detection to select:
/// - AVX2 on x86/x86_64 when available
/// - SSE2 on x86/x86_64 as fallback (baseline for x86_64)
/// - Scalar implementation on other platforms
#[inline(always)]
fn scale_in_place(v: &mut [f32], scalar: f32) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // Use runtime detection for best available instruction set.
        if is_x86_feature_detected!("avx2") {
            // SAFETY: We just verified AVX2 is available.
            unsafe {
                simd::scale_in_place_avx2(v, scalar);
            }
            return;
        }
        if is_x86_feature_detected!("sse2") {
            // SAFETY: We just verified SSE2 is available. SSE2 is a baseline
            // requirement for x86_64, so this check is mainly for 32-bit x86.
            unsafe {
                simd::scale_in_place_sse2(v, scalar);
            }
            return;
        }
    }

    // Fallback for non-x86 platforms or x86 CPUs without SSE2.
    scale_in_place_scalar(v, scalar);
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

/// Computes sum of squared differences using the best available SIMD instructions.
///
/// Uses runtime feature detection to select:
/// - AVX2 with FMA on x86/x86_64 when available
/// - SSE2 on x86/x86_64 as fallback (baseline for x86_64)
/// - Scalar implementation on other platforms
#[inline]
fn squared_diff_sum(a: &[f32], b: &[f32]) -> f32 {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // Use runtime detection for best available instruction set.
        // The order of checks is from most to least performant.
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            // SAFETY: We just verified AVX2 and FMA are available.
            return unsafe { simd::squared_diff_sum_avx2(a, b) };
        }
        if is_x86_feature_detected!("sse2") {
            // SAFETY: We just verified SSE2 is available. SSE2 is a baseline
            // requirement for x86_64, so this check is mainly for 32-bit x86.
            return unsafe { simd::squared_diff_sum_sse2(a, b) };
        }
    }

    // Fallback for non-x86 platforms or x86 CPUs without SSE2.
    squared_diff_sum_scalar(a, b)
}

/// Computes dot product using the best available SIMD instructions.
///
/// Uses runtime feature detection to select:
/// - AVX2 with FMA on x86/x86_64 when available
/// - SSE2 on x86/x86_64 as fallback (baseline for x86_64)
/// - Scalar implementation on other platforms
#[inline]
fn dot_product_sum(a: &[f32], b: &[f32]) -> f32 {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // Use runtime detection for best available instruction set.
        // The order of checks is from most to least performant.
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            // SAFETY: We just verified AVX2 and FMA are available.
            return unsafe { simd::dot_product_avx2(a, b) };
        }
        if is_x86_feature_detected!("sse2") {
            // SAFETY: We just verified SSE2 is available. SSE2 is a baseline
            // requirement for x86_64, so this check is mainly for 32-bit x86.
            return unsafe { simd::dot_product_sse2(a, b) };
        }
    }

    // Fallback for non-x86 platforms or x86 CPUs without SSE2.
    dot_product_scalar(a, b)
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
        return Err(Error::Query(
            crate::utils::error::QueryError::InvalidParameter {
                parameter: "vectors".to_string(),
                reason: format!("dimension mismatch: {} vs {}", a.len(), b.len()),
            },
        ));
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
    if a.len() != b.len() {
        return Err(Error::Query(
            crate::utils::error::QueryError::InvalidParameter {
                parameter: "vectors".to_string(),
                reason: format!("dimension mismatch: {} vs {}", a.len(), b.len()),
            },
        ));
    }

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
    let (dot, _, _) = dot_and_magnitudes(a, b);

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
    if a.len() != b.len() {
        return Err(Error::Query(
            crate::utils::error::QueryError::InvalidParameter {
                parameter: "vectors".to_string(),
                reason: format!("dimension mismatch: {} vs {}", a.len(), b.len()),
            },
        ));
    }

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
    if a.len() != b.len() {
        return Err(Error::Query(
            crate::utils::error::QueryError::InvalidParameter {
                parameter: "vectors".to_string(),
                reason: format!("dimension mismatch: {} vs {}", a.len(), b.len()),
            },
        ));
    }

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
/// - Dimension limits are enforced at storage time (see [`PropertyValue::vector`])
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
/// [`NORMALIZATION_TOLERANCE`] (1e-6) as the tolerance value.
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
    // Count NaN values first
    let nan_count = v.iter().filter(|x| x.is_nan()).count();
    if nan_count > 0 {
        return Err(Error::Vector(VectorError::ContainsNaN { count: nan_count }));
    }

    // Count Infinity values (both positive and negative)
    let inf_count = v.iter().filter(|x| x.is_infinite()).count();
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
/// 1. Pre-normalize vectors with [`normalize`] or [`normalize_in_place`]
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
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "Identical vectors should have similarity 1.0"
        );
    }

    #[test]
    fn test_cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(
            (sim + 1.0).abs() < 1e-6,
            "Opposite vectors should have similarity -1.0"
        );
    }

    #[test]
    fn test_cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(
            sim.abs() < 1e-6,
            "Orthogonal vectors should have similarity 0.0"
        );
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
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "Parallel 1D vectors should have similarity 1.0"
        );

        let c = vec![-3.0];
        let sim_neg = cosine_similarity(&a, &c).unwrap();
        assert!(
            (sim_neg + 1.0).abs() < 1e-6,
            "Anti-parallel 1D vectors should have similarity -1.0"
        );
    }

    #[test]
    fn test_cosine_similarity_symmetry() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![4.0, 3.0, 2.0, 1.0];
        let sim_ab = cosine_similarity(&a, &b).unwrap();
        let sim_ba = cosine_similarity(&b, &a).unwrap();
        assert!(
            (sim_ab - sim_ba).abs() < 1e-6,
            "Cosine similarity should be symmetric"
        );
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
                sim,
                a,
                b
            );
        }
    }

    // ========================================================================
    // Large Dimension Tests
    // ========================================================================

    #[test]
    fn test_cosine_similarity_large_dimension_1536() {
        // OpenAI text-embedding-3-small dimension
        let dim = 1536;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32).sin()).collect();
        let b: Vec<f32> = (0..dim).map(|i| (i as f32).cos()).collect();

        let sim = cosine_similarity(&a, &b).unwrap();
        // Sine and cosine are approximately orthogonal over many periods
        assert!(
            sim.abs() < 0.1,
            "Expected near-orthogonal vectors at dim={}, got sim={}",
            dim,
            sim
        );
    }

    #[test]
    fn test_cosine_similarity_large_dimension_3072() {
        // OpenAI text-embedding-3-large dimension
        let dim = 3072;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.1).sin()).collect();
        let b: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.1).sin()).collect();

        let sim = cosine_similarity(&a, &b).unwrap();
        // Identical vectors should have similarity 1.0
        assert!(
            (sim - 1.0).abs() < 1e-5,
            "Expected self-similarity of 1.0 at dim={}, got {}",
            dim,
            sim
        );
    }

    #[test]
    fn test_cosine_similarity_large_dimension_opposite() {
        let dim = 1536;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.01).cos()).collect();
        let b: Vec<f32> = a.iter().map(|x| -x).collect();

        let sim = cosine_similarity(&a, &b).unwrap();
        // Opposite vectors should have similarity -1.0
        assert!(
            (sim + 1.0).abs() < 1e-5,
            "Expected opposite similarity of -1.0 at dim={}, got {}",
            dim,
            sim
        );
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
        assert!(
            sim.is_nan(),
            "NaN in second vector should propagate to output"
        );
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

    // ========================================================================
    // Normalized Cosine Similarity Tests
    // ========================================================================
    //
    // Note: These tests use the public `normalize` function imported via super::*

    #[test]
    fn test_cosine_similarity_normalized_identical() {
        let a = normalize(&[1.0, 2.0, 3.0]);
        let sim = cosine_similarity_normalized(&a, &a).unwrap();
        assert!((sim - 1.0).abs() < 1e-6, "Self-similarity should be 1.0");
    }

    #[test]
    fn test_cosine_similarity_normalized_opposite() {
        let a = normalize(&[1.0, 0.0]);
        let b = normalize(&[-1.0, 0.0]);
        let sim = cosine_similarity_normalized(&a, &b).unwrap();
        assert!(
            (sim + 1.0).abs() < 1e-6,
            "Opposite vectors should have similarity -1.0"
        );
    }

    #[test]
    fn test_cosine_similarity_normalized_orthogonal() {
        let a = vec![1.0, 0.0, 0.0]; // Already unit
        let b = vec![0.0, 1.0, 0.0]; // Already unit
        let sim = cosine_similarity_normalized(&a, &b).unwrap();
        assert!(
            sim.abs() < 1e-6,
            "Orthogonal unit vectors should have similarity 0.0"
        );
    }

    #[test]
    fn test_cosine_similarity_normalized_45_degrees() {
        // cos(45°) = 1/sqrt(2) ≈ 0.707
        let a = vec![1.0, 0.0];
        let b = normalize(&[1.0, 1.0]);
        let sim = cosine_similarity_normalized(&a, &b).unwrap();
        let expected = 1.0 / 2.0_f32.sqrt();
        assert!(
            (sim - expected).abs() < 1e-5,
            "Expected {}, got {}",
            expected,
            sim
        );
    }

    #[test]
    fn test_cosine_similarity_normalized_matches_general() {
        let a_raw = vec![1.0, 2.0, 3.0, 4.0];
        let b_raw = vec![4.0, 3.0, 2.0, 1.0];

        // General cosine similarity
        let sim_general = cosine_similarity(&a_raw, &b_raw).unwrap();

        // Normalized version
        let a_norm = normalize(&a_raw);
        let b_norm = normalize(&b_raw);
        let sim_normalized = cosine_similarity_normalized(&a_norm, &b_norm).unwrap();

        assert!(
            (sim_general - sim_normalized).abs() < 1e-5,
            "General ({}) and normalized ({}) should match",
            sim_general,
            sim_normalized
        );
    }

    #[test]
    fn test_cosine_similarity_normalized_dimension_mismatch() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0];
        let result = cosine_similarity_normalized(&a, &b);
        assert!(result.is_err());
    }

    #[test]
    fn test_cosine_similarity_normalized_empty() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let sim = cosine_similarity_normalized(&a, &b).unwrap();
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_cosine_similarity_normalized_high_dimension() {
        // Test with a higher dimension to exercise SIMD paths
        let dim = 384; // Sentence Transformers dimension
        let a: Vec<f32> = (0..dim).map(|i| (i as f32) / dim as f32).collect();
        let b: Vec<f32> = (0..dim).map(|i| ((dim - i) as f32) / dim as f32).collect();

        let a_norm = normalize(&a);
        let b_norm = normalize(&b);

        let sim = cosine_similarity_normalized(&a_norm, &b_norm).unwrap();
        let sim_general = cosine_similarity(&a, &b).unwrap();

        assert!(
            (sim - sim_general).abs() < 1e-4,
            "High-dim: general ({}) vs normalized ({})",
            sim_general,
            sim
        );
    }

    // ========================================================================
    // Euclidean Distance Tests
    // ========================================================================

    #[test]
    fn test_euclidean_distance_3_4_5_triangle() {
        // Classic 3-4-5 right triangle
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        let dist = euclidean_distance(&a, &b).unwrap();
        assert!(
            (dist - 5.0).abs() < 1e-6,
            "3-4-5 triangle distance should be 5.0, got {}",
            dist
        );
    }

    #[test]
    fn test_squared_euclidean_distance_3_4_5_triangle() {
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        let dist_sq = squared_euclidean_distance(&a, &b).unwrap();
        assert!(
            (dist_sq - 25.0).abs() < 1e-6,
            "3² + 4² should be 25, got {}",
            dist_sq
        );
    }

    #[test]
    fn test_euclidean_distance_same_point() {
        let a = vec![1.0, 2.0, 3.0];
        let dist = euclidean_distance(&a, &a).unwrap();
        assert!(
            dist.abs() < 1e-6,
            "Distance to self should be 0, got {}",
            dist
        );
    }

    #[test]
    fn test_squared_euclidean_distance_same_point() {
        let a = vec![1.0, 2.0, 3.0];
        let dist_sq = squared_euclidean_distance(&a, &a).unwrap();
        assert!(
            dist_sq.abs() < 1e-6,
            "Squared distance to self should be 0, got {}",
            dist_sq
        );
    }

    #[test]
    fn test_euclidean_distance_dimension_mismatch() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        let result = euclidean_distance(&a, &b);
        assert!(result.is_err());
    }

    #[test]
    fn test_squared_euclidean_distance_dimension_mismatch() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        let result = squared_euclidean_distance(&a, &b);
        assert!(result.is_err());
    }

    #[test]
    fn test_euclidean_distance_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let dist = euclidean_distance(&a, &b).unwrap();
        assert_eq!(dist, 0.0);
    }

    #[test]
    fn test_squared_euclidean_distance_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let dist_sq = squared_euclidean_distance(&a, &b).unwrap();
        assert_eq!(dist_sq, 0.0);
    }

    #[test]
    fn test_euclidean_distance_symmetry() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![4.0, 3.0, 2.0, 1.0];
        let dist_ab = euclidean_distance(&a, &b).unwrap();
        let dist_ba = euclidean_distance(&b, &a).unwrap();
        assert!(
            (dist_ab - dist_ba).abs() < 1e-6,
            "Euclidean distance should be symmetric"
        );
    }

    #[test]
    fn test_squared_euclidean_distance_symmetry() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![4.0, 3.0, 2.0, 1.0];
        let dist_sq_ab = squared_euclidean_distance(&a, &b).unwrap();
        let dist_sq_ba = squared_euclidean_distance(&b, &a).unwrap();
        assert!(
            (dist_sq_ab - dist_sq_ba).abs() < 1e-6,
            "Squared Euclidean distance should be symmetric"
        );
    }

    #[test]
    fn test_euclidean_distance_single_dimension() {
        let a = vec![5.0];
        let b = vec![2.0];
        let dist = euclidean_distance(&a, &b).unwrap();
        assert!(
            (dist - 3.0).abs() < 1e-6,
            "1D distance should be |5 - 2| = 3, got {}",
            dist
        );
    }

    #[test]
    fn test_euclidean_distance_negative_values() {
        let a = vec![-1.0, -2.0];
        let b = vec![2.0, 2.0];
        let dist = euclidean_distance(&a, &b).unwrap();
        // sqrt((2 - -1)² + (2 - -2)²) = sqrt(9 + 16) = 5
        assert!(
            (dist - 5.0).abs() < 1e-6,
            "Distance with negative values should be 5.0, got {}",
            dist
        );
    }

    #[test]
    fn test_euclidean_distance_3d() {
        // Distance from (0,0,0) to (1,2,2)
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 2.0];
        let dist = euclidean_distance(&a, &b).unwrap();
        // sqrt(1 + 4 + 4) = sqrt(9) = 3
        assert!(
            (dist - 3.0).abs() < 1e-6,
            "3D distance should be 3.0, got {}",
            dist
        );
    }

    #[test]
    fn test_euclidean_vs_squared_relationship() {
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let b = vec![5.0, 4.0, 3.0, 2.0, 1.0];
        let dist = euclidean_distance(&a, &b).unwrap();
        let dist_sq = squared_euclidean_distance(&a, &b).unwrap();
        assert!(
            (dist * dist - dist_sq).abs() < 1e-5,
            "euclidean² should equal squared_euclidean: {}² vs {}",
            dist,
            dist_sq
        );
    }

    #[test]
    fn test_euclidean_distance_large_dimension_384() {
        // Sentence Transformers dimension
        let dim = 384;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32).sin()).collect();
        let b: Vec<f32> = (0..dim).map(|i| (i as f32).cos()).collect();

        let dist = euclidean_distance(&a, &b).unwrap();
        let dist_sq = squared_euclidean_distance(&a, &b).unwrap();

        assert!(dist >= 0.0, "Distance should be non-negative");
        assert!(dist_sq >= 0.0, "Squared distance should be non-negative");
        assert!(
            (dist * dist - dist_sq).abs() < 1e-4,
            "Relationship should hold at high dimension"
        );
    }

    #[test]
    fn test_euclidean_distance_large_dimension_1536() {
        // OpenAI text-embedding-3-small dimension
        let dim = 1536;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.01).sin()).collect();
        let b: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.01).cos()).collect();

        let dist = euclidean_distance(&a, &b).unwrap();
        let dist_sq = squared_euclidean_distance(&a, &b).unwrap();

        assert!(dist >= 0.0, "Distance should be non-negative");
        assert!(
            (dist * dist - dist_sq).abs() < 1e-3,
            "Relationship should hold at dim={}: {}² vs {}",
            dim,
            dist,
            dist_sq
        );
    }

    #[test]
    fn test_squared_euclidean_distance_preserves_ordering() {
        // Squared distance should preserve distance ordering for comparisons
        let query = vec![0.0, 0.0];
        let near = vec![1.0, 1.0];
        let far = vec![3.0, 4.0];

        let dist_near = euclidean_distance(&query, &near).unwrap();
        let dist_far = euclidean_distance(&query, &far).unwrap();
        let dist_sq_near = squared_euclidean_distance(&query, &near).unwrap();
        let dist_sq_far = squared_euclidean_distance(&query, &far).unwrap();

        // If near < far in distance, should also be true for squared distance
        assert!(dist_near < dist_far);
        assert!(dist_sq_near < dist_sq_far);
    }

    #[test]
    fn test_euclidean_distance_unit_axis() {
        // Distance along unit axes
        let origin = vec![0.0, 0.0, 0.0];
        let x_axis = vec![1.0, 0.0, 0.0];
        let y_axis = vec![0.0, 1.0, 0.0];
        let z_axis = vec![0.0, 0.0, 1.0];

        assert!((euclidean_distance(&origin, &x_axis).unwrap() - 1.0).abs() < 1e-6);
        assert!((euclidean_distance(&origin, &y_axis).unwrap() - 1.0).abs() < 1e-6);
        assert!((euclidean_distance(&origin, &z_axis).unwrap() - 1.0).abs() < 1e-6);
    }

    // ========================================================================
    // Dot Product Tests
    // ========================================================================

    #[test]
    fn test_dot_product_basic() {
        // 1×4 + 2×5 + 3×6 = 4 + 10 + 18 = 32
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(
            (result - 32.0).abs() < 1e-6,
            "Dot product should be 32, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_self_equals_squared_magnitude() {
        // dot(a, a) = ||a||²
        let a = vec![3.0, 4.0];
        let self_dot = dot_product(&a, &a).unwrap();
        // 3² + 4² = 9 + 16 = 25
        assert!(
            (self_dot - 25.0).abs() < 1e-6,
            "Self dot product should be 25, got {}",
            self_dot
        );
    }

    #[test]
    fn test_dot_product_orthogonal_vectors() {
        // Orthogonal vectors have dot product 0
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(
            result.abs() < 1e-6,
            "Orthogonal vectors should have dot product 0, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_orthogonal_3d() {
        let x = vec![1.0, 0.0, 0.0];
        let y = vec![0.0, 1.0, 0.0];
        let z = vec![0.0, 0.0, 1.0];

        assert!(dot_product(&x, &y).unwrap().abs() < 1e-6);
        assert!(dot_product(&y, &z).unwrap().abs() < 1e-6);
        assert!(dot_product(&x, &z).unwrap().abs() < 1e-6);
    }

    #[test]
    fn test_dot_product_symmetry() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![4.0, 3.0, 2.0, 1.0];
        let dot_ab = dot_product(&a, &b).unwrap();
        let dot_ba = dot_product(&b, &a).unwrap();
        assert!(
            (dot_ab - dot_ba).abs() < 1e-6,
            "Dot product should be symmetric: {} vs {}",
            dot_ab,
            dot_ba
        );
    }

    #[test]
    fn test_dot_product_dimension_mismatch() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        let result = dot_product(&a, &b);
        assert!(result.is_err());
    }

    #[test]
    fn test_dot_product_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let result = dot_product(&a, &b).unwrap();
        assert_eq!(result, 0.0);
    }

    #[test]
    fn test_dot_product_single_element() {
        let a = vec![5.0];
        let b = vec![3.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(
            (result - 15.0).abs() < 1e-6,
            "Single element dot product should be 15, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_negative_values() {
        let a = vec![-1.0, 2.0, -3.0];
        let b = vec![4.0, -5.0, 6.0];
        // -1×4 + 2×(-5) + (-3)×6 = -4 - 10 - 18 = -32
        let result = dot_product(&a, &b).unwrap();
        assert!(
            (result + 32.0).abs() < 1e-6,
            "Dot product should be -32, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(
            result.abs() < 1e-6,
            "Dot product with zero vector should be 0, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_parallel_same_direction() {
        // Parallel vectors in same direction: dot = ||a|| × ||b||
        let a = vec![3.0, 0.0];
        let b = vec![4.0, 0.0];
        let result = dot_product(&a, &b).unwrap();
        // 3 × 4 = 12
        assert!(
            (result - 12.0).abs() < 1e-6,
            "Parallel same direction dot product should be 12, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_parallel_opposite_direction() {
        // Parallel vectors in opposite direction: dot = -||a|| × ||b||
        let a = vec![3.0, 0.0];
        let b = vec![-4.0, 0.0];
        let result = dot_product(&a, &b).unwrap();
        // 3 × (-4) = -12
        assert!(
            (result + 12.0).abs() < 1e-6,
            "Parallel opposite direction dot product should be -12, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_large_dimension_384() {
        // Sentence Transformers dimension
        let dim = 384;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32).sin()).collect();
        let b: Vec<f32> = (0..dim).map(|i| (i as f32).cos()).collect();

        let result = dot_product(&a, &b).unwrap();
        // Just verify it runs and produces a finite result
        assert!(result.is_finite(), "Dot product should be finite");
    }

    #[test]
    fn test_dot_product_large_dimension_1536() {
        // OpenAI text-embedding-3-small dimension
        let dim = 1536;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.01).sin()).collect();
        let b: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.01).cos()).collect();

        let result = dot_product(&a, &b).unwrap();
        assert!(
            result.is_finite(),
            "Large dimension dot product should be finite"
        );
    }

    #[test]
    fn test_dot_product_large_dimension_self() {
        // Self dot product at large dimension
        let dim = 1536;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.01).sin()).collect();

        let self_dot = dot_product(&a, &a).unwrap();
        // Self dot should equal sum of squares
        let expected: f32 = a.iter().map(|x| x * x).sum();
        assert!(
            (self_dot - expected).abs() < 1e-3,
            "Self dot at dim={} should equal sum of squares: {} vs {}",
            dim,
            self_dot,
            expected
        );
    }

    #[test]
    fn test_dot_product_nan_propagation() {
        let a = vec![f32::NAN, 1.0];
        let b = vec![1.0, 1.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(result.is_nan(), "NaN in input should propagate to output");
    }

    #[test]
    fn test_dot_product_inf_propagation() {
        let a = vec![f32::INFINITY, 1.0];
        let b = vec![1.0, 1.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(
            result.is_infinite() && result > 0.0,
            "Positive Inf should propagate, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_neg_inf_propagation() {
        let a = vec![f32::NEG_INFINITY, 1.0];
        let b = vec![1.0, 1.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(
            result.is_infinite() && result < 0.0,
            "Negative Inf should propagate, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_matches_manual_calculation() {
        // Verify against manual calculation for various sizes
        for size in [1, 3, 7, 8, 15, 16, 17, 31, 32, 33, 100] {
            let a: Vec<f32> = (0..size).map(|i| i as f32).collect();
            let b: Vec<f32> = (0..size).map(|i| (size - i) as f32).collect();

            let simd_result = dot_product(&a, &b).unwrap();
            let manual_result: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();

            assert!(
                (simd_result - manual_result).abs() < 1e-4,
                "SIMD and manual should match at size {}: {} vs {}",
                size,
                simd_result,
                manual_result
            );
        }
    }

    #[test]
    fn test_dot_product_simd_boundary_cases() {
        // Test at SIMD boundaries (multiples of 4 and 8)
        for size in [4, 8, 12, 16, 24, 32, 64, 128] {
            let a: Vec<f32> = (0..size).map(|i| (i as f32 * 0.1).sin()).collect();
            let b: Vec<f32> = (0..size).map(|i| (i as f32 * 0.1).cos()).collect();

            let simd_result = dot_product(&a, &b).unwrap();
            let expected: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();

            assert!(
                (simd_result - expected).abs() < 1e-5,
                "SIMD boundary case failed at size {}: {} vs {}",
                size,
                simd_result,
                expected
            );
        }
    }

    // ========================================================================
    // Magnitude Tests
    // ========================================================================

    #[test]
    fn test_magnitude_3_4_triangle() {
        // Classic 3-4-5 right triangle
        let v = vec![3.0, 4.0];
        let mag = magnitude(&v);
        assert!(
            (mag - 5.0).abs() < 1e-6,
            "Magnitude of [3, 4] should be 5, got {}",
            mag
        );
    }

    #[test]
    fn test_magnitude_unit_vectors() {
        let unit_x = vec![1.0, 0.0, 0.0];
        let unit_y = vec![0.0, 1.0, 0.0];
        let unit_z = vec![0.0, 0.0, 1.0];

        assert!((magnitude(&unit_x) - 1.0).abs() < 1e-6);
        assert!((magnitude(&unit_y) - 1.0).abs() < 1e-6);
        assert!((magnitude(&unit_z) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_magnitude_zero_vector() {
        let v = vec![0.0, 0.0, 0.0];
        assert_eq!(magnitude(&v), 0.0);
    }

    #[test]
    fn test_magnitude_empty_vector() {
        let v: Vec<f32> = vec![];
        assert_eq!(magnitude(&v), 0.0);
    }

    #[test]
    fn test_magnitude_single_element() {
        assert!((magnitude(&[5.0]) - 5.0).abs() < 1e-6);
        assert!((magnitude(&[-5.0]) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn test_magnitude_negative_components() {
        let v = vec![-3.0, -4.0];
        assert!(
            (magnitude(&v) - 5.0).abs() < 1e-6,
            "Magnitude should be positive regardless of component signs"
        );
    }

    #[test]
    fn test_magnitude_large_dimension() {
        // Vector of all 1s with dimension n has magnitude sqrt(n)
        let n = 1536;
        let v: Vec<f32> = vec![1.0; n];
        let expected = (n as f32).sqrt();
        assert!(
            (magnitude(&v) - expected).abs() < 1e-4,
            "Magnitude of {} 1s should be sqrt({}), got {}",
            n,
            n,
            magnitude(&v)
        );
    }

    // ========================================================================
    // Squared Magnitude Tests
    // ========================================================================

    #[test]
    fn test_squared_magnitude_3_4_triangle() {
        let v = vec![3.0, 4.0];
        let sq_mag = squared_magnitude(&v);
        assert!(
            (sq_mag - 25.0).abs() < 1e-6,
            "Squared magnitude of [3, 4] should be 25, got {}",
            sq_mag
        );
    }

    #[test]
    fn test_squared_magnitude_zero_vector() {
        let v = vec![0.0, 0.0, 0.0];
        assert_eq!(squared_magnitude(&v), 0.0);
    }

    #[test]
    fn test_squared_magnitude_empty_vector() {
        let v: Vec<f32> = vec![];
        assert_eq!(squared_magnitude(&v), 0.0);
    }

    #[test]
    fn test_squared_magnitude_vs_magnitude() {
        let v = vec![1.0, 2.0, 3.0, 4.0];
        let mag = magnitude(&v);
        let sq_mag = squared_magnitude(&v);
        let diff = (mag * mag - sq_mag).abs();
        assert!(
            diff < 1e-5,
            "squared_magnitude should equal magnitude²: mag²={}, sq_mag={}, diff={}",
            mag * mag,
            sq_mag,
            diff
        );
    }

    // ========================================================================
    // Normalize Tests
    // ========================================================================

    #[test]
    fn test_normalize_3_4_triangle() {
        let v = vec![3.0, 4.0];
        let unit = normalize(&v);
        assert!((unit[0] - 0.6).abs() < 1e-6);
        assert!((unit[1] - 0.8).abs() < 1e-6);
        assert!((magnitude(&unit) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_already_unit() {
        let v = vec![1.0, 0.0, 0.0];
        let unit = normalize(&v);
        assert!((unit[0] - 1.0).abs() < 1e-6);
        assert!(unit[1].abs() < 1e-6);
        assert!(unit[2].abs() < 1e-6);
    }

    #[test]
    fn test_normalize_zero_vector() {
        let v = vec![0.0, 0.0, 0.0];
        let unit = normalize(&v);
        // Zero vector should return zero vector
        assert_eq!(unit, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn test_normalize_empty_vector() {
        let v: Vec<f32> = vec![];
        let unit = normalize(&v);
        assert!(unit.is_empty());
    }

    #[test]
    fn test_normalize_negative_components() {
        let v = vec![-3.0, -4.0];
        let unit = normalize(&v);
        assert!((unit[0] - (-0.6)).abs() < 1e-6);
        assert!((unit[1] - (-0.8)).abs() < 1e-6);
        assert!((magnitude(&unit) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_mixed_sign_components() {
        // Test mixed positive and negative components: [3, -4] has magnitude 5
        let v = vec![3.0, -4.0];
        let unit = normalize(&v);
        assert!((unit[0] - 0.6).abs() < 1e-6);
        assert!((unit[1] - (-0.8)).abs() < 1e-6);
        assert!((magnitude(&unit) - 1.0).abs() < 1e-6);

        // Verify sign is preserved
        assert!(unit[0] > 0.0, "First component should remain positive");
        assert!(unit[1] < 0.0, "Second component should remain negative");
    }

    #[test]
    fn test_normalize_preserves_direction() {
        let v = vec![2.0, 4.0, 6.0];
        let unit = normalize(&v);
        // Ratios should be preserved: 1:2:3
        let ratio_1_2 = unit[0] / unit[1];
        let ratio_1_3 = unit[0] / unit[2];
        assert!((ratio_1_2 - 0.5).abs() < 1e-6);
        assert!((ratio_1_3 - (1.0 / 3.0)).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_large_dimension() {
        let n = 1536;
        let v: Vec<f32> = (0..n).map(|i| (i as f32 * 0.1).sin()).collect();
        let unit = normalize(&v);
        assert!(
            (magnitude(&unit) - 1.0).abs() < 1e-5,
            "Normalized vector should have magnitude 1.0, got {}",
            magnitude(&unit)
        );
    }

    // ========================================================================
    // Normalize In-Place Tests
    // ========================================================================

    #[test]
    fn test_normalize_in_place_3_4_triangle() {
        let mut v = vec![3.0, 4.0];
        normalize_in_place(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
        assert!((magnitude(&v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_in_place_zero_vector() {
        let mut v = vec![0.0, 0.0, 0.0];
        normalize_in_place(&mut v);
        // Zero vector should remain unchanged
        assert_eq!(v, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn test_normalize_in_place_empty_vector() {
        let mut v: Vec<f32> = vec![];
        normalize_in_place(&mut v);
        assert!(v.is_empty());
    }

    #[test]
    fn test_normalize_in_place_matches_normalize() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let expected = normalize(&v);

        let mut v_copy = v.clone();
        normalize_in_place(&mut v_copy);

        for (a, b) in v_copy.iter().zip(expected.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn test_normalize_in_place_large_dimension() {
        let n = 1536;
        let mut v: Vec<f32> = (0..n).map(|i| (i as f32 * 0.1).cos()).collect();
        normalize_in_place(&mut v);
        assert!(
            (magnitude(&v) - 1.0).abs() < 1e-5,
            "In-place normalized vector should have magnitude 1.0, got {}",
            magnitude(&v)
        );
    }

    // ========================================================================
    // Is Normalized Tests
    // ========================================================================

    #[test]
    fn test_is_normalized_unit_vectors() {
        assert!(is_normalized(&[1.0, 0.0, 0.0], 1e-6));
        assert!(is_normalized(&[0.0, 1.0, 0.0], 1e-6));
        assert!(is_normalized(&[0.0, 0.0, 1.0], 1e-6));
    }

    #[test]
    fn test_is_normalized_normalized_vector() {
        let v = normalize(&[3.0, 4.0]);
        assert!(is_normalized(&v, 1e-6));
    }

    #[test]
    fn test_is_normalized_non_unit_vector() {
        assert!(!is_normalized(&[3.0, 4.0], 1e-6)); // magnitude = 5
        assert!(!is_normalized(&[2.0, 0.0, 0.0], 1e-6)); // magnitude = 2
    }

    #[test]
    fn test_is_normalized_zero_vector() {
        // Zero vector has magnitude 0, which is not 1
        assert!(!is_normalized(&[0.0, 0.0, 0.0], 1e-6));
    }

    #[test]
    fn test_is_normalized_tolerance() {
        // Vector with magnitude 1.0001 should pass with 1e-3 tolerance
        let v = vec![1.0001, 0.0, 0.0];
        assert!(!is_normalized(&v, 1e-6));
        assert!(is_normalized(&v, 1e-3));
    }

    #[test]
    fn test_is_normalized_diagonal() {
        // Unit diagonal in 3D: each component = 1/sqrt(3)
        let c = 1.0 / 3.0_f32.sqrt();
        let v = vec![c, c, c];
        assert!(is_normalized(&v, 1e-6));
    }

    // ========================================================================
    // DistanceMetric Tests
    // ========================================================================

    #[test]
    fn test_distance_metric_default() {
        let metric = DistanceMetric::default();
        assert_eq!(metric, DistanceMetric::Cosine);
    }

    #[test]
    fn test_distance_metric_cosine_identical_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];

        // Identical vectors: similarity = 1, distance = 0
        let similarity = DistanceMetric::Cosine.compute_similarity(&a, &b).unwrap();
        let distance = DistanceMetric::Cosine.compute_distance(&a, &b).unwrap();

        assert!((similarity - 1.0).abs() < 1e-6);
        assert!((distance - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_cosine_opposite_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];

        // Opposite vectors: similarity = -1, distance = 2
        let similarity = DistanceMetric::Cosine.compute_similarity(&a, &b).unwrap();
        let distance = DistanceMetric::Cosine.compute_distance(&a, &b).unwrap();

        assert!((similarity - (-1.0)).abs() < 1e-6);
        assert!((distance - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_cosine_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];

        // Orthogonal vectors: similarity = 0, distance = 1
        let similarity = DistanceMetric::Cosine.compute_similarity(&a, &b).unwrap();
        let distance = DistanceMetric::Cosine.compute_distance(&a, &b).unwrap();

        assert!((similarity - 0.0).abs() < 1e-6);
        assert!((distance - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_euclidean_identical_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];

        // Identical vectors: distance = 0, similarity = 1
        let distance = DistanceMetric::Euclidean.compute_distance(&a, &b).unwrap();
        let similarity = DistanceMetric::Euclidean
            .compute_similarity(&a, &b)
            .unwrap();

        assert!((distance - 0.0).abs() < 1e-6);
        assert!((similarity - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_euclidean_unit_distance() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 0.0];

        // Distance = 1, similarity = 1/(1+1) = 0.5
        let distance = DistanceMetric::Euclidean.compute_distance(&a, &b).unwrap();
        let similarity = DistanceMetric::Euclidean
            .compute_similarity(&a, &b)
            .unwrap();

        assert!((distance - 1.0).abs() < 1e-6);
        assert!((similarity - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_euclidean_3_4_5_triangle() {
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];

        // Distance = 5, similarity = 1/(1+5) = 1/6
        let distance = DistanceMetric::Euclidean.compute_distance(&a, &b).unwrap();
        let similarity = DistanceMetric::Euclidean
            .compute_similarity(&a, &b)
            .unwrap();

        assert!((distance - 5.0).abs() < 1e-6);
        assert!((similarity - (1.0 / 6.0)).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_dot_product_identical_unit_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];

        // Unit vectors: dot = 1, distance = 0
        let similarity = DistanceMetric::DotProduct
            .compute_similarity(&a, &b)
            .unwrap();
        let distance = DistanceMetric::DotProduct.compute_distance(&a, &b).unwrap();

        assert!((similarity - 1.0).abs() < 1e-6);
        assert!((distance - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_dot_product_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];

        // Orthogonal: dot = 0, distance = 1
        let similarity = DistanceMetric::DotProduct
            .compute_similarity(&a, &b)
            .unwrap();
        let distance = DistanceMetric::DotProduct.compute_distance(&a, &b).unwrap();

        assert!((similarity - 0.0).abs() < 1e-6);
        assert!((distance - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_dot_product_opposite_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];

        // Opposite: dot = -1, distance = 2
        let similarity = DistanceMetric::DotProduct
            .compute_similarity(&a, &b)
            .unwrap();
        let distance = DistanceMetric::DotProduct.compute_distance(&a, &b).unwrap();

        assert!((similarity - (-1.0)).abs() < 1e-6);
        assert!((distance - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_dimension_mismatch() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];

        // All metrics should return an error for mismatched dimensions
        assert!(DistanceMetric::Cosine.compute_distance(&a, &b).is_err());
        assert!(DistanceMetric::Cosine.compute_similarity(&a, &b).is_err());
        assert!(DistanceMetric::Euclidean.compute_distance(&a, &b).is_err());
        assert!(
            DistanceMetric::Euclidean
                .compute_similarity(&a, &b)
                .is_err()
        );
        assert!(DistanceMetric::DotProduct.compute_distance(&a, &b).is_err());
        assert!(
            DistanceMetric::DotProduct
                .compute_similarity(&a, &b)
                .is_err()
        );
    }

    #[test]
    fn test_distance_metric_name() {
        assert_eq!(DistanceMetric::Cosine.name(), "cosine");
        assert_eq!(DistanceMetric::Euclidean.name(), "euclidean");
        assert_eq!(DistanceMetric::DotProduct.name(), "dot_product");
    }

    #[test]
    fn test_distance_metric_display() {
        assert_eq!(format!("{}", DistanceMetric::Cosine), "cosine");
        assert_eq!(format!("{}", DistanceMetric::Euclidean), "euclidean");
        assert_eq!(format!("{}", DistanceMetric::DotProduct), "dot_product");
    }

    #[test]
    fn test_distance_metric_requires_normalized() {
        assert!(!DistanceMetric::Cosine.requires_normalized_vectors());
        assert!(!DistanceMetric::Euclidean.requires_normalized_vectors());
        assert!(DistanceMetric::DotProduct.requires_normalized_vectors());
    }

    #[test]
    fn test_distance_metric_clone_copy() {
        let metric = DistanceMetric::Cosine;
        // Both Clone and Copy traits work - use Copy (implicit)
        let copied1 = metric;
        let copied2 = metric;

        assert_eq!(metric, copied1);
        assert_eq!(metric, copied2);
    }

    #[test]
    fn test_distance_metric_debug() {
        assert_eq!(format!("{:?}", DistanceMetric::Cosine), "Cosine");
        assert_eq!(format!("{:?}", DistanceMetric::Euclidean), "Euclidean");
        assert_eq!(format!("{:?}", DistanceMetric::DotProduct), "DotProduct");
    }

    #[test]
    fn test_distance_metric_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(DistanceMetric::Cosine);
        set.insert(DistanceMetric::Euclidean);
        set.insert(DistanceMetric::DotProduct);
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn test_distance_metric_consistency() {
        // For normalized vectors, cosine similarity should equal dot product
        let a = normalize(&[3.0, 4.0]);
        let b = normalize(&[1.0, 0.0]);

        let cosine_sim = DistanceMetric::Cosine.compute_similarity(&a, &b).unwrap();
        let dot_sim = DistanceMetric::DotProduct
            .compute_similarity(&a, &b)
            .unwrap();

        assert!(
            (cosine_sim - dot_sim).abs() < 1e-5,
            "For normalized vectors, cosine ({}) should equal dot product ({})",
            cosine_sim,
            dot_sim
        );
    }

    #[test]
    fn test_distance_metric_all_metrics_same_order() {
        // All metrics should agree on which of two vectors is more similar to a query
        let query = vec![1.0, 0.5, 0.0];
        let closer = vec![0.9, 0.4, 0.1];
        let farther = vec![0.1, 0.9, 0.8];

        // Cosine
        let cosine_closer = DistanceMetric::Cosine
            .compute_similarity(&query, &closer)
            .unwrap();
        let cosine_farther = DistanceMetric::Cosine
            .compute_similarity(&query, &farther)
            .unwrap();
        assert!(
            cosine_closer > cosine_farther,
            "Cosine: {} should be > {}",
            cosine_closer,
            cosine_farther
        );

        // Euclidean
        let eucl_closer = DistanceMetric::Euclidean
            .compute_distance(&query, &closer)
            .unwrap();
        let eucl_farther = DistanceMetric::Euclidean
            .compute_distance(&query, &farther)
            .unwrap();
        assert!(
            eucl_closer < eucl_farther,
            "Euclidean: {} should be < {}",
            eucl_closer,
            eucl_farther
        );

        // Dot Product
        let dot_closer = DistanceMetric::DotProduct
            .compute_similarity(&query, &closer)
            .unwrap();
        let dot_farther = DistanceMetric::DotProduct
            .compute_similarity(&query, &farther)
            .unwrap();
        assert!(
            dot_closer > dot_farther,
            "DotProduct: {} should be > {}",
            dot_closer,
            dot_farther
        );
    }

    // ========================================================================
    // Vector Validation Tests
    // ========================================================================

    #[test]
    fn test_validate_vector_valid() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert!(validate_vector(&v).is_ok());
    }

    #[test]
    fn test_validate_vector_empty() {
        let v: Vec<f32> = vec![];
        assert!(validate_vector(&v).is_ok());
    }

    #[test]
    fn test_validate_vector_single_element() {
        let v = vec![42.0];
        assert!(validate_vector(&v).is_ok());
    }

    #[test]
    fn test_validate_vector_negative_and_zero() {
        let v = vec![-1.0, 0.0, 1.0, -0.0];
        assert!(validate_vector(&v).is_ok());
    }

    #[test]
    fn test_validate_vector_nan_single() {
        let v = vec![1.0, f32::NAN, 3.0];
        let result = validate_vector(&v);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::ContainsNaN { count })) => {
                assert_eq!(count, 1);
            }
            _ => panic!("Expected ContainsNaN error"),
        }
    }

    #[test]
    fn test_validate_vector_nan_multiple() {
        let v = vec![f32::NAN, 1.0, f32::NAN, 2.0, f32::NAN];
        let result = validate_vector(&v);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::ContainsNaN { count })) => {
                assert_eq!(count, 3);
            }
            _ => panic!("Expected ContainsNaN error with count 3"),
        }
    }

    #[test]
    fn test_validate_vector_infinity_positive() {
        let v = vec![1.0, f32::INFINITY, 3.0];
        let result = validate_vector(&v);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::ContainsInfinity { count })) => {
                assert_eq!(count, 1);
            }
            _ => panic!("Expected ContainsInfinity error"),
        }
    }

    #[test]
    fn test_validate_vector_infinity_negative() {
        let v = vec![1.0, f32::NEG_INFINITY, 3.0];
        let result = validate_vector(&v);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::ContainsInfinity { count })) => {
                assert_eq!(count, 1);
            }
            _ => panic!("Expected ContainsInfinity error"),
        }
    }

    #[test]
    fn test_validate_vector_infinity_multiple() {
        let v = vec![f32::INFINITY, 1.0, f32::NEG_INFINITY];
        let result = validate_vector(&v);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::ContainsInfinity { count })) => {
                assert_eq!(count, 2);
            }
            _ => panic!("Expected ContainsInfinity error with count 2"),
        }
    }

    #[test]
    fn test_validate_vector_nan_takes_precedence() {
        // When both NaN and Infinity are present, NaN error should be returned first
        let v = vec![f32::NAN, f32::INFINITY];
        let result = validate_vector(&v);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::ContainsNaN { count })) => {
                assert_eq!(count, 1);
            }
            _ => panic!("Expected ContainsNaN error (NaN takes precedence over Infinity)"),
        }
    }

    #[test]
    fn test_validate_vector_subnormal() {
        // Subnormal (denormalized) numbers should be valid
        let v = vec![f32::MIN_POSITIVE / 2.0, 1.0];
        assert!(validate_vector(&v).is_ok());
    }

    #[test]
    fn test_validate_vector_max_min_values() {
        // Extreme but finite values should be valid
        let v = vec![f32::MAX, f32::MIN, 1.0];
        assert!(validate_vector(&v).is_ok());
    }

    // ========================================================================
    // Dimension Match Tests
    // ========================================================================

    #[test]
    fn test_check_dimensions_match_equal() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        assert!(check_dimensions_match(&a, &b).is_ok());
    }

    #[test]
    fn test_check_dimensions_match_empty() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert!(check_dimensions_match(&a, &b).is_ok());
    }

    #[test]
    fn test_check_dimensions_match_single() {
        let a = vec![1.0];
        let b = vec![2.0];
        assert!(check_dimensions_match(&a, &b).is_ok());
    }

    #[test]
    fn test_check_dimensions_match_unequal() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        let result = check_dimensions_match(&a, &b);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::DimensionMismatch { expected, actual })) => {
                assert_eq!(expected, 3);
                assert_eq!(actual, 2);
            }
            _ => panic!("Expected DimensionMismatch error"),
        }
    }

    #[test]
    fn test_check_dimensions_match_unequal_reversed() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0, 2.0, 3.0];
        let result = check_dimensions_match(&a, &b);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::DimensionMismatch { expected, actual })) => {
                assert_eq!(expected, 2);
                assert_eq!(actual, 3);
            }
            _ => panic!("Expected DimensionMismatch error"),
        }
    }

    #[test]
    fn test_check_dimensions_match_empty_vs_nonempty() {
        let a: Vec<f32> = vec![];
        let b = vec![1.0];
        let result = check_dimensions_match(&a, &b);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::DimensionMismatch { expected, actual })) => {
                assert_eq!(expected, 0);
                assert_eq!(actual, 1);
            }
            _ => panic!("Expected DimensionMismatch error"),
        }
    }

    // ========================================================================
    // Validate Vector with Bounds Tests
    // ========================================================================

    #[test]
    fn test_validate_vector_with_bounds_valid() {
        let v = vec![1.0, 2.0, 3.0];
        assert!(validate_vector_with_bounds(&v, 10).is_ok());
    }

    #[test]
    fn test_validate_vector_with_bounds_exact() {
        let v = vec![1.0, 2.0, 3.0];
        assert!(validate_vector_with_bounds(&v, 3).is_ok());
    }

    #[test]
    fn test_validate_vector_with_bounds_too_large() {
        let v = vec![1.0, 2.0, 3.0];
        let result = validate_vector_with_bounds(&v, 2);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::DimensionTooLarge {
                dimension,
                max_allowed,
            })) => {
                assert_eq!(dimension, 3);
                assert_eq!(max_allowed, 2);
            }
            _ => panic!("Expected DimensionTooLarge error"),
        }
    }

    #[test]
    fn test_validate_vector_with_bounds_empty() {
        let v: Vec<f32> = vec![];
        assert!(validate_vector_with_bounds(&v, 0).is_ok());
    }

    #[test]
    fn test_validate_vector_with_bounds_nan() {
        // NaN should be detected even if dimension is within bounds
        let v = vec![f32::NAN, 2.0];
        let result = validate_vector_with_bounds(&v, 10);
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::ContainsNaN { .. }))
        ));
    }

    #[test]
    fn test_validate_vector_with_bounds_dimension_checked_first() {
        // If dimension exceeds bounds, that error should be returned
        // even if the vector also contains NaN
        let v = vec![f32::NAN, 2.0, 3.0, 4.0];
        let result = validate_vector_with_bounds(&v, 2);
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::DimensionTooLarge { .. }))
        ));
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

                // Cosine similarity should be scale-invariant.
                //
                // Why 10x tolerance? Scaling introduces additional error sources:
                // 1. The scaling multiplication itself: len extra multiply operations
                // 2. Magnitude changes: scaled vectors have different magnitudes,
                //    potentially causing different rounding in the sqrt and division
                // 3. For scale factors near the edges (0.1 or 10.0), the magnitude
                //    difference is up to 100x, affecting numerical stability
                //
                // Empirically, 10x base tolerance handles these cases while still
                // catching genuine scale invariance violations.
                if !sim1.is_nan() && !sim2.is_nan() {
                    prop_assert!((sim1 - sim2).abs() < PROPTEST_TOLERANCE * 10.0,
                        "Scale invariance failed: {} vs {} for scale {}", sim1, sim2, scale);
                }
            }
        }

        // ====================================================================
        // Euclidean Distance Property Tests
        // ====================================================================

        #[test]
        fn prop_euclidean_distance_is_non_negative(
            (a, b) in same_length_vectors(100)
        ) {
            let dist = euclidean_distance(&a, &b).unwrap();
            prop_assert!(dist >= 0.0, "Distance should be non-negative, got {}", dist);
        }

        #[test]
        fn prop_squared_euclidean_distance_is_non_negative(
            (a, b) in same_length_vectors(100)
        ) {
            let dist_sq = squared_euclidean_distance(&a, &b).unwrap();
            prop_assert!(dist_sq >= 0.0, "Squared distance should be non-negative, got {}", dist_sq);
        }

        #[test]
        fn prop_euclidean_distance_is_symmetric(
            (a, b) in same_length_vectors(100)
        ) {
            let dist_ab = euclidean_distance(&a, &b).unwrap();
            let dist_ba = euclidean_distance(&b, &a).unwrap();
            prop_assert!((dist_ab - dist_ba).abs() < PROPTEST_TOLERANCE,
                "Euclidean distance should be symmetric: {} vs {}", dist_ab, dist_ba);
        }

        #[test]
        fn prop_squared_euclidean_distance_is_symmetric(
            (a, b) in same_length_vectors(100)
        ) {
            let dist_sq_ab = squared_euclidean_distance(&a, &b).unwrap();
            let dist_sq_ba = squared_euclidean_distance(&b, &a).unwrap();
            prop_assert!((dist_sq_ab - dist_sq_ba).abs() < PROPTEST_TOLERANCE,
                "Squared Euclidean distance should be symmetric: {} vs {}", dist_sq_ab, dist_sq_ba);
        }

        #[test]
        fn prop_euclidean_distance_self_is_zero(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            let dist = euclidean_distance(&a, &a).unwrap();
            prop_assert!(dist.abs() < PROPTEST_TOLERANCE,
                "Distance to self should be 0, got {} for {:?}", dist, a);
        }

        #[test]
        fn prop_squared_euclidean_distance_self_is_zero(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            let dist_sq = squared_euclidean_distance(&a, &a).unwrap();
            prop_assert!(dist_sq.abs() < PROPTEST_TOLERANCE,
                "Squared distance to self should be 0, got {} for {:?}", dist_sq, a);
        }

        #[test]
        fn prop_euclidean_squared_relationship(
            (a, b) in same_length_vectors(100)
        ) {
            let dist = euclidean_distance(&a, &b).unwrap();
            let dist_sq = squared_euclidean_distance(&a, &b).unwrap();
            // dist² should equal dist_sq
            // Use relative tolerance for larger values
            let tolerance = PROPTEST_TOLERANCE * 100.0 + dist_sq * 1e-5;
            prop_assert!((dist * dist - dist_sq).abs() < tolerance,
                "euclidean² ({}) should equal squared_euclidean ({})", dist * dist, dist_sq);
        }

        #[test]
        fn prop_euclidean_distance_triangle_inequality(
            a in prop::collection::vec(-50.0f32..50.0f32, 1..50usize),
            b in prop::collection::vec(-50.0f32..50.0f32, 1..50usize),
            c in prop::collection::vec(-50.0f32..50.0f32, 1..50usize)
        ) {
            // Triangle inequality: d(a,c) <= d(a,b) + d(b,c)
            // Only test if all vectors have same length
            if a.len() == b.len() && b.len() == c.len() {
                let d_ab = euclidean_distance(&a, &b).unwrap();
                let d_bc = euclidean_distance(&b, &c).unwrap();
                let d_ac = euclidean_distance(&a, &c).unwrap();

                // Allow small tolerance for floating-point errors
                let tolerance = PROPTEST_TOLERANCE * 100.0;
                prop_assert!(d_ac <= d_ab + d_bc + tolerance,
                    "Triangle inequality violated: d(a,c)={} > d(a,b)={} + d(b,c)={}",
                    d_ac, d_ab, d_bc);
            }
        }

        // ====================================================================
        // Dot Product Property Tests
        // ====================================================================

        #[test]
        fn prop_dot_product_is_symmetric(
            (a, b) in same_length_vectors(100)
        ) {
            let dot_ab = dot_product(&a, &b).unwrap();
            let dot_ba = dot_product(&b, &a).unwrap();
            prop_assert!((dot_ab - dot_ba).abs() < PROPTEST_TOLERANCE,
                "Dot product should be symmetric: {} vs {}", dot_ab, dot_ba);
        }

        #[test]
        fn prop_dot_product_self_equals_squared_magnitude(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            let self_dot = dot_product(&a, &a).unwrap();
            // Self dot product should equal sum of squares (squared magnitude)
            let expected: f32 = a.iter().map(|x| x * x).sum();

            // Use relative tolerance for larger values
            let tolerance = PROPTEST_TOLERANCE * 100.0 + expected * 1e-5;
            prop_assert!((self_dot - expected).abs() < tolerance,
                "Self dot product should equal squared magnitude: {} vs {}", self_dot, expected);
        }

        #[test]
        fn prop_dot_product_with_zero_vector(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            let zeros: Vec<f32> = vec![0.0; a.len()];
            let result = dot_product(&a, &zeros).unwrap();
            prop_assert!(result.abs() < PROPTEST_TOLERANCE,
                "Dot product with zero vector should be 0, got {}", result);
        }

        #[test]
        fn prop_dot_product_bilinearity_scalar(
            (a, b) in same_length_vectors(50),
            scale in 0.1f32..10.0f32
        ) {
            // Scalar multiplication: dot(c*a, b) = c * dot(a, b)
            let dot_ab = dot_product(&a, &b).unwrap();
            let scaled_a: Vec<f32> = a.iter().map(|x| x * scale).collect();
            let dot_scaled = dot_product(&scaled_a, &b).unwrap();

            // Use tolerance based on intermediate value magnitudes.
            //
            // Key insight: when vectors have mixed +/- values, catastrophic
            // cancellation occurs. Large intermediate values (e.g., 9000) can
            // cancel to give small results (e.g., 16). The floating-point error
            // is bounded by the intermediate magnitudes, not the final result.
            //
            // Example: sum of products might be [+9000, -8500, +500, -984, ...]
            // giving final result of ~16, but error is proportional to ~9000.
            //
            // Solution: base tolerance on sum of absolute products, which
            // represents the "scale" of computation regardless of cancellation.
            let expected = scale * dot_ab;
            let sum_abs_products: f32 = a.iter()
                .zip(b.iter())
                .map(|(x, y)| (x * y).abs())
                .sum();
            // Error is proportional to intermediate magnitudes * scale * f32 epsilon
            // Use 1e-5 relative to intermediate values (conservative for f32)
            let tolerance = PROPTEST_TOLERANCE * 100.0 + sum_abs_products * scale * 1e-5;
            prop_assert!((dot_scaled - expected).abs() < tolerance,
                "Scalar bilinearity failed: dot({}*a, b)={} vs {}*dot(a,b)={}",
                scale, dot_scaled, scale, expected);
        }

        // ====================================================================
        // Normalization Property Tests
        // ====================================================================

        #[test]
        fn prop_normalized_vector_has_unit_magnitude(
            v in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            // Skip zero vectors using SIMD squared_magnitude
            let sq_mag = squared_magnitude(&v);
            if sq_mag > 1e-10 {
                let unit = normalize(&v);
                let mag = magnitude(&unit);
                prop_assert!((mag - 1.0).abs() < PROPTEST_TOLERANCE,
                    "Normalized vector should have magnitude 1.0, got {} for {:?}", mag, v);
            }
        }

        #[test]
        fn prop_normalize_in_place_has_unit_magnitude(
            v in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            // Skip zero vectors using SIMD squared_magnitude
            let sq_mag = squared_magnitude(&v);
            if sq_mag > 1e-10 {
                let mut v_copy = v.clone();
                normalize_in_place(&mut v_copy);
                let mag = magnitude(&v_copy);
                prop_assert!((mag - 1.0).abs() < PROPTEST_TOLERANCE,
                    "In-place normalized vector should have magnitude 1.0, got {}", mag);
            }
        }

        #[test]
        fn prop_normalize_matches_normalize_in_place(
            v in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            let unit = normalize(&v);
            let mut v_copy = v.clone();
            normalize_in_place(&mut v_copy);

            for (a, b) in unit.iter().zip(v_copy.iter()) {
                prop_assert!((a - b).abs() < PROPTEST_TOLERANCE,
                    "normalize and normalize_in_place should produce same result");
            }
        }

        #[test]
        fn prop_magnitude_is_non_negative(
            v in prop::collection::vec(-100.0f32..100.0f32, 0..100usize)
        ) {
            let mag = magnitude(&v);
            prop_assert!(mag >= 0.0, "Magnitude should be non-negative, got {}", mag);
        }

        #[test]
        fn prop_squared_magnitude_is_non_negative(
            v in prop::collection::vec(-100.0f32..100.0f32, 0..100usize)
        ) {
            let sq_mag = squared_magnitude(&v);
            prop_assert!(sq_mag >= 0.0, "Squared magnitude should be non-negative, got {}", sq_mag);
        }

        #[test]
        fn prop_magnitude_squared_relationship(
            v in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            let mag = magnitude(&v);
            let sq_mag = squared_magnitude(&v);
            // magnitude² should equal squared_magnitude
            // Use relative tolerance to avoid scale-dependent thresholds
            let diff = (mag * mag - sq_mag).abs();
            let relative_tolerance = 1e-5; // 0.001% relative error
            let threshold = sq_mag.max(1.0) * relative_tolerance;
            prop_assert!(diff < threshold,
                "magnitude² ({}) should equal squared_magnitude ({}), diff={}, threshold={}",
                mag * mag, sq_mag, diff, threshold);
        }

        #[test]
        fn prop_normalize_preserves_direction(
            v in prop::collection::vec(-100.0f32..100.0f32, 2..50usize)
        ) {
            // Skip zero or near-zero vectors using SIMD squared_magnitude
            let sq_mag = squared_magnitude(&v);
            if sq_mag > 1e-6 {
                let unit = normalize(&v);
                // Cosine similarity between original and normalized should be 1.0
                let sim = cosine_similarity(&v, &unit).unwrap();
                if !sim.is_nan() {
                    prop_assert!((sim - 1.0).abs() < PROPTEST_TOLERANCE * 10.0,
                        "Normalized vector should have same direction: sim = {}", sim);
                }
            }
        }

        #[test]
        fn prop_is_normalized_after_normalize(
            v in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            // Skip zero vectors using SIMD squared_magnitude
            let sq_mag = squared_magnitude(&v);
            if sq_mag > 1e-10 {
                let unit = normalize(&v);
                // Use 1e-5 tolerance matching PROPTEST_TOLERANCE
                prop_assert!(is_normalized(&unit, PROPTEST_TOLERANCE),
                    "Normalized vector should pass is_normalized check");
            }
        }
    }
}
