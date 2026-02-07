use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use aletheiadb::utils::error::{Error, VectorError};

#[test]
fn test_warden_hnsw_dos_capacity() {
    // 🛡️ Warden: Test DoS vector via excessive capacity
    // Attempt to allocate 100M vectors (should be rejected)
    const HUGE_CAPACITY: usize = 100_000_000;

    let result = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
        .initial_capacity(HUGE_CAPACITY)
        .build();

    assert!(result.is_err(), "Should reject huge capacity");

    match result {
        Err(Error::Vector(VectorError::InvalidVector { reason })) => {
            assert!(reason.contains("capacity"));
        }
        _ => panic!(
            "Expected InvalidVector error for capacity, got {:?}",
            result
        ),
    }
}

#[test]
fn test_warden_hnsw_dos_dimensions() {
    // 🛡️ Warden: Test DoS vector via excessive dimensions
    // Attempt to create index with 1M dimensions (should be rejected)
    const HUGE_DIMS: usize = 1_000_000;

    let result = HnswIndexBuilder::new(HUGE_DIMS, DistanceMetric::Cosine).build();

    assert!(result.is_err(), "Should reject huge dimensions");

    match result {
        Err(Error::Vector(VectorError::InvalidVector { reason })) => {
            assert!(reason.contains("dimensions"));
        }
        _ => panic!(
            "Expected InvalidVector error for dimensions, got {:?}",
            result
        ),
    }
}
