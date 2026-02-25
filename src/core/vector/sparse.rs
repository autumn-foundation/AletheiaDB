use super::constants::SQUARED_MAGNITUDE_THRESHOLD;
use crate::core::error::{Error, Result, VectorError};
use crate::core::property::MAX_VECTOR_DIMENSIONS;

// ============================================================================
// Sparse Vector Type
// ============================================================================

/// A sparse vector representation optimized for vectors with many zero values.
///
/// Sparse vectors store only non-zero values along with their indices, making them
/// memory-efficient for high-dimensional vectors where most values are zero.
/// This is particularly useful for algorithms like BM25 and SPLADE.
///
/// # Format
///
/// - `indices`: Sorted array of non-zero element positions
/// - `values`: Corresponding non-zero values
/// - `dimension`: Total vector dimension (including zeros)
///
/// # Invariants
///
/// The struct maintains these invariants:
/// 1. `indices.len() == values.len()` (each index has a corresponding value)
/// 2. `indices` are sorted in ascending order
/// 3. All indices are `< dimension`
/// 4. No duplicate indices
/// 5. All values are non-zero (no stored zeros)
///
/// # Example
///
/// ```rust
/// use aletheiadb::core::vector::SparseVec;
///
/// // Sparse vector: [0.0, 1.5, 0.0, 0.0, 2.3, 0.0, 0.0, 0.8]
/// let sparse = SparseVec::new(
///     vec![1, 4, 7],        // indices of non-zero values
///     vec![1.5, 2.3, 0.8],  // corresponding values
///     8                      // total dimension
/// ).unwrap();
///
/// assert_eq!(sparse.nnz(), 3);  // 3 non-zero elements
/// assert_eq!(sparse.dimension(), 8);
/// ```
///
/// # Use Cases
///
/// - **BM25**: Text retrieval using term frequency vectors
/// - **SPLADE**: Sparse learned embeddings for information retrieval
/// - **One-hot encodings**: Categorical features with single non-zero value
/// - **TF-IDF**: Document vectors with few non-zero terms
///
/// # Performance
///
/// - Space complexity: O(nnz) where nnz = number of non-zero elements
/// - Dense equivalent would be: O(dimension)
/// - Memory savings can be 10-1000x for sparse data
///
/// # Equality and Comparison
///
/// While this type implements `PartialEq` for compatibility with `PropertyValue`,
/// direct equality comparison using `==` is **not recommended** for floating-point
/// values. NaN != NaN (IEEE 754) and floating-point precision issues can cause
/// semantically equal vectors to compare unequal. For robust equality checks,
/// use [`approx_eq`](Self::approx_eq) with an appropriate epsilon value instead.
#[derive(Debug, Clone, PartialEq)]
pub struct SparseVec {
    /// Indices of non-zero elements (sorted, unique, all < dimension).
    indices: Vec<u32>,
    /// Non-zero values corresponding to indices.
    values: Vec<f32>,
    /// Total dimension of the vector (including zeros).
    dimension: u32,
}

