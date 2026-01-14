//! Tests for memory-mapped index persistence.

use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::{DistanceMetric, HnswConfig, HnswIndex, HnswIndexBuilder, StorageMode, VectorIndex};
use tempfile::TempDir;

/// Test save and load roundtrip.
#[test]
fn test_save_load_roundtrip() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("test_index.usearch");

    // Create and populate index
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();

    index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.add(node2, &[0.0, 1.0, 0.0, 0.0]).unwrap();

    // Save
    index.save(&index_path).unwrap();

    // Load into new index
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    let loaded = HnswIndex::load(&index_path, config).unwrap();

    // Verify data
    assert_eq!(loaded.len(), 2);

    let results = loaded.search(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();
    assert!(!results.is_empty());
}

/// Test memory-mapped index creation and query.
#[test]
fn test_mmap_index_query() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("mmap_index.usearch");

    // Create memory-mapped index
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .storage(StorageMode::MemoryMapped { path: index_path.clone() })
        .build()
        .unwrap();

    // Add vectors
    for i in 1..=100 {
        let node = NodeId::new(i).unwrap();
        let vec = vec![i as f32, 0.0, 0.0, 0.0];
        index.add(node, &vec).unwrap();
    }

    // Query
    let results = index.search(&[50.0, 0.0, 0.0, 0.0], 5).unwrap();
    assert_eq!(results.len(), 5);

    // Verify file exists
    assert!(index_path.exists());
}

/// Test opening existing memory-mapped index.
#[test]
fn test_open_mmap_index() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("existing_mmap.usearch");

    // Create and save index
    {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0]).unwrap();

        index.save(&index_path).unwrap();
    }

    // Open as memory-mapped
    let mmap_index = HnswIndex::open_mmap(&index_path).unwrap();

    // Should be able to query
    let results = mmap_index.search(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();
    assert!(!results.is_empty());
}
