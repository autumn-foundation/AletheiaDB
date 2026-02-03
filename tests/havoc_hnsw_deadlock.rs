use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

use gallifreydb::core::id::NodeId;

/// 👺 HAVOC DEADLOCK REPRODUCTION
///
/// Triggers a deadlock between `add` (Occupied path) and `save`.
///
/// Lock Inversion:
/// - Thread 1 (`add`): Acquires Shard Lock (via entry) -> Waits for Inner Write Lock
/// - Thread 2 (`save`): Acquires Inner Read Lock -> Waits for Shard Lock (via iter)
///
/// To detonate:
/// `cargo test --test havoc_hnsw_deadlock -- --ignored --nocapture`
#[test]
#[ignore = "This test intentionally deadlocks. Run manually to verify fragility."]
fn havoc_deadlock_add_vs_save() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("havoc_deadlock.index");

    // Create index
    let index = Arc::new(
        HnswIndexBuilder::new(128, DistanceMetric::Cosine)
            .build()
            .expect("Failed to build index"),
    );

    let node_id = NodeId::new(1).unwrap();
    let vector = vec![1.0; 128];

    // Add initial node so that subsequent adds hit the 'Occupied' path
    index.add(node_id, &vector).expect("Initial add failed");

    let index_clone1 = index.clone();
    let index_clone2 = index.clone();
    let path_clone = index_path.clone();

    let barrier = Arc::new(Barrier::new(3));
    let b1 = barrier.clone();
    let b2 = barrier.clone();

    println!("Starting deadlock torture test...");

    // Thread 1: Updates the node (Occupied path)
    // Lock Order: Shard Lock (via entry) -> Inner Write Lock
    let t1 = thread::spawn(move || {
        b1.wait();
        for i in 0..10000 {
            let vec = vec![i as f32; 128];
            // This hits the Occupied path because node_id exists
            index_clone1.add(node_id, &vec).unwrap();
            // Busy loop to keep pressure high
        }
    });

    // Thread 2: Saves the index
    // Lock Order: Inner Read Lock -> Shard Lock (via iter)
    let t2 = thread::spawn(move || {
        b2.wait();
        for i in 0..100 {
            let p = path_clone.with_extension(format!("{}.index", i));
            // This calls save(), which acquires inner.read() then iterates id_mapping
            let _ = index_clone2.save(&p);
            // Yield slightly to allow T1 to get locks
            thread::sleep(Duration::from_millis(1));
        }
    });

    // Wait for threads to start
    barrier.wait();

    println!("Threads running. If this hangs, we successfully deadlocked.");

    // Join threads
    t1.join().unwrap();
    t2.join().unwrap();

    println!("Test finished without deadlock (Unlucky timing?)");
}
