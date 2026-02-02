use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn test_deadlock_save_vs_update() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("deadlock_test");

    let index = Arc::new(HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap());

    // Add an initial node
    let node_id = NodeId::new(1).unwrap();
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let index_clone1 = index.clone();
    let path_clone = path.clone();

    // Thread A: Continuously saves the index
    // This locks `inner` (read), then `id_mapping` (bucket read)
    let handle_save = thread::spawn(move || {
        for _ in 0..100 {
            // Short sleep to increase interleaving chances
            // thread::sleep(Duration::from_millis(1));
            let _ = index_clone1.save(&path_clone);
        }
    });

    let index_clone2 = index.clone();
    // Thread B: Continuously updates the node
    // This locks `id_mapping` (bucket write/entry), then `inner` (write)
    let handle_update = thread::spawn(move || {
        for i in 0..1000 {
             // Short sleep to increase interleaving chances
            // thread::sleep(Duration::from_millis(1));
            // Toggle vector to force updates
            let val = if i % 2 == 0 { 1.0 } else { 0.0 };
            let _ = index_clone2.add(node_id, &[val, 1.0-val, 0.0, 0.0]);
        }
    });

    // We expect this to deadlock.
    // If it deadlocks, the test runner will eventually timeout (default 60s).
    // To make it fail faster for demonstration, we could use a channel/timeout logic.

    handle_save.join().unwrap();
    handle_update.join().unwrap();
}
