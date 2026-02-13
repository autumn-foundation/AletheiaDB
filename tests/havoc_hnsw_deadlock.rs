use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

#[test]
fn test_hnsw_reentrant_deadlock_prevented() {
    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    let id1 = NodeId::new(1).unwrap();
    index.add(id1, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let index_clone = index.clone();

    // Test that re-entrant modifications are detected and prevented.
    // Previously, this would cause a deadlock. Now it should return an error.

    let (tx, rx) = std::sync::mpsc::channel();

    thread::spawn(move || {
        println!("Starting search with filter...");

        // This filter attempts to modify the index, which would cause a deadlock
        // if not prevented. With our fix (PR #870), this should return an error.
        let result = index_clone.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 1, |id| {
            println!("Inside filter for {:?}", id);

            // Attempt to modify index from within the filter callback.
            // This should now return an error instead of deadlocking.
            let new_id = NodeId::new(id.as_u64() + 100).unwrap();
            let add_result = index_clone.add(new_id, &[0.0, 1.0, 0.0, 0.0]);

            // Verify that the add operation was rejected with the expected error
            assert!(add_result.is_err(), "Expected add to fail during filter");
            let err_msg = add_result.unwrap_err().to_string();
            assert!(
                err_msg.contains("Cannot modify index from within a search_with_filter callback"),
                "Expected re-entrancy error, got: {}",
                err_msg
            );

            true
        });

        println!("Search finished with result: {:?}", result);

        // The search itself should succeed (the filter just checks the add failed)
        assert!(result.is_ok(), "Expected search to succeed: {:?}", result);

        tx.send(()).unwrap();
    });

    // Wait for 2 seconds. The operation should complete quickly now.
    if rx.recv_timeout(Duration::from_secs(2)).is_err() {
        panic!("Test timed out! The re-entrancy prevention may not be working correctly.");
    }
}

#[test]
fn havoc_hnsw_deadlock_repro() {
    // Chaos Test: Concurrent Add (Occupied) vs Add (Vacant) Deadlock
    // Ideally this runs with many iterations to catch the race.

    // Reduce iterations for CI to avoid timeout flakes, but enough to trigger race locally
    let _iterations = 1000;
    let num_threads = 16;

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = vec![];

    // Thread Group A: Repeatedly update the SAME ID (Occupied path)
    // This holds the shard lock for ID 1 continuously.
    for i in 0..num_threads {
        let index = index.clone();
        let stop = stop.clone();
        handles.push(thread::spawn(move || {
            let id = NodeId::new(1).unwrap();
            let mut vec = vec![1.0, 0.0, 0.0, 0.0];
            while !stop.load(Ordering::Relaxed) {
                // Mutate vector slightly to force updates
                vec[0] = (i as f32) * 0.1;
                if let Err(e) = index.add(id, &vec) {
                    eprintln!("Update failed: {}", e);
                }
                // Yield to increase contention
                thread::yield_now();
            }
        }));
    }

    // Thread Group B: Add NEW IDs (Vacant path)
    // This holds the INNER lock, then tries to acquire shard lock.
    for i in 0..num_threads {
        let index = index.clone();
        let stop = stop.clone();
        handles.push(thread::spawn(move || {
            let mut id_val = 1000 * (i as u64) + 2;
            let vec = vec![0.0, 1.0, 0.0, 0.0];
            while !stop.load(Ordering::Relaxed) {
                let id = NodeId::new(id_val).unwrap();
                if let Err(e) = index.add(id, &vec) {
                    eprintln!("Add failed: {}", e);
                }
                id_val += 1;
                // Yield to increase contention
                thread::yield_now();
            }
        }));
    }

    // Run for a bit
    thread::sleep(Duration::from_secs(5));
    stop.store(true, Ordering::Relaxed);

    for handle in handles {
        handle.join().unwrap();
    }
}
