use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::hnsw::HnswConfig;
use gallifreydb::index::vector::hnsw::HnswIndex;
use gallifreydb::index::vector::{DistanceMetric, VectorIndex};
use std::time::Instant;
use tempfile::tempdir;

#[test]
fn test_save_performance_and_correctness() {
    let dimensions = 128; // Smaller dimensions for faster test
    let count = 1000;

    // 1. Setup index
    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(count);
    let index = HnswIndex::new(config).expect("Failed to create index");

    // 2. Populate with data
    for i in 0..count {
        let node_id = NodeId::new(i as u64 + 1).unwrap();
        let vector: Vec<f32> = (0..dimensions).map(|x| (x as f32) / (dimensions as f32)).collect();
        index.add(node_id, &vector).expect("Failed to add vector");
    }

    // 3. Prepare temp directory
    let dir = tempdir().expect("Failed to create temp dir");
    let file_path = dir.path().join("test_index.usearch");

    // 4. Measure save time
    let start = Instant::now();
    index.save(&file_path).expect("Failed to save index");
    let duration = start.elapsed();

    println!("Save operation took: {:?}", duration);

    // 5. Verify files exist
    assert!(file_path.exists(), "Index file should exist");
    assert!(file_path.with_extension("usearch.mappings").exists(), "Mappings file should exist");

    // 6. Verify loading works (validity check)
    let loaded_index = HnswIndex::load(&file_path, index.config()).expect("Failed to load index");
    assert_eq!(loaded_index.len(), count, "Loaded index should have same count");
}

#[cfg(feature = "tokio")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_save_in_tokio_context() {
    let dimensions = 128;
    let count = 500;

    // 1. Setup index
    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(count);
    let index = HnswIndex::new(config).expect("Failed to create index");

    // 2. Populate with data
    for i in 0..count {
        let node_id = NodeId::new(i as u64 + 1).unwrap();
        let vector: Vec<f32> = (0..dimensions).map(|x| (x as f32) / (dimensions as f32)).collect();
        index.add(node_id, &vector).expect("Failed to add vector");
    }

    // 3. Prepare temp directory
    let dir = tempdir().expect("Failed to create temp dir");
    let file_path = dir.path().join("test_index_async.usearch");

    // 4. Measure save time in async context
    let start = Instant::now();
    // This call is synchronous, but we want to make sure it doesn't panic or fail inside tokio runtime
    // and ideally (after optimization) uses block_in_place
    index.save(&file_path).expect("Failed to save index");
    let duration = start.elapsed();

    println!("Async context save operation took: {:?}", duration);

    // 5. Verify files
    assert!(file_path.exists());
    assert!(file_path.with_extension("usearch.mappings").exists());
}

#[cfg(feature = "tokio")]
#[tokio::test(flavor = "current_thread")]
async fn test_save_in_single_thread_context() {
    let dimensions = 128;
    let count = 100;

    // 1. Setup index
    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(count);
    let index = HnswIndex::new(config).expect("Failed to create index");

    // 2. Populate with data
    for i in 0..count {
        let node_id = NodeId::new(i as u64 + 1).unwrap();
        let vector: Vec<f32> = (0..dimensions).map(|x| (x as f32) / (dimensions as f32)).collect();
        index.add(node_id, &vector).expect("Failed to add vector");
    }

    // 3. Prepare temp directory
    let dir = tempdir().expect("Failed to create temp dir");
    let file_path = dir.path().join("test_index_single_thread.usearch");

    // 4. Measure save time in async context (should fallback to blocking)
    index.save(&file_path).expect("Failed to save index");

    // 5. Verify files
    assert!(file_path.exists());
    assert!(file_path.with_extension("usearch.mappings").exists());
}
