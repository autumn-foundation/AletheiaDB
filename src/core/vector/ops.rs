use crate::utils::error::Result;
use super::constants::{NORMALIZATION_TOLERANCE, SQUARED_MAGNITUDE_THRESHOLD};
use super::validation::check_dimensions_match;
use super::simd::{dot_product_sum, scale_in_place};

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
/// [`cosine_similarity`](super::metric::cosine_similarity) when you only need the dot product and not the
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
/// Unlike two-vector functions like [`cosine_similarity`](super::metric::cosine_similarity), normalization functions
/// do not validate against `MAX_VECTOR_DIMENSIONS`. This is intentional because:
/// - Single-vector operations don't have dimension mismatch issues
/// - Dimension limits are enforced at storage time (see `PropertyValue::vector`)
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
/// using optimized functions like [`cosine_similarity_normalized`](super::metric::cosine_similarity_normalized).
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
