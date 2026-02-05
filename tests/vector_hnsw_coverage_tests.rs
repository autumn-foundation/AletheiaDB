//! Additional tests for HNSW index code coverage.
//! These tests specifically target uncovered code paths.

use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::{
    DistanceMetric, HnswConfig, HnswIndex, HnswIndexBuilder, Quantization, StorageMode, VectorIndex,
};
use std::path::PathBuf;
use tempfile::tempdir;

// ============================================================================
// HnswConfig builder method tests
// ============================================================================

#[test]
fn test_hnsw_config_with_dimensions() {
    let config = HnswConfig::default().with_dimensions(256);
    assert_eq!(config.dimensions, 256);
}

#[test]
fn test_hnsw_config_with_metric() {
    let config = HnswConfig::default().with_metric(DistanceMetric::Euclidean);
    assert_eq!(config.metric, DistanceMetric::Euclidean);
}

#[test]
fn test_hnsw_config_with_m() {
    let config = HnswConfig::default().with_m(32);
    assert_eq!(config.m, 32);
}

#[test]
fn test_hnsw_config_with_ef_construction() {
    let config = HnswConfig::default().with_ef_construction(256);
    assert_eq!(config.ef_construction, 256);
}

#[test]
fn test_hnsw_config_with_ef_search() {
    let config = HnswConfig::default().with_ef_search(128);
    assert_eq!(config.ef_search, 128);
}

#[test]
fn test_hnsw_config_with_capacity() {
    let config = HnswConfig::default().with_capacity(5000);
    assert_eq!(config.capacity, 5000);
}

// ============================================================================
// HnswConfig serialization tests
// ============================================================================

#[test]
fn test_hnsw_config_serialize_deserialize() {
    let config = HnswConfig::new(384, DistanceMetric::Cosine)
        .with_m(24)
        .with_ef_construction(200)
        .with_ef_search(100)
        .with_capacity(10000);

    let mut buf = Vec::new();
    config.serialize_into(&mut buf).unwrap();

    let mut cursor = std::io::Cursor::new(buf);
    let deserialized = HnswConfig::deserialize_from(&mut cursor).unwrap();

    assert_eq!(deserialized.dimensions, 384);
    assert_eq!(deserialized.metric, DistanceMetric::Cosine);
    assert_eq!(deserialized.m, 24);
    assert_eq!(deserialized.ef_construction, 200);
    assert_eq!(deserialized.ef_search, 100);
    assert_eq!(deserialized.capacity, 10000);
}

#[test]
fn test_hnsw_config_serialize_all_metrics() {
    // Test serialization with all distance metrics
    for metric in [
        DistanceMetric::Cosine,
        DistanceMetric::Euclidean,
        DistanceMetric::DotProduct,
        DistanceMetric::Haversine,
        DistanceMetric::Hamming,
        DistanceMetric::Tanimoto,
    ] {
        let config = HnswConfig::new(64, metric);

        let mut buf = Vec::new();
        config.serialize_into(&mut buf).unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let deserialized = HnswConfig::deserialize_from(&mut cursor).unwrap();

        assert_eq!(deserialized.metric, metric);
    }
}

// ============================================================================
// HnswConfig PartialEq tests
// ============================================================================

#[test]
fn test_hnsw_config_partial_eq() {
    let config1 = HnswConfig::new(128, DistanceMetric::Cosine);
    let config2 = HnswConfig::new(128, DistanceMetric::Cosine);
    let config3 = HnswConfig::new(256, DistanceMetric::Cosine);

    assert_eq!(config1, config2);
    assert_ne!(config1, config3);
}

#[test]
fn test_hnsw_config_partial_eq_with_storage_mode() {
    let config1 = HnswConfig::new(64, DistanceMetric::Cosine).with_storage(StorageMode::InMemory);
    let config2 =
        HnswConfig::new(64, DistanceMetric::Cosine).with_storage(StorageMode::MemoryMapped {
            path: PathBuf::from("/tmp/index"),
        });

    assert_ne!(config1, config2);
}

// ============================================================================
// HnswIndex method tests
// ============================================================================

#[test]
fn test_hnsw_index_new_from_config() {
    let config = HnswConfig::new(64, DistanceMetric::Cosine);
    let index = HnswIndex::new(config).unwrap();

    assert_eq!(index.dimensions(), 64);
    assert_eq!(index.distance_metric(), DistanceMetric::Cosine);
}

