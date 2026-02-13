use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::{Arc, Barrier};
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

// Increase thread count to force contention
const NUM_UPDATERS: usize = 32;
const NUM_INSERTERS: usize = 32;
const DURATION_SECS: u64 = 10;

// We use a small number of keys to maximize collision probability if DashMap
// uses a small number of shards (default is often 64 or related to core count).
const NUM_INITIAL_NODES: u64 = 100;

#[test]
fn havoc_deadlock_repro() {
    // 1. Setup Index
    let index = HnswIndexBuilder::new(384, DistanceMetric::Cosine)
        .m(16)
        .ef_construction(100)
        .build()
        .expect("Failed to build index");
    let index = Arc::new(index);

    // 2. Pre-populate
    for i in 0..NUM_INITIAL_NODES {
        let id = NodeId::new(i + 1).unwrap();
        let vec = vec![0.1f32; 384];
        index.add(id, &vec).unwrap();
    }

    // 3. Spawn Threads
    let barrier = Arc::new(Barrier::new(NUM_UPDATERS + NUM_INSERTERS));
    let mut handles = vec![];

    // Updaters (Occupied path)
    // They lock Shard(existing) -> Inner
    for t in 0..NUM_UPDATERS {
        let index = index.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let start = std::time::Instant::now();
            let mut i = 0;
            while start.elapsed().as_secs() < DURATION_SECS {
                let id_val = (t as u64 * 10 + i) % NUM_INITIAL_NODES + 1;
                let id = NodeId::new(id_val).unwrap();
                let vec = vec![0.2f32; 384];
                // This hits the Occupied path because the node exists
                index.add(id, &vec).unwrap();
                i += 1;
                if i % 100 == 0 {
                    thread::yield_now();
                }
            }
        }));
    }

    // Inserters (Vacant path)
    // They lock Inner -> Shard(new)
    for t in 0..NUM_INSERTERS {
        let index = index.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let start = std::time::Instant::now();
            let mut i = 0;
            while start.elapsed().as_secs() < DURATION_SECS {
                // Use IDs outside the initial range to ensure Vacant path
                let id_val = NUM_INITIAL_NODES + 1000 + (t as u64 * 100000) + i;
                let id = NodeId::new(id_val).unwrap();
                let vec = vec![0.3f32; 384];

                // Add (Vacant path)
                index.add(id, &vec).unwrap();
                i += 1;
                if i % 100 == 0 {
                    thread::yield_now();
                }
            }
        }));
    }

    // 4. Wait for threads
    for handle in handles {
        handle.join().unwrap();
    }
}
