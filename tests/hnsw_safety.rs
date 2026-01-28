use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::{DistanceMetric, HnswConfig, HnswIndexBuilder};
use gallifreydb::index::VectorIndex;

#[test]
fn test_panic_in_custom_metric_does_not_abort() {
    // Create config with a custom metric that panics
    let config = HnswConfig::new(4, DistanceMetric::Cosine)
        .with_custom_metric("panicking_metric", |_, _| {
            panic!("This panic should be caught!");
        });

    // Build index from config
    let index = HnswIndexBuilder::from_config(&config)
        .build()
        .expect("Failed to build index");

    // Add a vector
    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![1.0, 0.0, 0.0, 0.0];
    index.add(node1, &vec1).expect("First add failed");

    // Add another vector - this triggers distance calculation during insertion/search
    let node2 = NodeId::new(2).unwrap();
    let vec2 = vec![0.0, 1.0, 0.0, 0.0];

    // The add operation involves searching for nearest neighbors to connect to.
    // This will call the distance metric.
    // Since our metric panics, it should now return f32::INFINITY.
    // The insertion might "succeed" but with infinite distance (placing it far away),
    // or fail gracefully. What matters is the process doesn't abort.
    let result = index.add(node2, &vec2);

    // We verify the process didn't abort.
    assert!(result.is_ok(), "Add with panicking metric failed but didn't crash");

    // Search should also trigger the panic
    let search_result = index.search(&[1.0, 0.0, 0.0, 0.0], 10);

    assert!(search_result.is_ok(), "Search with panicking metric failed but didn't crash");

    let results = search_result.unwrap();
    // Since all distances are INFINITY, similarities will be very low (or -INFINITY depending on conversion)
    // But we just care about survival.
    println!("Survived panic! Results found: {}", results.len());
}
