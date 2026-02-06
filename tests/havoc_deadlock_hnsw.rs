use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn test_havoc_deadlock_search_add() {
    // 🧨 DEADLOCK REPRODUCTION 🧨
    // Scenario:
    // Thread 1 (Search): Acquires `inner` (Read) -> Calls Predicate -> Spawns T3 -> T3 calls `save` -> T3 Iterates `id_mapping` (Wants A).
    // Thread 2 (Add): Acquires `id_mapping` (Write) -> Wants `inner` (Write).
    //
    // Cycle: T1 (Inner) -> Waits T3 -> T3 (Wants IdMap) -> Blocked by T2 -> T2 (IdMap) -> Wants Inner (Held by T1).
    // Note: T2 waits for Inner(Write). T1 holds Inner(Read). T2 cannot proceed.
    // T3 needs IdMap. T2 holds IdMap. T3 cannot proceed.
    // T1 waits for T3. T1 cannot proceed.

    let _dir = tempdir().unwrap();

    // Create index
    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap()
    );

    // Populate with one node (Node 1) so we can update it (Occupied path)
    let node1 = NodeId::new(1).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let start_barrier = Arc::new(Barrier::new(2));

    let index_clone1 = index.clone();
    let start_barrier1 = start_barrier.clone();

    // Thread 1: Search with evil predicate
    let t1 = thread::spawn(move || {
        start_barrier1.wait();
        // Reduced iterations to make it run faster but still trigger
        for _ in 0..100 {
            let index_clone2 = index_clone1.clone();

            let _ = index_clone1.search_with_filter(
                &[1.0, 0.0, 0.0, 0.0],
                1,
                |_id| {
                    // Spawn T3 to access id_mapping (via inspect_ids)
                    // We spawn to avoid holding T1's stack, but T3 runs concurrently.
                    let index_clone3 = index_clone2.clone();
                    let t3 = thread::spawn(move || {
                         // Calls inspect_ids(), which iterates id_mapping.
                         // This requires id_mapping read lock.
                         // It does NOT require inner lock.
                         let _ = index_clone3.inspect_ids();
                    });

                    // Wait for T3. This keeps T1 blocked (holding Inner Read lock).
                    t3.join().unwrap();
                    true
                }
            );
            thread::yield_now();
        }
    });

    let index_clone2 = index.clone();
    let start_barrier2 = start_barrier.clone();

    // Thread 2: Add loop (Occupied)
    let t2 = thread::spawn(move || {
        start_barrier2.wait();
        let node1 = NodeId::new(1).unwrap();
        for i in 0..100 {
            // Update node1 repeatedly
            // Occupied path: Holds IdMap lock -> Acquires Inner(Write)
            // Use different vectors to ensure actual updates
            let val = (i % 10) as f32 / 10.0;
            let _ = index_clone2.add(node1, &[val, 1.0 - val, 0.0, 0.0]);
            thread::yield_now();
        }
    });

    // Verification wrapper
    let (tx, rx) = std::sync::mpsc::channel();

    let tx1 = tx.clone();
    thread::spawn(move || {
        t1.join().unwrap();
        let _ = tx1.send("T1 done");
    });

    let tx2 = tx.clone();
    thread::spawn(move || {
        t2.join().unwrap();
        let _ = tx2.send("T2 done");
    });

    // We expect 2 completions. Timeout 10s.
    println!("Waiting for threads...");
    for i in 0..2 {
        if rx.recv_timeout(Duration::from_secs(10)).is_err() {
            panic!("DEADLOCK DETECTED: Threads failed to complete within 10 seconds. Iteration {}", i);
        }
        println!("Thread completed.");
    }
}
