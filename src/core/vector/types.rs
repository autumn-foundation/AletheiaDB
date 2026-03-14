use super::constants::MAX_VECTOR_DIMENSIONS;
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
/// use aletheiadb::core::vector::VectorDimension;
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
    /// use aletheiadb::core::vector::VectorDimension;
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
    /// use aletheiadb::core::vector::VectorDimension;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vector::constants::MAX_VECTOR_DIMENSIONS;

    #[test]
    fn test_vector_dimension_new() {
        let dim = VectorDimension::new(100);
        assert_eq!(dim.0, 100);
    }

    #[test]
    fn test_vector_dimension_as_usize() {
        let dim = VectorDimension::new(100);
        assert_eq!(dim.as_usize(), 100);
    }

    #[test]
    fn test_vector_dimension_is_zero() {
        assert!(VectorDimension::new(0).is_zero());
        assert!(!VectorDimension::new(1).is_zero());
    }

    #[test]
    fn test_vector_dimension_exceeds_max() {
        assert!(!VectorDimension::new(MAX_VECTOR_DIMENSIONS - 1).exceeds_max());
        assert!(!VectorDimension::new(MAX_VECTOR_DIMENSIONS).exceeds_max());
        assert!(VectorDimension::new(MAX_VECTOR_DIMENSIONS + 1).exceeds_max());
    }

    #[test]
    fn test_vector_dimension_fmt() {
        let dim = VectorDimension::new(100);
        assert_eq!(format!("{}", dim), "100");
    }

    #[test]
    fn test_vector_dimension_from_usize() {
        let dim: VectorDimension = 100.into();
        assert_eq!(dim.0, 100);

        let dim2 = VectorDimension::from(200);
        assert_eq!(dim2.0, 200);
    }

    #[test]
    fn test_usize_from_vector_dimension() {
        let dim = VectorDimension::new(100);
        let val: usize = dim.into();
        assert_eq!(val, 100);

        let val2 = usize::from(VectorDimension::new(200));
        assert_eq!(val2, 200);
    }

    #[test]
    fn test_vector_dimension_default() {
        let dim = VectorDimension::default();
        assert_eq!(dim.0, 0);
        assert!(dim.is_zero());
    }
}
