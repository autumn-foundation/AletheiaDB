use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

#[test]
fn test_remove_timeout_during_search() {
    // Set short timeout for testing
    aletheiadb::index::vector::hnsw::set_lock_timeout_internal(Duration::from_millis(100));

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    // Add a node so remove has something to do
    let id = NodeId::new(1).unwrap();
    index.add(id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let index_clone = index.clone();
    let barrier_clone = barrier.clone();

    thread::spawn(move || {
        // Search holds Read lock.
        // We block inside the callback.
        let _ = index_clone.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 1, |_| {
            barrier_clone.wait(); // Signal main thread we are holding lock
            thread::sleep(Duration::from_millis(500)); // Hold lock longer than 100ms timeout
            true
        });
    });

    barrier.wait(); // Wait for search to start

    // Attempt remove. Should fail with timeout because Search holds Read lock.
    let result = index.remove(id);
    assert!(result.is_err(), "Remove should have failed with timeout");

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("timeout"),
        "Error should be timeout, got: {}",
        err
    );
}

#[test]
fn test_add_timeout_during_search() {
    // Set short timeout for testing
    aletheiadb::index::vector::hnsw::set_lock_timeout_internal(Duration::from_millis(100));

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );
    let id = NodeId::new(1).unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let index_clone = index.clone();
    let barrier_clone = barrier.clone();

    thread::spawn(move || {
        // Search holds Read lock
        let _ = index_clone.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 1, |_| {
            barrier_clone.wait();
            thread::sleep(Duration::from_millis(500));
            true
        });
    });

    barrier.wait();

    // Attempt add. Should fail with timeout.
    let result = index.add(id, &[1.0, 0.0, 0.0, 0.0]);
    assert!(result.is_err(), "Add should have failed with timeout");

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("timeout"),
        "Error should be timeout, got: {}",
        err
    );
}
