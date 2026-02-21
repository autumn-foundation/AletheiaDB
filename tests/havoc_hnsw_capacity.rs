use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use std::sync::{Arc, Barrier};
use std::thread;

#[test]
fn test_havoc_hnsw_capacity_overflow() {
    // 👺 Havoc Test: "The Stampede"
    //
    // Vulnerability: HnswIndex::check_and_expand_capacity checks capacity under a read lock.
    // If multiple threads pass this check simultaneously, they queue up to acquire the write lock.
    // The "padding" (128) is insufficient if N > padding threads race.
    //
    // Configuration:
    // Capacity = 200
    // Padding = 128 (hardcoded in src/index/vector/hnsw.rs)
    // Initial Size = 70
    // Condition: size + 1 + 128 <= capacity => 70 + 129 = 199 <= 200 (Passes)
    //
    // We launch 150 threads.
    // If they all pass the check (reading size=70), they will add 150 vectors.
    // Final size = 70 + 150 = 220.
    // Capacity = 200.
    // Result: Overflow -> Crash/UB.

    let capacity = 200;
    let initial_size = 70;
    let num_threads = 150;

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(capacity)
            .build()
            .expect("Failed to build index"),
    );

    // Fill initially to 70
    for i in 0..initial_size {
        let id = NodeId::new((i + 1) as u64).unwrap();
        index
            .add(id, &[1.0, 0.0, 0.0, 0.0])
            .expect("Failed initial add");
    }

    assert_eq!(index.len(), initial_size);

    let barrier = Arc::new(Barrier::new(num_threads));
    let mut handles = vec![];

    for i in 0..num_threads {
        let index_clone = Arc::clone(&index);
        let barrier_clone = Arc::clone(&barrier);
        let id = NodeId::new((initial_size + i + 1) as u64).unwrap();

        handles.push(thread::spawn(move || {
            // Synchronize threads to maximize race condition window
            barrier_clone.wait();

            // All threads should pass check_and_expand_capacity concurrently here
            // because they all read size=70 (or close to it) before anyone writes.
            // Then they serialize on inner.write() and overflow.
            if let Err(e) = index_clone.add(id, &[1.0, 0.0, 0.0, 0.0]) {
                // We expect it might fail or panic, but if it returns error, log it.
                eprintln!("Add failed: {}", e);
            }
        }));
    }

    for handle in handles {
        handle
            .join()
            .expect("Thread should not panic with the fix applied");
    }

    // If we survive, verify length. With the fix, all adds should have succeeded.
    let final_len = index.len();
    println!("Final size: {}", final_len);
    assert_eq!(
        final_len,
        initial_size + num_threads,
        "Index should contain all added items"
    );
}
