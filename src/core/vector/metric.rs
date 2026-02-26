use super::ops::{cosine_similarity, dot_product, euclidean_distance};
use crate::core::error::Result;
use std::fmt;

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
/// | [`Haversine`](DistanceMetric::Haversine) | Geographic coordinates | [0, ∞) distance | Great circle distance |
/// | [`Hamming`](DistanceMetric::Hamming) | Binary vectors | [0, ∞) distance | Bit-level difference |
/// | [`Tanimoto`](DistanceMetric::Tanimoto) | Chemical fingerprints | [0, 1] similarity | Bit-level Jaccard similarity |
///
/// # Example
///
/// ```rust
/// use aletheiadb::core::vector::DistanceMetric;
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

    /// Haversine distance.
    ///
    /// Great circle distance for geographic coordinates.
    ///
    /// - **Distance**: Range [0, ∞)
    /// - **Similarity**: Computed as `1 / (1 + distance)`
    Haversine,

    /// Hamming distance.
    ///
    /// Bit-level distance for binary vectors.
    ///
    /// - **Distance**: Range [0, ∞)
    /// - **Similarity**: Computed as `1 / (1 + distance)`
    Hamming,

    /// Tanimoto similarity (Jaccard for bitsets).
    ///
    /// Bit-level similarity for chemical fingerprints or binary vectors.
    ///
    /// - **Similarity**: Range [0, 1]
    /// - **Distance**: Computed as `1 - similarity`
    Tanimoto,
}

impl DistanceMetric {
    /// Encode distance metric as a byte for serialization.
    ///
    /// Encoding:
    /// - 0 = Cosine
    /// - 1 = Euclidean
    /// - 2 = DotProduct
    /// - 3 = Haversine
    /// - 4 = Hamming
    /// - 5 = Tanimoto
    ///
    /// # Example
    ///
    /// ```
    /// use aletheiadb::core::vector::DistanceMetric;
    ///
    /// assert_eq!(DistanceMetric::Cosine.to_u8(), 0);
    /// assert_eq!(DistanceMetric::Euclidean.to_u8(), 1);
    /// assert_eq!(DistanceMetric::DotProduct.to_u8(), 2);
    /// assert_eq!(DistanceMetric::Haversine.to_u8(), 3);
    /// assert_eq!(DistanceMetric::Hamming.to_u8(), 4);
    /// assert_eq!(DistanceMetric::Tanimoto.to_u8(), 5);
    /// ```
    pub fn to_u8(self) -> u8 {
        match self {
            DistanceMetric::Cosine => 0,
            DistanceMetric::Euclidean => 1,
            DistanceMetric::DotProduct => 2,
            DistanceMetric::Haversine => 3,
            DistanceMetric::Hamming => 4,
            DistanceMetric::Tanimoto => 5,
        }
    }

