use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_havoc_deadlock_scenario() {
    let (tx, rx) = std::sync::mpsc::channel();

    thread::spawn(move || {
        run_deadlock_scenario();
        tx.send(()).unwrap();
    });

    if rx.recv_timeout(Duration::from_secs(20)).is_err() {
        panic!("Deadlock detected: havoc_concurrency scenario did not complete in time");
    }
}

fn run_deadlock_scenario() {
    let index = Arc::new(
        HnswIndexBuilder::new(128, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    let start_time = std::time::Instant::now();
    let duration = Duration::from_secs(5); // Run for 5 seconds

    // Thread A: Adds/updates a single node repeatedly (Occupied path)
    let index_clone_a = Arc::clone(&index);
    let handle_a = thread::spawn(move || {
        let node_id = NodeId::new(1).unwrap();
        let vector = vec![0.0f32; 128];
        // Ensure initial add is done so we hit Occupied path
        index_clone_a.add(node_id, &vector).unwrap();

        while start_time.elapsed() < duration {
            // This triggers the Occupied path because we use the same ID repeatedly
            index_clone_a.add(node_id, &vector).unwrap();
        }
    });

    // Thread B: Searches repeatedly.
    let index_clone_b = Arc::clone(&index);
    let handle_b = thread::spawn(move || {
        let vector = vec![0.0f32; 128];
        while start_time.elapsed() < duration {
            let _ = index_clone_b.search(&vector, 10);
        }
    });

    // Thread C: Saves repeatedly.
    let index_clone_c = Arc::clone(&index);
    let handle_c = thread::spawn(move || {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.usearch");
        while start_time.elapsed() < duration {
            let _ = index_clone_c.save(&path);
            thread::sleep(Duration::from_millis(10)); // Slight delay
        }
    });

    // Thread D: Filtered search.
    let index_clone_d = Arc::clone(&index);
    let handle_d = thread::spawn(move || {
        let vector = vec![0.0f32; 128];
        while start_time.elapsed() < duration {
            let _ = index_clone_d.search_with_filter(&vector, 10, |_| true);
        }
    });

    // Wait for threads
    handle_a.join().unwrap();
    handle_b.join().unwrap();
    handle_c.join().unwrap();
    handle_d.join().unwrap();
}
