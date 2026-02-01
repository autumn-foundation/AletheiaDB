use crate::utils::error::Result;
use super::ops::dot_product;
use super::simd::{dot_and_magnitudes, dot_product_sum, squared_diff_sum};
use super::validation::check_dimensions_match;
use std::fmt;

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
/// 1. Pre-normalize vectors with [`normalize`](super::ops::normalize) or [`normalize_in_place`](super::ops::normalize_in_place)
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
