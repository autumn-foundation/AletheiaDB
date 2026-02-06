//! Edge case tests for additional code coverage.

use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{
    DistanceMetric, HnswConfig, HnswIndex, HnswIndexBuilder, Quantization, StorageMode, VectorIndex,
};
use std::path::PathBuf;
use tempfile::tempdir;

// ============================================================================
// VectorIndex default implementations via HnswIndex
// ============================================================================

#[test]
fn test_is_empty() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    assert!(index.is_empty());

    let node = NodeId::new(1).unwrap();
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    assert!(!index.is_empty());
}

// ============================================================================
// Memory-mapped storage tests
// ============================================================================

#[test]
fn test_memory_mapped_storage_mode_build() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("mmap_test.usearch");

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .storage(StorageMode::MemoryMapped { path: index_path })
        .build()
        .unwrap();

    // Should work like a normal index
    let node = NodeId::new(1).unwrap();
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    assert_eq!(index.len(), 1);
}

// ============================================================================
// HnswConfig with storage tests
// ============================================================================

#[test]
fn test_hnsw_config_with_storage_memory_mapped() {
    let config =
        HnswConfig::new(64, DistanceMetric::Cosine).with_storage(StorageMode::MemoryMapped {
            path: PathBuf::from("/tmp/test"),
        });

    match config.storage {
        StorageMode::MemoryMapped { path } => {
            assert_eq!(path, PathBuf::from("/tmp/test"));
        }
        _ => panic!("Expected MemoryMapped storage"),
    }
}

// ============================================================================
// Search on empty index
// ============================================================================

#[test]
fn test_search_empty_index() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 10).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_search_with_filter_empty_index() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let results = index
        .search_with_filter(&[1.0, 0.0, 0.0, 0.0], 10, |_| true)
        .unwrap();
    assert!(results.is_empty());
}

// ============================================================================
// k=0 and large k tests
// ============================================================================

#[test]
fn test_search_k_zero() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 0).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_search_k_larger_than_index() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    for i in 0..5 {
        let node = NodeId::new(i + 1).unwrap();
        index.add(node, &[i as f32, 0.0, 0.0, 0.0]).unwrap();
    }

    // Request more results than vectors in index
    let results = index.search(&[0.0, 0.0, 0.0, 0.0], 100).unwrap();
    assert_eq!(results.len(), 5);
}

// ============================================================================
// Filter that matches nothing
// ============================================================================

#[test]
fn test_search_with_filter_no_matches() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    for i in 0..10 {
        let node = NodeId::new(i + 1).unwrap();
        index.add(node, &[i as f32, 0.0, 0.0, 0.0]).unwrap();
    }

    // Filter that matches nothing
    let results = index
        .search_with_filter(&[0.0, 0.0, 0.0, 0.0], 10, |_| false)
        .unwrap();
    assert!(results.is_empty());
}

// ============================================================================
// Index with capacity = 0 (uses default)
// ============================================================================

#[test]
fn test_build_with_zero_capacity_uses_default() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .initial_capacity(0)
        .build()
        .unwrap();

    // Should still work with default capacity
    for i in 0..100 {
        let node = NodeId::new(i + 1).unwrap();
        index.add(node, &[i as f32, 0.0, 0.0, 0.0]).unwrap();
    }

    assert_eq!(index.len(), 100);
}

// ============================================================================
// Different distance metrics in search results
// ============================================================================

#[test]
fn test_dot_product_similarity_values() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::DotProduct)
        .build()
        .unwrap();

    let n1 = NodeId::new(1).unwrap();
    let n2 = NodeId::new(2).unwrap();

    index.add(n1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.add(n2, &[0.5, 0.5, 0.0, 0.0]).unwrap();

    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();

    // DotProduct returns -distance as similarity
    // So results should be sorted by descending similarity (ascending distance)
    assert_eq!(results.len(), 2);
}

#[test]
fn test_euclidean_similarity_values() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Euclidean)
        .build()
        .unwrap();

    let n1 = NodeId::new(1).unwrap();
    let n2 = NodeId::new(2).unwrap();
    let n3 = NodeId::new(3).unwrap();

    index.add(n1, &[0.0, 0.0, 0.0, 0.0]).unwrap(); // Closest to query
    index.add(n2, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.add(n3, &[2.0, 0.0, 0.0, 0.0]).unwrap(); // Farthest

    let results = index.search(&[0.0, 0.0, 0.0, 0.0], 3).unwrap();

    assert_eq!(results.len(), 3);
    assert_eq!(results[0].0, n1); // Closest first
    // Euclidean returns -distance, so higher (less negative) = more similar
    assert!(results[0].1 >= results[1].1);
    assert!(results[1].1 >= results[2].1);
}