    /// Decode distance metric from a byte.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte value is not a valid metric encoding (>= 6).
    ///
    /// # Example
    ///
    /// ```
    /// use aletheiadb::core::vector::DistanceMetric;
    ///
    /// assert_eq!(DistanceMetric::from_u8(0).unwrap(), DistanceMetric::Cosine);
    /// assert_eq!(DistanceMetric::from_u8(1).unwrap(), DistanceMetric::Euclidean);
    /// assert_eq!(DistanceMetric::from_u8(2).unwrap(), DistanceMetric::DotProduct);
    /// assert_eq!(DistanceMetric::from_u8(3).unwrap(), DistanceMetric::Haversine);
    /// assert_eq!(DistanceMetric::from_u8(4).unwrap(), DistanceMetric::Hamming);
    /// assert_eq!(DistanceMetric::from_u8(5).unwrap(), DistanceMetric::Tanimoto);
    /// assert!(DistanceMetric::from_u8(6).is_err());
    /// ```
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            0 => Ok(DistanceMetric::Cosine),
            1 => Ok(DistanceMetric::Euclidean),
            2 => Ok(DistanceMetric::DotProduct),
            3 => Ok(DistanceMetric::Haversine),
            4 => Ok(DistanceMetric::Hamming),
            5 => Ok(DistanceMetric::Tanimoto),
            _ => Err(crate::core::error::StorageError::CorruptedData(format!(
                "Invalid distance metric encoding: {}",
                value
            ))
            .into()),
        }
    }

    /// Computes the distance between two vectors using this metric.
    ///
    /// Lower values indicate more similar vectors.
    ///
    /// # Returns
    ///
    /// - [`Cosine`](DistanceMetric::Cosine): `1 - cosine_similarity`, range [0, 2]
    /// - [`Euclidean`](DistanceMetric::Euclidean): L2 distance, range [0, ∞)
    /// - [`DotProduct`](DistanceMetric::DotProduct): `1 - dot_product` (meaningful only for normalized vectors)
    /// - [`Haversine`](DistanceMetric::Haversine): Unimplemented (returns error)
    /// - [`Hamming`](DistanceMetric::Hamming): Unimplemented (returns error)
    /// - [`Tanimoto`](DistanceMetric::Tanimoto): Unimplemented (returns error)
    ///
    /// # Errors
    ///
    /// Returns an error if the vectors have different lengths.
    /// Returns an error if the metric is not implemented for dense float vectors.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::DistanceMetric;
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
            DistanceMetric::Haversine => {
                // Placeholder for Haversine implementation
                // Real implementation requires lat/lon pairs
                Err(crate::core::error::Error::NotImplemented {
                    feature: "Haversine distance for dense vectors".to_string(),
                    reason: "Not yet implemented in core ops".to_string(),
                })
            }
            DistanceMetric::Hamming => {
                // Hamming usually requires binary inputs
                Err(crate::core::error::Error::NotImplemented {
                    feature: "Hamming distance for float vectors".to_string(),
                    reason: "Requires binary quantization".to_string(),
                })
            }
            DistanceMetric::Tanimoto => {
                // Tanimoto usually requires binary inputs
                Err(crate::core::error::Error::NotImplemented {
                    feature: "Tanimoto distance for float vectors".to_string(),
                    reason: "Requires binary quantization".to_string(),
                })
            }
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
    /// - [`Haversine`](DistanceMetric::Haversine): Unimplemented (returns error)
    /// - [`Hamming`](DistanceMetric::Hamming): Unimplemented (returns error)
    /// - [`Tanimoto`](DistanceMetric::Tanimoto): Unimplemented (returns error)
    ///
    /// # Errors
    ///
    /// Returns an error if the vectors have different lengths.
    /// Returns an error if the metric is not implemented for dense float vectors.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::DistanceMetric;
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
            DistanceMetric::Haversine => {
                Err(crate::core::error::Error::NotImplemented {
                    feature: "Haversine similarity".to_string(),
                    reason: "Not yet implemented in core ops".to_string(),
                })
            }
            DistanceMetric::Hamming => {
                Err(crate::core::error::Error::NotImplemented {
                    feature: "Hamming similarity".to_string(),
                    reason: "Requires binary quantization".to_string(),
                })
            }
            DistanceMetric::Tanimoto => {
                Err(crate::core::error::Error::NotImplemented {
                    feature: "Tanimoto similarity".to_string(),
                    reason: "Requires binary quantization".to_string(),
                })
            }
        }
    }

    /// Returns a human-readable name for this metric.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::DistanceMetric;
    ///
    /// assert_eq!(DistanceMetric::Cosine.name(), "cosine");
    /// assert_eq!(DistanceMetric::Euclidean.name(), "euclidean");
    /// assert_eq!(DistanceMetric::DotProduct.name(), "dot_product");
    /// assert_eq!(DistanceMetric::Haversine.name(), "haversine");
    /// assert_eq!(DistanceMetric::Hamming.name(), "hamming");
    /// assert_eq!(DistanceMetric::Tanimoto.name(), "tanimoto");
    /// ```
    #[inline]
    pub const fn name(&self) -> &'static str {
        match self {
            DistanceMetric::Cosine => "cosine",
            DistanceMetric::Euclidean => "euclidean",
            DistanceMetric::DotProduct => "dot_product",
            DistanceMetric::Haversine => "haversine",
            DistanceMetric::Hamming => "hamming",
            DistanceMetric::Tanimoto => "tanimoto",
        }
    }

    /// Returns whether this metric requires normalized vectors for optimal results.
    ///
    /// - [`Cosine`](DistanceMetric::Cosine): No (handles normalization internally)
    /// - [`Euclidean`](DistanceMetric::Euclidean): No (works with any vectors)
    /// - [`DotProduct`](DistanceMetric::DotProduct): Yes (otherwise not a proper similarity)
    /// - [`Haversine`](DistanceMetric::Haversine): No
    /// - [`Hamming`](DistanceMetric::Hamming): No
    /// - [`Tanimoto`](DistanceMetric::Tanimoto): No
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::DistanceMetric;
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
