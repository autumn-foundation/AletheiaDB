use aletheiadb::core::vector::SparseVec;
use aletheiadb::core::vector::sparse::{sparse_dot_product, sparse_cosine_similarity};
use aletheiadb::utils::error::{Error, VectorError};

#[test]
fn test_sparse_dot_product_dimension_mismatch() {
    let vec_a = SparseVec::new(vec![0, 5], vec![1.0, 2.0], 10).unwrap();
    let vec_b = SparseVec::new(vec![0, 15], vec![1.0, 2.0], 20).unwrap();

    let result = sparse_dot_product(&vec_a, &vec_b);

    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::DimensionMismatch { expected, actual })) => {
            assert!((expected == 10 && actual == 20) || (expected == 20 && actual == 10));
        }
        _ => panic!("Expected DimensionMismatch error, got {:?}", result),
    }
}

#[test]
fn test_sparse_cosine_similarity_dimension_mismatch() {
    let vec_a = SparseVec::new(vec![0, 5], vec![1.0, 2.0], 10).unwrap();
    let vec_b = SparseVec::new(vec![0, 15], vec![1.0, 2.0], 20).unwrap();

    let result = sparse_cosine_similarity(&vec_a, &vec_b);

    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::DimensionMismatch { expected, actual })) => {
            assert!((expected == 10 && actual == 20) || (expected == 20 && actual == 10));
        }
        _ => panic!("Expected DimensionMismatch error, got {:?}", result),
    }
}
