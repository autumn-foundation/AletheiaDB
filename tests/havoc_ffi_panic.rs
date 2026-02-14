use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, Quantization};

#[test]
fn test_ffi_panic_resilience_metric() {
    // This test verifies that panics in custom metrics don't cause undefined behavior (UB)
    // by unwinding through FFI boundaries.

    let builder = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .quantization(Quantization::F32)
        .with_custom_metric("panic_metric", |_, _| {
            panic!("Panic in custom metric!");
        });

    let index = builder.build().expect("Failed to build index");

    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    index.add(node1, &vec1).expect("Failed to add vector");

    let query = vec![1.0, 0.0, 0.0, 0.0];

    // This search calls the metric, which panics.
    // The wrapper should catch the panic and return f32::MAX.
    // The process should NOT abort.
    let results = index.search(&query, 10).expect("Search failed");

    // Since we return f32::MAX (dissimilar), results should be valid but likely have low similarity
    // or be empty if thresholding is applied (though search returns k results regardless of score usually).
    println!("Search results: {:?}", results);
}

#[test]
fn test_ffi_panic_resilience_filter() {
    // This test verifies that panics in search filters don't cause UB.

    let builder = HnswIndexBuilder::new(4, DistanceMetric::Cosine);
    let index = builder.build().expect("Failed to build index");

    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    index.add(node1, &vec1).expect("Failed to add vector");

    let query = vec![1.0, 0.0, 0.0, 0.0];

    // Search with a filter that panics
    let results = index
        .search_with_filter(&query, 10, |_| {
            panic!("Panic in filter predicate!");
        })
        .expect("Search failed");

    // The wrapper should catch panic and return false (filter out).
    // So results should be empty.
    assert!(
        results.is_empty(),
        "Expected empty results due to filter panic"
    );
}
