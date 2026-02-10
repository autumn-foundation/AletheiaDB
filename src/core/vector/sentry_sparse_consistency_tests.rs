use super::*;
use crate::utils::error::{Error, VectorError};

// This file documents and tests the consistency (or lack thereof) of sparse vector operations
// regarding dimension mismatch handling.
//
// Current state:
// - sparse_dot_product: ALLOWS mismatch (inconsistent with dense)
// - sparse_cosine_similarity: ALLOWS mismatch (inconsistent with dense)
// - sparse_squared_euclidean_distance: REJECTS mismatch (consistent)
//
// These tests establish the baseline behavior before refactoring.

#[test]
fn test_sparse_dot_product_mismatch_behavior() {
    // 🧪 Strategy: Create sparse vectors with different dimensions but potentially overlapping indices.
    // 💣 Risk: If dimension check is missing, this will succeed, potentially returning a mathematically questionable result.

    let a = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
    let b = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 10).unwrap(); // Different dimension (10 vs 5)

    let result = sparse_dot_product(&a, &b);

    // NEW BEHAVIOR: Fails (Consistent with dense)
    assert!(result.is_err(), "sparse_dot_product should reject dimension mismatch");
    assert!(matches!(
        result.unwrap_err(),
        Error::Vector(VectorError::DimensionMismatch { .. })
    ));
}

#[test]
fn test_sparse_cosine_similarity_mismatch_behavior() {
    // 🧪 Strategy: Create sparse vectors with different dimensions.
    // 💣 Risk: Cosine similarity usually implies vectors are in the same inner product space.

    let a = SparseVec::new(vec![0, 2], vec![1.0, 1.0], 5).unwrap();
    let b = SparseVec::new(vec![0, 2], vec![1.0, 1.0], 100).unwrap(); // Mismatched

    let result = sparse_cosine_similarity(&a, &b);

    // NEW BEHAVIOR: Fails (Consistent with dense)
    assert!(result.is_err(), "sparse_cosine_similarity should reject dimension mismatch");
    assert!(matches!(
        result.unwrap_err(),
        Error::Vector(VectorError::DimensionMismatch { .. })
    ));
}

#[test]
fn test_sparse_euclidean_mismatch_behavior() {
    // 🧪 Strategy: Create sparse vectors with different dimensions.
    // 💣 Risk: Euclidean distance calculation requires dimension match.

    let a = SparseVec::new(vec![0], vec![1.0], 5).unwrap();
    let b = SparseVec::new(vec![0], vec![1.0], 10).unwrap();

    let result = sparse_squared_euclidean_distance(&a, &b);

    // CURRENT BEHAVIOR: Fails (Correct/Consistent)
    assert!(result.is_err(), "sparse_squared_euclidean_distance correctly rejects mismatch");
    assert!(matches!(
        result.unwrap_err(),
        Error::Vector(VectorError::DimensionMismatch { .. })
    ));
}
