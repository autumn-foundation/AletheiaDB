use crate::utils::error::{Error, Result, VectorError};

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
/// use aletheiadb::core::vector::validate_vector;
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
/// use aletheiadb::core::vector::check_dimensions_match;
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
/// use aletheiadb::core::vector::validate_vector_with_bounds;
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
