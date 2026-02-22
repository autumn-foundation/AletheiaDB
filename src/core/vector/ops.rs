use super::constants::SQUARED_MAGNITUDE_THRESHOLD;
use super::simd::{
    dot_and_magnitudes, dot_product_sum, scale_and_copy, scale_in_place, squared_diff_sum,
};
use super::validation::check_dimensions_match;
use crate::core::error::Result;

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
/// use aletheiadb::core::vector::cosine_similarity;
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
    // by at most machine epsilon (~1e-7 for f32). However, with subnormal numbers
    // or extreme scales, precision loss can be larger.
    // We use a looser threshold (1e-2) to accomodate these edge cases found by fuzzing.
    debug_assert!(
        result.is_nan() || result.abs() <= 1.0 + 1e-2,
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
/// use aletheiadb::core::vector::cosine_similarity_normalized;
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
/// use aletheiadb::core::vector::squared_euclidean_distance;
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
/// use aletheiadb::core::vector::euclidean_distance;
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
/// use aletheiadb::core::vector::dot_product;
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
/// use aletheiadb::core::vector::magnitude;
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
/// use aletheiadb::core::vector::squared_magnitude;
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
/// use aletheiadb::core::vector::{normalize, magnitude};
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
#[allow(clippy::uninit_vec)] // Performance optimization: we explicitly fill the vector
pub fn normalize(v: &[f32]) -> Vec<f32> {
    let sq_mag = squared_magnitude(v);
    // Use squared magnitude threshold to avoid denormal number issues.
    // See SQUARED_MAGNITUDE_THRESHOLD for details.
    if sq_mag < SQUARED_MAGNITUDE_THRESHOLD {
        // Return zero vector of same length
        return vec![0.0; v.len()];
    }
    // Scale and copy in one pass using SIMD
    // Compute 1/sqrt(sq_mag) directly to avoid intermediate variable
    let inv_mag = 1.0 / sq_mag.sqrt();

    // Allocate uninitialized vector to avoid zero-filling overhead.
    // This provides ~15% speedup for large vectors by avoiding an extra write pass.
    let mut result = Vec::with_capacity(v.len());

    // Use spare_capacity_mut to get uninitialized memory safely without creating a
    // reference to uninitialized data (which would be UB).
    let dst = result.spare_capacity_mut();
    // We must limit the slice to the number of elements we're going to write
    // In case capacity > v.len() (unlikely with with_capacity but possible)
    let dst = &mut dst[..v.len()];

    scale_and_copy(v, dst, inv_mag);

    // SAFETY: scale_and_copy guarantees it has written to all elements of dst.
    // We passed a slice of length v.len(), so v.len() elements are now initialized.
    unsafe { result.set_len(v.len()) };

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
/// use aletheiadb::core::vector::{normalize_in_place, magnitude};
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
/// use aletheiadb::core::vector::{is_normalized, normalize};
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
/// use aletheiadb::core::vector::{is_normalized_default, normalize};
///
/// let v = vec![3.0, 4.0];
/// assert!(!is_normalized_default(&v));
///
/// let unit = normalize(&v);
/// assert!(is_normalized_default(&unit));
/// ```
#[inline]
pub fn is_normalized_default(v: &[f32]) -> bool {
    // We can't import NORMALIZATION_TOLERANCE from constants because of visibility/import loops?
    // Actually constants.rs is a sibling.
    is_normalized(v, super::constants::NORMALIZATION_TOLERANCE)
}
