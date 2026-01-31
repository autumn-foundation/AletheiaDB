//! Type definitions for vector operations.

use crate::core::property::MAX_VECTOR_DIMENSIONS;
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
pub(crate) const SQUARED_MAGNITUDE_THRESHOLD: f32 = 1e-14;

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

/// Maximum allowed vector dimension.
///
/// Re-exported from [`crate::core::property::MAX_VECTOR_DIMENSIONS`] for convenience.
/// This limit (100,000) far exceeds typical embedding sizes and exists to prevent
/// DoS attacks via memory exhaustion during deserialization.
pub const MAX_DIMENSION: VectorDimension = VectorDimension(MAX_VECTOR_DIMENSIONS);