impl SparseVec {
    /// Creates a new sparse vector.
    ///
    /// # Arguments
    ///
    /// * `indices` - Positions of non-zero elements (will be sorted if not already)
    /// * `values` - Non-zero values corresponding to indices
    /// * `dimension` - Total vector dimension (must be > max(indices))
    ///
    /// # Returns
    ///
    /// - `Ok(SparseVec)` if the input is valid
    /// - `Err` if validation fails (see Errors section)
    ///
    /// # Errors
    ///
    /// Returns `VectorError::DimensionMismatch` if:
    /// - `indices.len() != values.len()`
    /// - Any index >= dimension
    ///
    /// Returns `VectorError::InvalidSparseVector` if:
    /// - Duplicate indices found
    /// - Zero values found (sparse vectors should only store non-zero values)
    ///
    /// Returns `VectorError::DimensionTooLarge` if dimension exceeds MAX_VECTOR_DIMENSIONS.
    ///
    /// Returns `VectorError::ContainsNaN` if any value is NaN.
    ///
    /// Returns `VectorError::ContainsInfinity` if any value is infinite.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::SparseVec;
    ///
    /// // Valid sparse vector
    /// let sparse = SparseVec::new(vec![0, 2, 5], vec![1.0, 2.0, 3.0], 10).unwrap();
    /// assert_eq!(sparse.nnz(), 3);
    ///
    /// // Error: mismatched lengths
    /// let result = SparseVec::new(vec![0, 1], vec![1.0], 10);
    /// assert!(result.is_err());
    ///
    /// // Error: index out of bounds
    /// let result = SparseVec::new(vec![0, 10], vec![1.0, 2.0], 10);
    /// assert!(result.is_err());
    /// ```
    pub fn new(mut indices: Vec<u32>, mut values: Vec<f32>, dimension: u32) -> Result<Self> {
        // Validate dimension
        if dimension as usize > MAX_VECTOR_DIMENSIONS {
            return Err(VectorError::DimensionTooLarge {
                dimension: dimension as usize,
                max_allowed: MAX_VECTOR_DIMENSIONS,
            }
            .into());
        }

        // Validate lengths match
        if indices.len() != values.len() {
            return Err(VectorError::DimensionMismatch {
                expected: indices.len(),
                actual: values.len(),
            }
            .into());
        }

        // Validate and normalize data
        if !indices.is_empty() {
            // Sort by indices (if not already sorted)
            let mut index_value_pairs: Vec<(u32, f32)> = indices.into_iter().zip(values).collect();
            index_value_pairs.sort_by_key(|(idx, _)| *idx);

            // Check for duplicates and out-of-bounds indices
            let mut prev_idx = None;
            for (idx, val) in &index_value_pairs {
                // Check NaN
                if val.is_nan() {
                    return Err(VectorError::ContainsNaN { count: 1 }.into());
                }
                // Check Infinity
                if val.is_infinite() {
                    return Err(VectorError::ContainsInfinity { count: 1 }.into());
                }
                // Check for zero values (sparse vectors should not store zeros)
                if *val == 0.0 {
                    return Err(Error::Vector(VectorError::InvalidSparseVector {
                        reason: "Sparse vector contains zero value".to_string(),
                    }));
                }
                // Check index bounds
                if *idx >= dimension {
                    return Err(Error::Vector(VectorError::InvalidSparseVector {
                        reason: format!(
                            "Index {} is out of bounds for dimension {}",
                            idx, dimension
                        ),
                    }));
                }
                // Check for duplicates
                if let Some(prev) = prev_idx
                    && prev == *idx
                {
                    return Err(Error::Vector(VectorError::InvalidSparseVector {
                        reason: format!("Duplicate index {} found", idx),
                    }));
                }
                prev_idx = Some(*idx);
            }

            // Unzip back to separate vectors
            let (sorted_indices, sorted_values): (Vec<u32>, Vec<f32>) =
                index_value_pairs.into_iter().unzip();
            indices = sorted_indices;
            values = sorted_values;
        }

        Ok(Self {
            indices,
            values,
            dimension,
        })
    }

    /// Returns the number of non-zero elements.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::SparseVec;
    ///
    /// let sparse = SparseVec::new(vec![1, 3, 5], vec![1.0, 2.0, 3.0], 10).unwrap();
    /// assert_eq!(sparse.nnz(), 3);
    /// ```
    #[inline]
    pub fn nnz(&self) -> usize {
        self.indices.len()
    }

    /// Returns the total dimension of the vector (including zeros).
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::SparseVec;
    ///
    /// let sparse = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
    /// assert_eq!(sparse.dimension(), 100);
    /// ```
    #[inline]
    pub fn dimension(&self) -> usize {
        self.dimension as usize
    }

