use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};

#[test]
fn test_havoc_hnsw_capacity_limit() {
    // Regression test for OOM vulnerability in HnswIndexBuilder.
    // Ensure that requesting massive capacity returns an error instead of aborting the process.

    let huge_capacity = usize::MAX / 2;

    println!("Attempting to build index with capacity: {}", huge_capacity);

    let result = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
        .initial_capacity(huge_capacity)
        .build();

    // We expect an error, not a crash.
    match result {
        Ok(_) => panic!("Should not have succeeded allocating huge capacity!"),
        Err(e) => {
            let msg = e.to_string();
            println!("Caught expected error: {}", msg);
            assert!(msg.contains("exceeds maximum allowed"));
        }
    }
}