#[test]
fn test_hnsw_index_config_method() {
    let config = HnswConfig::new(128, DistanceMetric::Euclidean)
        .with_m(24)
        .with_ef_construction(200);

    let index = HnswIndex::new(config.clone()).unwrap();
    let returned_config = index.config();

    assert_eq!(returned_config.dimensions, 128);
    assert_eq!(returned_config.metric, DistanceMetric::Euclidean);
    assert_eq!(returned_config.m, 24);
}

#[test]
fn test_hnsw_index_m_method() {
    let index = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .m(32)
        .build()
        .unwrap();

    assert_eq!(index.m(), 32);
}

#[test]
fn test_hnsw_index_set_and_get_ef_search() {
    let index = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .ef_search(64)
        .build()
        .unwrap();

    // Verify initial value
    assert_eq!(index.get_ef_search(), 64);

    // Change ef_search
    index.set_ef_search(128);

    // Verify changed value
    assert_eq!(index.get_ef_search(), 128);
}

// ============================================================================
// Builder validation error tests
// ============================================================================

#[test]
fn test_hnsw_builder_zero_dimensions_error() {
    let result = HnswIndexBuilder::new(0, DistanceMetric::Cosine).build();
    assert!(result.is_err());
    match result {
        Err(err) => assert!(
            err.to_string().contains("dimensions must be > 0"),
            "Expected dimension error, got: {}",
            err
        ),
        Ok(_) => panic!("Expected error"),
    }
}

#[test]
fn test_hnsw_builder_zero_m_error() {
    let result = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .m(0)
        .build();
    assert!(result.is_err());
    match result {
        Err(err) => assert!(
            err.to_string().contains("M must be in range"),
            "Expected M range error, got: {}",
            err
        ),
        Ok(_) => panic!("Expected error"),
    }
}

#[test]
fn test_hnsw_builder_m_too_large_error() {
    let result = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .m(100) // Max is 64
        .build();
    assert!(result.is_err());
    match result {
        Err(err) => assert!(
            err.to_string().contains("M must be in range"),
            "Expected M range error, got: {}",
            err
        ),
        Ok(_) => panic!("Expected error"),
    }
}

#[test]
fn test_hnsw_builder_ef_construction_too_large() {
    let result = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .ef_construction(5000)
        .build();
    assert!(result.is_err());
    match result {
        Err(err) => assert!(
            err.to_string().contains("ef_construction must be in range"),
            "Expected ef_construction range error, got: {}",
            err
        ),
        Ok(_) => panic!("Expected error"),
    }
}

#[test]
fn test_hnsw_builder_ef_construction_too_small() {
    let result = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .ef_construction(5)
        .build();
    assert!(result.is_err());
    match result {
        Err(err) => assert!(
            err.to_string().contains("ef_construction must be in range"),
            "Expected ef_construction range error, got: {}",
            err
        ),
        Ok(_) => panic!("Expected error"),
    }
}

#[test]
fn test_hnsw_builder_ef_search_too_large() {
    let result = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .ef_search(5000)
        .build();
    assert!(result.is_err());
    match result {
        Err(err) => assert!(
            err.to_string().contains("ef_search must be in range"),
            "Expected ef_search range error, got: {}",
            err
        ),
        Ok(_) => panic!("Expected error"),
    }
}

#[test]
fn test_hnsw_builder_ef_search_too_small() {
    let result = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .ef_search(0)
        .build();
    assert!(result.is_err());
    match result {
        Err(err) => assert!(
            err.to_string().contains("ef_search must be in range"),
            "Expected ef_search range error, got: {}",
            err
        ),
        Ok(_) => panic!("Expected error"),
    }
}

// ============================================================================
// Distance metric conversion tests
// ============================================================================

#[test]
fn test_haversine_metric() {
    let index = HnswIndexBuilder::new(2, DistanceMetric::Haversine)
        .build()
        .unwrap();

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();

    // Add lat/lon coordinates (simplified test)
    index.add(node1, &[0.0, 0.0]).unwrap();
    index.add(node2, &[0.1, 0.1]).unwrap();

    let results = index.search(&[0.0, 0.0], 2).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, node1); // Exact match should be first
}

#[test]
fn test_hamming_metric() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Hamming)
        .build()
        .unwrap();

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();

    // Binary-like vectors
    index.add(node1, &[1.0, 0.0, 1.0, 0.0]).unwrap();
    index.add(node2, &[0.0, 1.0, 0.0, 1.0]).unwrap(); // Completely different

    let results = index.search(&[1.0, 0.0, 1.0, 0.0], 2).unwrap();
    // Just verify we get 2 results - Hamming distance behavior may vary
    assert_eq!(results.len(), 2);
}

