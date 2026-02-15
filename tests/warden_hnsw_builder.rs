use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};

#[test]
fn test_hnsw_builder_huge_dimensions() {
    // 1 billion dimensions * 4 bytes = 4GB per vector.
    // This is huge but "allocatable" on large RAM machines, potentially causing OOM.
    // More importantly, it might exceed isize::MAX if we go larger, causing UB in slice::from_raw_parts.
    // Try with a value that is definitely too large for our application logic but fits in usize.
    let huge_dims = 1_000_000_000;

    let result = HnswIndexBuilder::new(huge_dims, DistanceMetric::Cosine).build();

    match result {
        Ok(_) => panic!("Builder should have rejected huge dimensions"),
        Err(e) => {
            let msg = e.to_string();
            // We now expect a specific validation error
            assert!(msg.contains("dimensions 1000000000 exceeds maximum allowed"), "Unexpected error message: {}", msg);
        }
    }
}
