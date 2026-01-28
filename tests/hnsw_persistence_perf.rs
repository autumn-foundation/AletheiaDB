use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::hnsw::HnswConfig;
use gallifreydb::index::vector::hnsw::HnswIndex;
use gallifreydb::index::vector::{DistanceMetric, VectorIndex};
use std::sync::Arc;
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
        let vector: Vec<f32> = (0..dimensions)
            .map(|x| (x as f32) / (dimensions as f32))
            .collect();
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
    assert!(
        file_path.with_extension("usearch.mappings").exists(),
        "Mappings file should exist"
    );

    // 6. Verify loading works (validity check)
    let loaded_index = HnswIndex::load(&file_path, index.config()).expect("Failed to load index");
    assert_eq!(
        loaded_index.len(),
        count,
        "Loaded index should have same count"
    );
}

#[cfg(any(feature = "tokio", feature = "embeddings"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_save_in_tokio_context() {
    let dimensions = 128;
    let count = 500;

    // 1. Setup index
    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(count);
    let index = Arc::new(HnswIndex::new(config).expect("Failed to create index"));

    // 2. Populate with data
    for i in 0..count {
        let node_id = NodeId::new(i as u64 + 1).unwrap();
        let vector: Vec<f32> = (0..dimensions)
            .map(|x| (x as f32) / (dimensions as f32))
            .collect();
        index.add(node_id, &vector).expect("Failed to add vector");
    }

    // 3. Prepare temp directory
    let dir = tempdir().expect("Failed to create temp dir");
    let file_path = dir.path().join("test_index_async.usearch");

    // 4. Verify concurrent execution
    // Spawn a background task that sleeps briefly to simulate other work
    // If save() blocks the thread, this task might be delayed if both run on same thread
    // but in multi-thread runtime, they should run in parallel if block_in_place works
    let start = Instant::now();

    let index_clone = Arc::clone(&index);
    let path_clone = file_path.clone();

    let save_handle = tokio::spawn(async move {
        // This will block the thread if block_in_place is not working
        index_clone.save(&path_clone).expect("Failed to save index");
    });

    // Do some "concurrent" work
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    save_handle.await.expect("Save task failed");
    let duration = start.elapsed();

    println!("Async context save operation took: {:?}", duration);

    // 5. Verify files
    assert!(file_path.exists());
    assert!(file_path.with_extension("usearch.mappings").exists());
}

#[test]
fn test_save_fallback_no_runtime() {
    // Explicitly test the synchronous fallback path when no runtime is present
    // This runs in a standard thread, ensuring coverage for the !tokio path logic
    let dimensions = 128;
    let count = 100;

    // 1. Setup index
    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(count);
    let index = HnswIndex::new(config).expect("Failed to create index");

    // 2. Populate with data
    for i in 0..count {
        let node_id = NodeId::new(i as u64 + 1).unwrap();
        let vector: Vec<f32> = (0..dimensions)
            .map(|x| (x as f32) / (dimensions as f32))
            .collect();
        index.add(node_id, &vector).expect("Failed to add vector");
    }

    // 3. Prepare temp directory
    let dir = tempdir().expect("Failed to create temp dir");
    let file_path = dir.path().join("test_index_no_runtime.usearch");

    // 4. Save - should work synchronously
    index.save(&file_path).expect("Failed to save index");

    // 5. Verify files
    assert!(file_path.exists());
    assert!(file_path.with_extension("usearch.mappings").exists());
}

#[cfg(any(feature = "tokio", feature = "embeddings"))]
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
        let vector: Vec<f32> = (0..dimensions)
            .map(|x| (x as f32) / (dimensions as f32))
            .collect();
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