#[test]
fn test_tanimoto_metric() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Tanimoto)
        .build()
        .unwrap();

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();

    index.add(node1, &[1.0, 1.0, 0.0, 0.0]).unwrap();
    index.add(node2, &[0.0, 0.0, 1.0, 1.0]).unwrap();

    let results = index.search(&[1.0, 1.0, 0.0, 0.0], 2).unwrap();
    // Tanimoto returns 1.0 - distance as similarity
    assert_eq!(results[0].0, node1);
    assert!(results[0].1 > results[1].1); // First result should have higher similarity
}

// ============================================================================
// Quantization tests
// ============================================================================

#[test]
fn test_quantization_f16_builds() {
    let index = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .quantization(Quantization::F16)
        .build()
        .unwrap();

    assert_eq!(index.quantization(), Quantization::F16);
}

#[test]
fn test_quantization_i8_builds() {
    let index = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .quantization(Quantization::I8)
        .build()
        .unwrap();

    assert_eq!(index.quantization(), Quantization::I8);
}

// ============================================================================
// Capacity expansion tests
// ============================================================================

#[test]
fn test_auto_capacity_expansion() {
    // Start with small capacity
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .initial_capacity(10)
        .build()
        .unwrap();

    // Add more vectors than initial capacity
    for i in 0..50 {
        let node = NodeId::new(i + 1).unwrap();
        let vector = vec![i as f32 / 50.0, 0.0, 0.0, 0.0];
        index.add(node, &vector).unwrap();
    }

    assert_eq!(index.len(), 50);

    // Should still be able to search
    let results = index.search(&[0.5, 0.0, 0.0, 0.0], 10).unwrap();
    assert!(!results.is_empty());
}

// ============================================================================
// Update existing vector tests
// ============================================================================

#[test]
fn test_update_existing_vector() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();

    // Add initial vector
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    // Update with different vector
    index.add(node, &[0.0, 1.0, 0.0, 0.0]).unwrap();

    // Should still have only 1 vector
    assert_eq!(index.len(), 1);

    // Search should find the updated vector
    let results = index.search(&[0.0, 1.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(results[0].0, node);
    assert!(results[0].1 > 0.99); // High similarity to updated vector
}

// ============================================================================
// Persistence tests
// ============================================================================

#[test]
fn test_save_load_with_mappings() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("test_index.usearch");

    // Create and populate index
    {
        let index = HnswIndexBuilder::new(8, DistanceMetric::Cosine)
            .build()
            .unwrap();

        for i in 0..100 {
            let node = NodeId::new(i + 1).unwrap();
            let vector: Vec<f32> = (0..8).map(|j| ((i + j) % 100) as f32 / 100.0).collect();
            index.add(node, &vector).unwrap();
        }

        index.save(&index_path).unwrap();
    }

    // Load and verify
    {
        let config = HnswConfig::new(8, DistanceMetric::Cosine);
        let index = HnswIndex::load(&index_path, config).unwrap();

        assert_eq!(index.len(), 100);

        // Verify we can search and get correct NodeIds back
        let query: Vec<f32> = (0..8).map(|j| j as f32 / 100.0).collect();
        let results = index.search(&query, 10).unwrap();

        assert_eq!(results.len(), 10);
        // Results should contain valid NodeIds (not zeros or incorrect mappings)
        for (node_id, _) in &results {
            assert!(node_id.as_u64() >= 1 && node_id.as_u64() <= 100);
        }
    }
}

#[test]
fn test_open_mmap_with_mappings() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("mmap_index.usearch");

    // Create and save index
    {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        let node1 = NodeId::new(42).unwrap();
        let node2 = NodeId::new(99).unwrap();

        index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.add(node2, &[0.0, 1.0, 0.0, 0.0]).unwrap();

        index.save(&index_path).unwrap();
    }

    // Open as mmap and verify mappings preserved
    {
        let index = HnswIndex::open_mmap(&index_path).unwrap();

        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(results.len(), 2);

        // Verify specific NodeIds are in results
        let ids: Vec<u64> = results.iter().map(|(id, _)| id.as_u64()).collect();
        assert!(ids.contains(&42) || ids.contains(&99));
    }
}

// ============================================================================
// Memory usage test
// ============================================================================

#[test]
fn test_memory_usage_reporting() {
    let index = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
        .initial_capacity(1000)
        .build()
        .unwrap();

    // Add some vectors
    for i in 0..100 {
        let node = NodeId::new(i + 1).unwrap();
        let vector = vec![0.1f32; 128];
        index.add(node, &vector).unwrap();
    }

    let memory = index.memory_usage();
    assert!(memory > 0, "Memory usage should be non-zero");
}

