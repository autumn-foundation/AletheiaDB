use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use aletheiadb::utils::error::Error;
use std::sync::Arc;
use std::thread;

// Test to improve code coverage for edge cases in HnswIndex

#[test]
fn test_havoc_add_contention_coverage() {
    // This test forces the "Vacant -> Occupied" retry loop in add().
    // Multiple threads race to add the SAME node ID.
    // 1. All threads check id_mapping -> Vacant.
    // 2. Thread A acquires inner lock, adds, inserts to mapping, releases.
    // 3. Thread B acquires inner lock, checks entry -> Occupied! -> continue loop.
    // 4. Thread B loops, checks id_mapping -> Occupied.
    // 5. Thread B enters "Occupied" path logic.

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );
    let node_id = NodeId::new(1).unwrap();
    let vector = vec![1.0, 0.0, 0.0, 0.0];

    let num_threads = 50;
    let mut handles = vec![];

    // Barrier to synchronize start for maximum contention
    let barrier = Arc::new(std::sync::Barrier::new(num_threads));

    for _ in 0..num_threads {
        let index_clone = Arc::clone(&index);
        let vector_clone = vector.clone();
        let barrier_clone = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            barrier_clone.wait();
            // Everyone tries to add the same node
            index_clone.add(node_id, &vector_clone).unwrap();
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(index.len(), 1);
}

#[test]
fn test_havoc_save_index_failure_coverage() {
    // Test failure path when saving index
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();
    let node_id = NodeId::new(1).unwrap();
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    // Try to save to a path in a non-existent directory
    // This should fail at index.save() step
    let path = std::path::Path::new("/non_existent_directory_xyz/test.index");
    let result = index.save(path);

    assert!(result.is_err());
    match result {
        Err(Error::Vector(aletheiadb::utils::error::VectorError::IndexError(msg))) => {
            // Verify it's the expected error message from save_internal
            assert!(msg.contains("Failed to save index"));
        }
        _ => panic!("Expected IndexError, got {:?}", result),
    }
}

#[test]
fn test_havoc_save_mappings_failure_coverage() {
    // Test failure path when creating mappings file
    // We want index.save() to succeed, but File::create(mappings_path) to fail.

    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("test_fail_map.index");

    // Create the mappings path as a DIRECTORY.
    // This will cause File::create() to fail with "Is a directory" (EISDIR) on Unix.
    let mappings_path = index_path.with_extension("usearch.mappings");
    std::fs::create_dir(&mappings_path).unwrap();

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();
    let node_id = NodeId::new(1).unwrap();
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let result = index.save(&index_path);

    assert!(result.is_err());
    match result {
        Err(Error::Vector(aletheiadb::utils::error::VectorError::IndexError(msg))) => {
            assert!(msg.contains("Failed to create mappings file"));
        }
        _ => panic!("Expected IndexError, got {:?}", result),
    }
}
