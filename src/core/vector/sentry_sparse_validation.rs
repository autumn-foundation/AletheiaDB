use super::sparse::SparseVec;
use crate::core::error::{Error, VectorError};
use crate::core::property::MAX_VECTOR_DIMENSIONS;

// ============================================================================
// Fast Path Tests (Sorted Input)
// ============================================================================

#[test]
fn test_sparse_vec_new_fast_path_valid() {
    // Already sorted, unique, non-zero, within bounds
    let indices = vec![1, 3, 5];
    let values = vec![1.0, 2.0, 3.0];
    let dim = 10;

    let result = SparseVec::new(indices.clone(), values.clone(), dim);
    assert!(result.is_ok());
    let sv = result.unwrap();
    assert_eq!(sv.indices(), &indices[..]);
    assert_eq!(sv.values(), &values[..]);
}

#[test]
fn test_sparse_vec_new_fast_path_duplicates() {
    // Sorted but has duplicates
    let indices = vec![1, 3, 3, 5];
    let values = vec![1.0, 2.0, 2.5, 3.0];
    let dim = 10;

    let result = SparseVec::new(indices, values, dim);
    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::InvalidSparseVector { reason })) => {
            assert!(reason.contains("Duplicate index 3"));
        }
        _ => panic!("Expected InvalidSparseVector error for duplicates"),
    }
}

#[test]
fn test_sparse_vec_new_fast_path_out_of_bounds() {
    // Sorted but last index is OOB
    let indices = vec![1, 3, 10];
    let values = vec![1.0, 2.0, 3.0];
    let dim = 10; // Index 10 is OOB for size 10 (0..9)

    let result = SparseVec::new(indices, values, dim);
    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::InvalidSparseVector { reason })) => {
            assert!(reason.contains("out of bounds"));
        }
        _ => panic!("Expected InvalidSparseVector error for OOB"),
    }
}

#[test]
fn test_sparse_vec_new_fast_path_zero_value() {
    // Sorted but contains a zero value
    let indices = vec![1, 3, 5];
    let values = vec![1.0, 0.0, 3.0];
    let dim = 10;

    let result = SparseVec::new(indices, values, dim);
    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::InvalidSparseVector { reason })) => {
            assert!(reason.contains("zero value"));
        }
        _ => panic!("Expected InvalidSparseVector error for zero value"),
    }
}

#[test]
fn test_sparse_vec_new_fast_path_nan() {
    let indices = vec![1];
    let values = vec![f32::NAN];
    let dim = 10;

    let result = SparseVec::new(indices, values, dim);
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        Error::Vector(VectorError::ContainsNaN { .. })
    ));
}

// ============================================================================
// Slow Path Tests (Unsorted Input)
// ============================================================================

#[test]
fn test_sparse_vec_new_slow_path_valid() {
    // Unsorted, valid
    let indices = vec![5, 1, 3];
    let values = vec![3.0, 1.0, 2.0];
    let dim = 10;

    let result = SparseVec::new(indices, values, dim);
    assert!(result.is_ok());
    let sv = result.unwrap();
    // Should be sorted
    assert_eq!(sv.indices(), &[1, 3, 5]);
    assert_eq!(sv.values(), &[1.0, 2.0, 3.0]);
}

#[test]
fn test_sparse_vec_new_slow_path_duplicates() {
    // Unsorted with duplicates (3 appears twice)
    let indices = vec![5, 3, 1, 3];
    let values = vec![3.0, 2.0, 1.0, 2.5];
    let dim = 10;

    let result = SparseVec::new(indices, values, dim);
    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::InvalidSparseVector { reason })) => {
            assert!(reason.contains("Duplicate index 3"));
        }
        _ => panic!("Expected InvalidSparseVector error for duplicates"),
    }
}

#[test]
fn test_sparse_vec_new_slow_path_out_of_bounds() {
    // Unsorted with OOB
    let indices = vec![5, 10, 1];
    let values = vec![3.0, 4.0, 1.0];
    let dim = 10;

    let result = SparseVec::new(indices, values, dim);
    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::InvalidSparseVector { reason })) => {
            assert!(reason.contains("out of bounds"));
        }
        _ => panic!("Expected InvalidSparseVector error for OOB"),
    }
}

#[test]
fn test_sparse_vec_new_slow_path_zero_value() {
    // Unsorted with zero
    let indices = vec![5, 1, 3];
    let values = vec![3.0, 0.0, 2.0]; // Index 1 has value 0.0
    let dim = 10;

    let result = SparseVec::new(indices, values, dim);
    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::InvalidSparseVector { reason })) => {
            assert!(reason.contains("zero value"));
        }
        _ => panic!("Expected InvalidSparseVector error for zero value"),
    }
}

// ============================================================================
// Edge Cases & Limits
// ============================================================================

#[test]
fn test_sparse_vec_dimension_check() {
    let indices = vec![0];
    let values = vec![1.0];
    let dim = MAX_VECTOR_DIMENSIONS + 1;

    // Use a cast to u32 if needed, but MAX_VECTOR_DIMENSIONS is usize
    let result = SparseVec::new(indices, values, dim as u32);
    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::DimensionTooLarge { .. })) => {} // Expected
        _ => panic!("Expected DimensionTooLarge error"),
    }
}

#[test]
fn test_sparse_vec_mismatched_lengths() {
    let indices = vec![1, 2];
    let values = vec![1.0];
    let dim = 10;

    let result = SparseVec::new(indices, values, dim);
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        Error::Vector(VectorError::DimensionMismatch { .. })
    ));
}