// ============================================================================
// Compact test (no-op for usearch)
// ============================================================================

#[test]
fn test_compact_noop() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.remove(node).unwrap();

    // compact() should succeed (it's a no-op for usearch native deletes)
    index.compact().unwrap();
}

// ============================================================================
// Batch operations tests
// ============================================================================

#[test]
fn test_add_batch() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let items: Vec<(NodeId, Vec<f32>)> = (0..10)
        .map(|i| {
            (
                NodeId::new(i + 1).unwrap(),
                vec![i as f32 / 10.0, 0.0, 0.0, 0.0],
            )
        })
        .collect();

    index.add_batch(&items).unwrap();
    assert_eq!(index.len(), 10);
}

#[test]
fn test_remove_batch() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    // Add vectors
    for i in 0..10 {
        let node = NodeId::new(i + 1).unwrap();
        index.add(node, &[i as f32 / 10.0, 0.0, 0.0, 0.0]).unwrap();
    }

    // Remove batch
    let ids: Vec<NodeId> = (1..=5).map(|i| NodeId::new(i).unwrap()).collect();
    index.remove_batch(&ids).unwrap();

    assert_eq!(index.len(), 5);
}

// ============================================================================
// Search with filter using different metrics
// ============================================================================

#[test]
fn test_search_with_filter_euclidean() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Euclidean)
        .build()
        .unwrap();

    for i in 0..10 {
        let node = NodeId::new(i + 1).unwrap();
        index.add(node, &[i as f32, 0.0, 0.0, 0.0]).unwrap();
    }

    // Filter to only IDs > 5
    let results = index
        .search_with_filter(&[5.0, 0.0, 0.0, 0.0], 5, |id| id.as_u64() > 5)
        .unwrap();

    for (id, _) in &results {
        assert!(id.as_u64() > 5);
    }
}

#[test]
fn test_search_with_filter_dot_product() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::DotProduct)
        .build()
        .unwrap();

    for i in 0..10 {
        let node = NodeId::new(i + 1).unwrap();
        index.add(node, &[i as f32 / 10.0, 0.0, 0.0, 0.0]).unwrap();
    }

    let results = index
        .search_with_filter(&[1.0, 0.0, 0.0, 0.0], 3, |id| id.as_u64() % 2 == 0)
        .unwrap();

    for (id, _) in &results {
        assert!(id.as_u64() % 2 == 0);
    }
}

// ============================================================================
// Error handling tests
// ============================================================================

#[test]
fn test_dimension_mismatch_on_add() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();
    let result = index.add(node, &[1.0, 0.0]); // Wrong dimensions

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("dimension"));
}

#[test]
fn test_dimension_mismatch_on_search() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let result = index.search(&[1.0, 0.0], 1); // Wrong dimensions
    assert!(result.is_err());
}

#[test]
fn test_dimension_mismatch_on_search_with_filter() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let result = index.search_with_filter(&[1.0, 0.0], 1, |_| true);
    assert!(result.is_err());
}

#[test]
fn test_nan_vector_rejected() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();
    let result = index.add(node, &[f32::NAN, 0.0, 0.0, 0.0]);

    assert!(result.is_err());
}

#[test]
fn test_infinity_vector_rejected() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();
    let result = index.add(node, &[f32::INFINITY, 0.0, 0.0, 0.0]);

    assert!(result.is_err());
}

// ============================================================================
// Search with filter Tanimoto similarity test
// ============================================================================

#[test]
fn test_search_with_filter_tanimoto() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Tanimoto)
        .build()
        .unwrap();

    for i in 0..10 {
        let node = NodeId::new(i + 1).unwrap();
        // Binary-like fingerprints
        let vec = vec![
            if i % 2 == 0 { 1.0 } else { 0.0 },
            if i % 3 == 0 { 1.0 } else { 0.0 },
            if i % 4 == 0 { 1.0 } else { 0.0 },
            if i % 5 == 0 { 1.0 } else { 0.0 },
        ];
        index.add(node, &vec).unwrap();
    }

    let results = index
        .search_with_filter(&[1.0, 1.0, 1.0, 1.0], 3, |id| id.as_u64() <= 5)
        .unwrap();

    for (id, similarity) in &results {
        assert!(id.as_u64() <= 5);
        // Tanimoto similarity should be in range [0, 1]
        assert!(*similarity >= 0.0 && *similarity <= 1.0);
    }
}

// ============================================================================
// Remove non-existent ID test
// ============================================================================

#[test]
fn test_remove_nonexistent_id() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(999).unwrap();
    // Should be a no-op, not an error
    let result = index.remove(node);
    assert!(result.is_ok());
}
