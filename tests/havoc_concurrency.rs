use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::tempdir;

#[test]
fn test_havoc_deadlock_save_add() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("havoc.index");

    // Create an index
    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    let num_threads = 10;
    let barrier = Arc::new(Barrier::new(num_threads + 1)); // +1 for save thread

    let mut handles = vec![];

    // Spawn adder threads
    for i in 0..num_threads {
        let index = index.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait(); // Sync start
            for j in 0..100 {
                let id = NodeId::new((i * 1000 + j) as u64).unwrap();
                let vec = vec![0.1, 0.2, 0.3, 0.4];
                // Spam add
                let _ = index.add(id, &vec);
            }
        }));
    }

    // Spawn saver thread
    let index_clone = index.clone();
    let barrier_clone = barrier.clone();
    let path_clone = path.clone();
    handles.push(thread::spawn(move || {
        barrier_clone.wait(); // Sync start
        for _ in 0..10 {
            // Spam save
            let _ = index_clone.save(&path_clone);
        }
    }));

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    // If we reach here, no deadlock occurred.
}

#[test]
fn test_havoc_race_inconsistency() {
    // This test attempts to create a "Zombie Vector" scenario
    // Threads race to add/remove the same ID.
    // We check if the index state is consistent at the end.
    // INCREASED iterations to ensure coverage of Occupied retry logic.

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    let num_threads = 8;
    let iterations = 5000; // Increased from 1000 to 5000 for coverage
    let target_id = NodeId::new(1).unwrap();
    let barrier = Arc::new(Barrier::new(num_threads));

    let mut handles = vec![];

    for i in 0..num_threads {
        let index = index.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..iterations {
                if i % 2 == 0 {
                    // Even threads add
                    let _ = index.add(target_id, &[1.0, 0.0, 0.0, 0.0]);
                } else {
                    // Odd threads remove
                    let _ = index.remove(target_id);
                }
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Check consistency
    let inner_count = index.len();
    let search_results = index.search(&[1.0, 0.0, 0.0, 0.0], 10).unwrap();
    let found = search_results.iter().any(|(id, _)| *id == target_id);

    if found {
        // If found in search, it must be in inner index
        assert!(inner_count >= 1, "Found via search but index empty?");
    } else {
        if inner_count > 0 {
            panic!(
                "Zombie Vector Detected! Inner count: {}, but ID not found in search.",
                inner_count
            );
        }
    }
}

#[test]
fn test_concurrent_adds_race() {
    // Specifically target the "Vacant" path race condition where multiple threads
    // try to add the SAME new ID simultaneously.
    // Run in a loop to ensure we hit the specific timing window for code coverage.

    let num_iterations = 50; // Repeat race attempt multiple times

    for _ in 0..num_iterations {
        let index = Arc::new(
            HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .build()
                .unwrap(),
        );

        let num_threads = 16; // Higher contention
        let target_id = NodeId::new(100).unwrap();
        let barrier = Arc::new(Barrier::new(num_threads));

        let mut handles = vec![];

        for _ in 0..num_threads {
            let index = index.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                // Try to add the same ID concurrently.
                let _ = index.add(target_id, &[0.5, 0.5, 0.5, 0.5]);
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Verify consistency for this iteration
        assert_eq!(index.len(), 1, "Should have exactly 1 vector");
        let results = index.search(&[0.5, 0.5, 0.5, 0.5], 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, target_id);
    }
}
