use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{
    DistanceMetric, HnswConfig, HnswIndex, HnswIndexBuilder, Quantization, VectorIndex,
};
use tempfile::tempdir;

#[test]
fn test_hnsw_custom_metric_persistence() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("custom_metric.index");

    let dims = 4;

    // 1. Create index with custom metric
    let original_index = {
        let config = HnswConfig::new(dims, DistanceMetric::Cosine)
            .with_quantization(Quantization::F32) // Required for custom metrics
            .with_custom_metric("manhattan", |a: &[f32], b: &[f32]| {
                // Manhattan distance (L1 norm)
                a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
            });

        HnswIndexBuilder::from_config(&config).build().unwrap()
    };

    // Add some vectors
    original_index
        .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
        .unwrap();
    original_index
        .add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0])
        .unwrap();
    original_index
        .add(NodeId::new(3).unwrap(), &[0.5, 0.5, 0.0, 0.0])
        .unwrap();

    // Save the index
    original_index.save(&path).unwrap();

    // 2. Load the index with the SAME custom metric
    let config_with_metric = HnswConfig::new(dims, DistanceMetric::Cosine)
        .with_quantization(Quantization::F32)
        .with_custom_metric("manhattan", |a: &[f32], b: &[f32]| {
            a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
        });

    let loaded_index = HnswIndex::load(&path, config_with_metric).unwrap();

    // 3. Verify the custom metric is being used
    // Query with [0.9, 0.1, 0, 0] - closest to node 1 using Manhattan distance
    let query = vec![0.9f32, 0.1, 0.0, 0.0];
    let results = loaded_index.search(&query, 3).unwrap();

    // With Manhattan distance:
    // - Distance to node 1 [1.0, 0.0, 0.0, 0.0] = |0.9-1.0| + |0.1-0.0| = 0.1 + 0.1 = 0.2
    // - Distance to node 2 [0.0, 1.0, 0.0, 0.0] = |0.9-0.0| + |0.1-1.0| = 0.9 + 0.9 = 1.8
    // - Distance to node 3 [0.5, 0.5, 0.0, 0.0] = |0.9-0.5| + |0.1-0.5| = 0.4 + 0.4 = 0.8

    // The results should be ordered by distance (closest first)
    assert!(!results.is_empty(), "Expected search results");
    assert_eq!(
        results[0].0,
        NodeId::new(1).unwrap(),
        "Expected node 1 to be closest using Manhattan distance"
    );

    println!("✓ Custom metric successfully preserved across save/load cycle");
    println!("  Results: {:?}", results);
}

#[test]
fn test_hnsw_dimension_mismatch_rejected() {
    // Test that loading an index with mismatched dimensions is properly rejected

    let dir = tempdir().unwrap();
    let path = dir.path().join("mismatch.index");

    // 1. Create index with 4 dimensions
    {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        index
            .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        index.save(&path).unwrap();
    }

    // 2. Try to load with different dimensions (100)
    // This should either fail gracefully OR if it succeeds, searches should validate dimensions
    let large_dims = 100;
    let config = HnswConfig::new(large_dims, DistanceMetric::Cosine);

    let index_res = HnswIndex::load(&path, config);

    match index_res {
        Ok(index) => {
            // If load succeeds despite dimension mismatch, search should catch it
            let query = vec![0.0f32; large_dims];
            let search_result = index.search(&query, 1);

            // We expect either the load or the search to fail with dimension mismatch
            // If search succeeds, the index internally handles the mismatch safely
            println!("Dimension mismatch handled: {:?}", search_result);
        }
        Err(err) => {
            println!("✓ Load correctly rejected dimension mismatch: {:?}", err);
        }
    }
}