// ============================================================================
// Load/save error scenarios would require path manipulation
// but we test the happy paths thoroughly
// ============================================================================

#[test]
fn test_save_creates_mappings_file() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("save_test.usearch");
    let mappings_path = dir.path().join("save_test.usearch.mappings");

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    for i in 0..10 {
        let node = NodeId::new(i + 1).unwrap();
        index.add(node, &[i as f32, 0.0, 0.0, 0.0]).unwrap();
    }

    index.save(&index_path).unwrap();

    // Verify mappings file was created
    assert!(mappings_path.exists());
}

// ============================================================================
// Concurrent update of same node (tests occupied entry path)
// ============================================================================

#[test]
fn test_concurrent_update_same_node() {
    use std::sync::Arc;
    use std::thread;

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(100)
            .build()
            .unwrap(),
    );

    let node = NodeId::new(1).unwrap();

    // Initial add
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    // Spawn multiple threads that all try to update the same node
    let mut handles = vec![];
    for i in 0..10 {
        let index = Arc::clone(&index);
        let handle = thread::spawn(move || {
            let vec = vec![i as f32 / 10.0, 0.0, 0.0, 0.0];
            index.add(node, &vec).unwrap();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Should still have only 1 vector
    assert_eq!(index.len(), 1);
}

// ============================================================================
// Load without mappings file (empty mappings case)
// ============================================================================

#[test]
fn test_load_without_mappings_file() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("no_mappings.usearch");

    // Create and save an index
    {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        let node = NodeId::new(1).unwrap();
        index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.save(&index_path).unwrap();
    }

    // Delete the mappings file
    let mappings_path = dir.path().join("no_mappings.usearch.mappings");
    std::fs::remove_file(&mappings_path).unwrap();

    // Load should succeed but without mappings
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    let index = HnswIndex::load(&index_path, config).unwrap();

    // Index has the vector but we can't map to NodeIds
    assert_eq!(index.len(), 1);

    // Search will return empty because reverse_mapping is empty
    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 10).unwrap();
    assert!(results.is_empty()); // No mappings, so no results can be returned
}

// ============================================================================
// HnswConfig default values
// ============================================================================

#[test]
fn test_hnsw_config_default_values() {
    let config = HnswConfig::default();

    assert_eq!(config.dimensions, 0);
    assert_eq!(config.metric, DistanceMetric::Cosine);
    assert_eq!(config.m, 16);
    assert_eq!(config.ef_construction, 128);
    assert_eq!(config.ef_search, 64);
    assert_eq!(config.capacity, 0);
    assert_eq!(config.quantization, Quantization::F32);
    assert!(matches!(config.storage, StorageMode::InMemory));
    assert!(config.custom_metric.is_none());
}

// ============================================================================
// HnswIndexBuilder from_config
// ============================================================================

#[test]
fn test_builder_from_config() {
    let config = HnswConfig::new(256, DistanceMetric::Euclidean)
        .with_m(32)
        .with_ef_construction(256)
        .with_ef_search(128);

    let index = HnswIndexBuilder::from_config(&config).build().unwrap();

    assert_eq!(index.dimensions(), 256);
    assert_eq!(index.distance_metric(), DistanceMetric::Euclidean);
    assert_eq!(index.m(), 32);
}

// ============================================================================
// Quantization memory_usage
// ============================================================================

#[test]
fn test_f16_memory_usage() {
    let index = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .quantization(Quantization::F16)
        .initial_capacity(100)
        .build()
        .unwrap();

    for i in 0..50 {
        let node = NodeId::new(i + 1).unwrap();
        index.add(node, &vec![0.1f32; 64]).unwrap();
    }

    let memory = index.memory_usage();
    assert!(memory > 0);
}

#[test]
fn test_i8_memory_usage() {
    let index = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .quantization(Quantization::I8)
        .initial_capacity(100)
        .build()
        .unwrap();

    for i in 0..50 {
        let node = NodeId::new(i + 1).unwrap();
        index.add(node, &vec![0.1f32; 64]).unwrap();
    }

    let memory = index.memory_usage();
    assert!(memory > 0);
}

// ============================================================================
// Search with Haversine on filter
// ============================================================================

#[test]
fn test_search_with_filter_haversine() {
    let index = HnswIndexBuilder::new(2, DistanceMetric::Haversine)
        .build()
        .unwrap();

    for i in 0..10 {
        let node = NodeId::new(i + 1).unwrap();
        // Simulated lat/lon
        index
            .add(node, &[(i as f32) * 0.01, (i as f32) * 0.01])
            .unwrap();
    }

    let results = index
        .search_with_filter(&[0.0, 0.0], 5, |id| id.as_u64() <= 5)
        .unwrap();

    for (id, _) in &results {
        assert!(id.as_u64() <= 5);
    }
}