    /// Returns a slice of the non-zero indices.
    ///
    /// The indices are guaranteed to be sorted in ascending order.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::SparseVec;
    ///
    /// let sparse = SparseVec::new(vec![5, 1, 3], vec![1.0, 2.0, 3.0], 10).unwrap();
    /// // Indices are sorted during construction
    /// assert_eq!(sparse.indices(), &[1, 3, 5]);
    /// ```
    #[inline]
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// Returns a slice of the non-zero values.
    ///
    /// The values correspond to the indices returned by [`indices()`](Self::indices).
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::SparseVec;
    ///
    /// let sparse = SparseVec::new(vec![5, 1, 3], vec![1.0, 2.0, 3.0], 10).unwrap();
    /// let indices = sparse.indices();
    /// let values = sparse.values();
    /// // values[i] corresponds to indices[i]
    /// assert_eq!(values.len(), indices.len());
    /// ```
    #[inline]
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// Converts this sparse vector to a dense vector representation.
    ///
    /// Creates a new vector of length `dimension()` with zeros everywhere
    /// except at the specified indices.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::SparseVec;
    ///
    /// let sparse = SparseVec::new(vec![1, 3], vec![1.5, 2.5], 5).unwrap();
    /// let dense = sparse.to_dense();
    /// assert_eq!(dense, vec![0.0, 1.5, 0.0, 2.5, 0.0]);
    /// ```
    pub fn to_dense(&self) -> Vec<f32> {
        let mut dense = vec![0.0; self.dimension as usize];
        for (&idx, &val) in self.indices.iter().zip(self.values.iter()) {
            dense[idx as usize] = val;
        }
        dense
    }

    /// Computes the squared magnitude (L2 norm squared) of this sparse vector.
    ///
    /// This is more efficient than computing the full magnitude when you only
    /// need to compare magnitudes (since sqrt is monotonic for positive values).
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::SparseVec;
    ///
    /// let sparse = SparseVec::new(vec![0, 1, 2], vec![1.0, 2.0, 2.0], 5).unwrap();
    /// // magnitude² = 1² + 2² + 2² = 1 + 4 + 4 = 9
    /// assert_eq!(sparse.squared_magnitude(), 9.0);
    /// ```
    pub fn squared_magnitude(&self) -> f32 {
        self.values.iter().map(|v| v * v).sum()
    }

    /// Computes the magnitude (L2 norm) of this sparse vector.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::SparseVec;
    ///
    /// let sparse = SparseVec::new(vec![0, 1], vec![3.0, 4.0], 5).unwrap();
    /// // magnitude = sqrt(3² + 4²) = sqrt(9 + 16) = 5.0
    /// assert_eq!(sparse.magnitude(), 5.0);
    /// ```
    #[inline]
    pub fn magnitude(&self) -> f32 {
        self.squared_magnitude().sqrt()
    }

    /// Checks if this sparse vector is approximately equal to another within a tolerance.
    ///
    /// This method provides an epsilon-based comparison that handles floating-point
    /// precision issues. Two sparse vectors are considered approximately equal if:
    /// - They have the same dimension
    /// - They have the same indices
    /// - All corresponding values differ by less than epsilon
    ///
    /// # Arguments
    ///
    /// * `other` - The sparse vector to compare with
    /// * `epsilon` - Maximum allowed difference for each value (typically 1e-6 for f32)
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::vector::SparseVec;
    ///
    /// let a = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
    /// let b = SparseVec::new(vec![0, 2], vec![1.0000001, 2.0000001], 5).unwrap();
    ///
    /// // Small floating-point differences are tolerated
    /// assert!(a.approx_eq(&b, 1e-5));
    /// assert!(!a.approx_eq(&b, 1e-10));
    /// ```
    ///
    /// # Note
    ///
    /// This is the **recommended way to compare sparse vectors**. While `SparseVec`
    /// implements `PartialEq` for compatibility with `PropertyValue`, direct equality
    /// via `==` is not recommended due to floating-point comparison concerns.
    /// See the "Equality and Comparison" section in the type documentation for details.
    pub fn approx_eq(&self, other: &SparseVec, epsilon: f32) -> bool {
        self.dimension == other.dimension
            && self.indices == other.indices
            && self.values.len() == other.values.len()
            && self
                .values
                .iter()
                .zip(other.values.iter())
                .all(|(a, b)| (a - b).abs() < epsilon)
    }
}

// ============================================================================
// Sparse Vector Similarity Functions
// ============================================================================

