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
    // The previous implementation had a deadlock where `add` held DashMap lock then Inner lock,
    // while `save` held Inner lock then DashMap lock.
    // The current implementation is supposed to be fixed (PR #751).
}

#[test]
fn test_havoc_race_inconsistency() {
    // This test attempts to create a "Zombie Vector" scenario
    // Threads race to add/remove the same ID.
    // We check if the index state is consistent at the end.

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    let num_threads = 8;
    let iterations = 1000;
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
    // If the ID is in mapping, it MUST be in inner index.
    // If it's not in mapping, it MUST NOT be in inner index.
    // NOTE: This check depends on internal implementation details which we can't easily access publicly.
    // However, `len()` returns inner size. `get_id_mappings` (crate-private) returns mapping.
    // Since we are in `tests/`, we can only access public API unless we use `#[cfg(test)]` tricks in lib.rs
    // But wait, `HnswIndex` has `len()` which delegates to `inner.read().size()`.
    // And we can infer mapping existence by `search` returning the ID.

    let inner_count = index.len();
    let search_results = index.search(&[1.0, 0.0, 0.0, 0.0], 10).unwrap();
    let found = search_results.iter().any(|(id, _)| *id == target_id);

    if found {
        // If found in search, it must be in inner index
        assert!(inner_count >= 1, "Found via search but index empty?");
    } else {
        // If not found in search, it might be removed from mapping but still in inner (zombie),
        // OR removed from both.
        // Ideally, if not in mapping, search shouldn't return it.
        // But if `inner_count > 0` and search returns nothing (and we only ever added 1 node ID),
        // then we have a zombie vector in `inner` that is unreachable via mapping.
        // Note: we only used `target_id`. So if `inner_count > 0` but `!found`,
        // it means `inner` has vectors but `reverse_mapping` doesn't map them to `target_id`.
        // This is exactly a Zombie Vector.
        if inner_count > 0 {
            panic!(
                "Zombie Vector Detected! Inner count: {}, but ID not found in search.",
                inner_count
            );
        }
    }
}
