use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder};

#[test]
fn test_havoc_ef_construction_too_large() {
    // Attempt to crash usearch by requesting 1 billion candidates per search
    let result = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
        .ef_construction(1_000_000_000)
        .build();

    assert!(result.is_err(), "Builder should reject excessive ef_construction");
    if let Err(e) = result {
        let msg = e.to_string();
        assert!(msg.contains("ef_construction must be in range"), "Error: {}", msg);
    }
}

#[test]
fn test_havoc_ef_construction_too_small() {
    // Lower bound check (< 10)
    let result = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
        .ef_construction(5)
        .build();

    assert!(result.is_err(), "Builder should reject small ef_construction");
    if let Err(e) = result {
        let msg = e.to_string();
        assert!(msg.contains("ef_construction must be in range"), "Error: {}", msg);
    }
}

#[test]
fn test_havoc_ef_search_too_large() {
    // Upper bound check (> 4096)
    let result = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
        .ef_search(5000)
        .build();

    assert!(result.is_err(), "Builder should reject excessive ef_search");
    if let Err(e) = result {
        let msg = e.to_string();
        assert!(msg.contains("ef_search must be in range"), "Error: {}", msg);
    }
}

#[test]
fn test_havoc_ef_search_too_small() {
    // Lower bound check (< 1)
    let result = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
        .ef_search(0)
        .build();

    assert!(result.is_err(), "Builder should reject zero ef_search");
    if let Err(e) = result {
        let msg = e.to_string();
        assert!(msg.contains("ef_search must be in range"), "Error: {}", msg);
    }
}
