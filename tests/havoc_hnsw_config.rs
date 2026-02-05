use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder};

#[test]
fn test_havoc_ef_construction_crash() {
    // Attempt to crash usearch by requesting 1 billion candidates per search
    // This should cause massive allocation in C++ heap during add().

    // ef_construction = 1,000,000,000
    // If usearch allocates vector of this size, it's >4GB or >8GB.
    // On limited RAM CI, this should OOM.

    // Fix: We now validate ef_construction <= 4096.

    let result = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
        .ef_construction(1_000_000_000)
        .build();

    // Verify the builder rejected the dangerous configuration
    assert!(result.is_err(), "Builder should reject excessive ef_construction to prevent DoS");

    if let Err(e) = result {
        // Optional: verify error message
        let msg = e.to_string();
        println!("Builder rejected config as expected: {}", msg);
        assert!(msg.contains("ef_construction must be in range"), "Error should mention parameter range");
    }
}
