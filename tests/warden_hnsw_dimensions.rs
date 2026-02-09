use aletheiadb::core::property::MAX_VECTOR_DIMENSIONS;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use aletheiadb::utils::error::{Error, VectorError};

#[test]
fn test_hnsw_dimension_overflow_rejection() {
    // Attempt to build an index with dimensions exceeding the maximum allowed.
    // MAX_VECTOR_DIMENSIONS is 100,000.
    // This protects against OOM DoS and potential UB in unsafe code blocks
    // that rely on dimensions * size_of<f32> fitting in isize::MAX.

    let excessive_dims = MAX_VECTOR_DIMENSIONS + 1;

    let result = HnswIndexBuilder::new(excessive_dims, DistanceMetric::Cosine).build();

    match result {
        Ok(_) => panic!(
            "HNSW builder accepted excessively large dimensions ({})",
            excessive_dims
        ),
        Err(Error::Vector(VectorError::InvalidVector { reason })) => {
            assert!(
                reason.contains("exceeds maximum"),
                "Error message should mention maximum limit, got: {}",
                reason
            );
        }
        Err(e) => panic!("Expected InvalidVector error, got: {:?}", e),
    }
}