// ============================================================================
// Search with Hamming on filter
// ============================================================================

#[test]
fn test_search_with_filter_hamming() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Hamming)
        .build()
        .unwrap();

    for i in 0..10 {
        let node = NodeId::new(i + 1).unwrap();
        let vec = vec![
            if i % 2 == 0 { 1.0 } else { 0.0 },
            if i % 3 == 0 { 1.0 } else { 0.0 },
            if i % 4 == 0 { 1.0 } else { 0.0 },
            if i % 5 == 0 { 1.0 } else { 0.0 },
        ];
        index.add(node, &vec).unwrap();
    }

    let results = index
        .search_with_filter(&[1.0, 1.0, 1.0, 1.0], 3, |id| id.as_u64() % 2 == 0)
        .unwrap();

    for (id, _) in &results {
        assert!(id.as_u64() % 2 == 0);
    }
}

// ============================================================================
// Memory-mapped index write protection
// ============================================================================

#[test]
fn test_mmap_index_rejects_add() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("mmap_readonly.usearch");

    // Create and save an index
    {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        let node = NodeId::new(1).unwrap();
        index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.save(&index_path).unwrap();
    }

    // Open as mmap and try to add (should fail)
    let mmap_index = HnswIndex::open_mmap(&index_path).unwrap();
    let result = mmap_index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0]);

    assert!(result.is_err());
    let err_str = format!("{:?}", result.unwrap_err());
    assert!(err_str.contains("read-only") || err_str.contains("memory-mapped"));
}

#[test]
fn test_mmap_index_rejects_remove() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("mmap_readonly2.usearch");

    // Create and save an index
    {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        let node = NodeId::new(1).unwrap();
        index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.save(&index_path).unwrap();
    }

    // Open as mmap and try to remove (should fail)
    let mmap_index = HnswIndex::open_mmap(&index_path).unwrap();
    let result = mmap_index.remove(NodeId::new(1).unwrap());

    assert!(result.is_err());
    let err_str = format!("{:?}", result.unwrap_err());
    assert!(err_str.contains("read-only") || err_str.contains("memory-mapped"));
}

// ============================================================================
// Mapping file integrity checks
// ============================================================================

#[test]
fn test_mapping_file_has_magic_header() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("magic_header.usearch");
    let mappings_path = dir.path().join("magic_header.usearch.mappings");

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.save(&index_path).unwrap();

    // Verify mappings file starts with magic bytes "GMAP"
    let data = std::fs::read(&mappings_path).unwrap();
    assert!(data.len() >= 4);
    assert_eq!(&data[0..4], b"GMAP");
}

#[test]
fn test_load_rejects_corrupted_mapping_crc() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("corrupted_crc.usearch");
    let mappings_path = dir.path().join("corrupted_crc.usearch.mappings");

    // Create and save an index
    {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        let node = NodeId::new(1).unwrap();
        index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.save(&index_path).unwrap();
    }

    // Corrupt the CRC (last 4 bytes)
    let mut data = std::fs::read(&mappings_path).unwrap();
    let len = data.len();
    data[len - 1] ^= 0xFF; // Flip bits in CRC
    std::fs::write(&mappings_path, &data).unwrap();

    // Load should fail with CRC error
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    let result = HnswIndex::load(&index_path, config);

    match result {
        Ok(_) => panic!("Expected error for corrupted CRC"),
        Err(e) => {
            let err_str = format!("{:?}", e);
            assert!(
                err_str.contains("CRC") || err_str.contains("corrupted"),
                "Unexpected error: {}",
                err_str
            );
        }
    }
}

#[test]
fn test_load_rejects_bad_magic_bytes() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("bad_magic.usearch");
    let mappings_path = dir.path().join("bad_magic.usearch.mappings");

    // Create and save an index
    {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        let node = NodeId::new(1).unwrap();
        index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.save(&index_path).unwrap();
    }

    // Corrupt the magic bytes
    let mut data = std::fs::read(&mappings_path).unwrap();
    data[0] = b'X';
    data[1] = b'Y';
    std::fs::write(&mappings_path, &data).unwrap();

    // Load should fail with magic error
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    let result = HnswIndex::load(&index_path, config);

    match result {
        Ok(_) => panic!("Expected error for bad magic bytes"),
        Err(e) => {
            let err_str = format!("{:?}", e);
            assert!(
                err_str.contains("magic") || err_str.contains("Invalid"),
                "Unexpected error: {}",
                err_str
            );
        }
    }
}
