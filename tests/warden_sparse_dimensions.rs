use aletheiadb::core::vector::{SparseVec, sparse_cosine_similarity, sparse_dot_product};
use aletheiadb::utils::error::{Error, VectorError};

#[test]
fn test_sparse_ops_dimension_mismatch() {
    let v1 = SparseVec::new(vec![0], vec![1.0], 10).unwrap();
    let v2 = SparseVec::new(vec![0], vec![1.0], 100).unwrap(); // Different dimension

    // Dot product should fail
    let dot_result = sparse_dot_product(&v1, &v2);
    assert!(
        dot_result.is_err(),
        "Dot product should fail with dimension mismatch"
    );
    match dot_result {
        Err(Error::Vector(VectorError::DimensionMismatch { expected, actual })) => {
            assert_eq!(expected, 10);
            assert_eq!(actual, 100);
        }
        _ => panic!(
            "Expected VectorError::DimensionMismatch, got {:?}",
            dot_result
        ),
    }

    // Cosine similarity should fail
    let cos_result = sparse_cosine_similarity(&v1, &v2);
    assert!(
        cos_result.is_err(),
        "Cosine similarity should fail with dimension mismatch"
    );
    match cos_result {
        Err(Error::Vector(VectorError::DimensionMismatch { expected, actual })) => {
            assert_eq!(expected, 10);
            assert_eq!(actual, 100);
        }
        _ => panic!(
            "Expected VectorError::DimensionMismatch, got {:?}",
            cos_result
        ),
    }
}