/// Computes the dot product between two sparse vectors.
///
/// The dot product is computed efficiently by iterating only over non-zero
/// elements. Vectors with different dimensions are handled gracefully by
/// only considering indices that exist in both vectors.
///
/// # Arguments
///
/// * `a` - First sparse vector
/// * `b` - Second sparse vector
///
/// # Returns
///
/// * `Ok(f32)` - The dot product
/// * `Err` - Never returns an error (kept for API consistency with dense vectors)
///
/// # Algorithm
///
/// Uses a merge-like algorithm since indices are sorted:
/// 1. Maintain pointers to both index arrays
/// 2. When indices match, multiply values and add to sum
/// 3. Advance pointer with smaller index
/// 4. Time complexity: O(nnz_a + nnz_b)
///
/// # Example
///
/// ```rust
/// use aletheiadb::core::vector::{SparseVec, sparse_dot_product};
///
/// // Sparse vectors: [1, 0, 2, 0, 0] and [0, 0, 3, 0, 4]
/// let a = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
/// let b = SparseVec::new(vec![2, 4], vec![3.0, 4.0], 5).unwrap();
///
/// // Only index 2 overlaps: 2.0 * 3.0 = 6.0
/// let dot = sparse_dot_product(&a, &b).unwrap();
/// assert_eq!(dot, 6.0);
/// ```
///
/// # Performance
///
/// For vectors with nnz_a and nnz_b non-zero elements:
/// - Time: O(nnz_a + nnz_b) - linear in sparsity
/// - Space: O(1) - no allocations
/// - Much faster than dense dot product when vectors are sparse
pub fn sparse_dot_product(a: &SparseVec, b: &SparseVec) -> Result<f32> {
    if a.dimension() != b.dimension() {
        return Err(VectorError::DimensionMismatch {
            expected: a.dimension(),
            actual: b.dimension(),
        }
        .into());
    }

    let mut sum = 0.0f32;
    let mut i = 0;
    let mut j = 0;

    let a_indices = a.indices();
    let a_values = a.values();
    let b_indices = b.indices();
    let b_values = b.values();

    // Merge-like algorithm since indices are sorted
    while i < a_indices.len() && j < b_indices.len() {
        if a_indices[i] == b_indices[j] {
            // Indices match - multiply and add
            sum += a_values[i] * b_values[j];
            i += 1;
            j += 1;
        } else if a_indices[i] < b_indices[j] {
            // a's index is smaller, advance a
            i += 1;
        } else {
            // b's index is smaller, advance b
            j += 1;
        }
    }

    Ok(sum)
}

/// Computes cosine similarity between two sparse vectors.
///
/// Cosine similarity measures the angle between vectors, ranging from -1 (opposite)
/// to 1 (identical direction). For sparse vectors, this is computed efficiently
/// by only considering non-zero elements.
///
/// # Arguments
///
/// * `a` - First sparse vector
/// * `b` - Second sparse vector
///
/// # Returns
///
/// * `Ok(f32)` - Cosine similarity in range [-1, 1]
/// * `Err` - If either vector has zero magnitude
///
/// # Formula
///
/// ```text
/// cosine_similarity = dot(a, b) / (||a|| * ||b||)
/// ```
///
/// # Example
///
/// ```rust
/// use aletheiadb::core::vector::{SparseVec, sparse_cosine_similarity};
///
/// let a = SparseVec::new(vec![0, 2], vec![1.0, 1.0], 5).unwrap();
/// let b = SparseVec::new(vec![0, 2], vec![1.0, 1.0], 5).unwrap();
///
/// // Identical vectors have cosine similarity = 1.0
/// let sim = sparse_cosine_similarity(&a, &b).unwrap();
/// assert!((sim - 1.0).abs() < 1e-6);
/// ```
///
/// # Performance
///
/// - Time: O(nnz_a + nnz_b) for dot product + O(nnz_a + nnz_b) for magnitudes
/// - Space: O(1)
/// - Much faster than dense cosine for sparse data
pub fn sparse_cosine_similarity(a: &SparseVec, b: &SparseVec) -> Result<f32> {
    let dot = sparse_dot_product(a, b)?;
    let mag_a = a.magnitude();
    let mag_b = b.magnitude();

    // Handle zero magnitude vectors
    if mag_a < SQUARED_MAGNITUDE_THRESHOLD || mag_b < SQUARED_MAGNITUDE_THRESHOLD {
        return Ok(0.0);
    }

    let similarity = dot / (mag_a * mag_b);

    // Clamp to [-1, 1] to handle floating-point errors
    Ok(similarity.clamp(-1.0, 1.0))
}

