use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use aletheiadb::utils::Error;

#[test]
fn test_hnsw_excessive_dimensions_dos() {
    // Attempt to create an index with massive dimensions
    // This should fail gracefully with a specific error, not crash or try to allocate massive memory
    let massive_dims = 1_000_000_000;

    let result = HnswIndexBuilder::new(massive_dims, DistanceMetric::Cosine)
        .build();

    match result {
        Ok(_) => {
            // If it succeeds, it's a vulnerability because we allowed massive allocation
            panic!("Should not allow creating index with {} dimensions", massive_dims);
        }
        Err(e) => {
            println!("Rejected with: {}", e);
            if let Error::Vector(ve) = e {
                let msg = ve.to_string();
                // We expect our specific validation error
                if msg.contains("dimensions") && msg.contains("too large") {
                    println!("Successfully rejected with dimensions error");
                    return;
                }

                // If we got "failed to reserve memory", that means we tried to allocate and failed (OOM protection)
                // This is what we want to prevent - we want to catch it BEFORE allocation attempt.
                if msg.contains("failed to reserve memory") {
                    panic!("Vulnerability: Attempted massive allocation (caught by OOM, but should be caught by validation)");
                }

                panic!("Rejected but with unexpected error: {}", msg);
            } else {
                panic!("Rejected with non-Vector error: {}", e);
            }
        }
    }
}