/// Computes squared Euclidean distance between two sparse vectors.
///
/// The squared distance is more efficient than computing the full Euclidean
/// distance since it avoids the square root operation. For comparing distances,
/// squared distance preserves ordering.
///
/// # Arguments
///
/// * `a` - First sparse vector
/// * `b` - Second sparse vector
///
/// # Returns
///
/// * `Ok(f32)` - Squared Euclidean distance
/// * `Err(VectorError::DimensionMismatch)` - If vectors have different dimensions
///
/// # Formula
///
/// ```text
/// squared_distance = ||a - b||² = Σ(a_i - b_i)²
/// ```
///
/// For sparse vectors, we only need to compute:
/// - ||a||² + ||b||² - 2*dot(a,b)
///
/// # Example
///
/// ```rust
/// use aletheiadb::core::vector::{SparseVec, sparse_squared_euclidean_distance};
///
/// let a = SparseVec::new(vec![0], vec![3.0], 5).unwrap();
/// let b = SparseVec::new(vec![], vec![], 5).unwrap(); // Zero vector
///
/// // Distance from [3,0,0,0,0] to [0,0,0,0,0] = 9
/// let dist_sq = sparse_squared_euclidean_distance(&a, &b).unwrap();
/// assert_eq!(dist_sq, 9.0);
/// ```
///
/// # Performance
///
/// - Time: O(nnz_a + nnz_b)
/// - Space: O(1)
pub fn sparse_squared_euclidean_distance(a: &SparseVec, b: &SparseVec) -> Result<f32> {
    // Check dimensions match
    if a.dimension() != b.dimension() {
        return Err(VectorError::DimensionMismatch {
            expected: a.dimension(),
            actual: b.dimension(),
        }
        .into());
    }

    // Stable algorithm: sum((a_i - b_i)^2)
    // Avoids catastrophic cancellation from ||a||^2 + ||b||^2 - 2<a,b>
    // when a and b are very close.
    // Also faster (O(N) single pass vs O(N) triple pass).

    let mut sum_sq_diff = 0.0f32;
    let mut i = 0;
    let mut j = 0;

    let a_indices = a.indices();
    let a_values = a.values();
    let b_indices = b.indices();
    let b_values = b.values();

    while i < a_indices.len() && j < b_indices.len() {
        if a_indices[i] == b_indices[j] {
            let diff = a_values[i] - b_values[j];
            sum_sq_diff += diff * diff;
            i += 1;
            j += 1;
        } else if a_indices[i] < b_indices[j] {
            sum_sq_diff += a_values[i] * a_values[i];
            i += 1;
        } else {
            sum_sq_diff += b_values[j] * b_values[j];
            j += 1;
        }
    }

    // Process remaining elements
    while i < a_indices.len() {
        sum_sq_diff += a_values[i] * a_values[i];
        i += 1;
    }

    while j < b_indices.len() {
        sum_sq_diff += b_values[j] * b_values[j];
        j += 1;
    }

    Ok(sum_sq_diff)
}

/// Computes Euclidean distance between two sparse vectors.
///
/// This is the L2 distance - the straight-line distance between two points.
///
/// # Arguments
///
/// * `a` - First sparse vector
/// * `b` - Second sparse vector
///
/// # Returns
///
/// * `Ok(f32)` - Euclidean distance
/// * `Err(VectorError::DimensionMismatch)` - If vectors have different dimensions
///
/// # Example
///
/// ```rust
/// use aletheiadb::core::vector::{SparseVec, sparse_euclidean_distance};
///
/// let a = SparseVec::new(vec![0], vec![3.0], 5).unwrap();
/// let b = SparseVec::new(vec![], vec![], 5).unwrap(); // Zero vector
///
/// // Distance from [3,0,0,0,0] to [0,0,0,0,0] = 3.0
/// let dist = sparse_euclidean_distance(&a, &b).unwrap();
/// assert_eq!(dist, 3.0);
/// ```
#[inline]
pub fn sparse_euclidean_distance(a: &SparseVec, b: &SparseVec) -> Result<f32> {
    sparse_squared_euclidean_distance(a, b).map(|sq| sq.sqrt())
}
