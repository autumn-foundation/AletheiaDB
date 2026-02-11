use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use aletheiadb::index::VectorIndex;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
#[ignore = "Demonstrates deadlock via thread spawning in filter callback (Havoc)"]
fn test_deadlock_in_filter_spawn() {
    // 👺 Havoc: Threading deadlock demonstration
    // "Users will spawn threads in callbacks because they can."
    // This test demonstrates that while IN_FILTER_CALLBACK protects against
    // recursive calls on the same thread, it cannot prevent deadlocks
    // when the callback spawns a thread that waits on the index lock.

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .expect("Failed to build index"),
    );

    let node1 = NodeId::new(1).unwrap();
    index
        .add(node1, &[1.0, 0.0, 0.0, 0.0])
        .expect("Failed to add node");

    let index_clone = Arc::clone(&index);

    // Watchdog to detect deadlock
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(2));
        if rx.try_recv().is_err() {
            // If we haven't received success signal, we likely deadlocked.
            // We panic to fail the test runner.
            // Note: In a real test runner, this panic happens in a background thread
            // and might not immediately stop the main test thread, but it logs the failure.
            panic!("👺 HAVOC: Watchdog timed out! Deadlock detected.");
        }
    });

    println!("Starting search with filter...");
    let result = index.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 10, move |_id| {
        println!("Inside filter callback");

        // Spawn a thread that tries to modify the index
        // This thread will NOT have IN_FILTER_CALLBACK set (thread-local)
        let index_inner = Arc::clone(&index_clone);
        let handle = thread::spawn(move || {
            println!("Spawned thread trying to add...");
            let node2 = NodeId::new(2).unwrap();
            // This attempts to acquire inner.write()
            // Main thread holds inner.read()
            // Main thread is waiting for this thread.
            // --> DEADLOCK
            index_inner.add(node2, &[0.0, 1.0, 0.0, 0.0])
        });

        println!("Waiting for spawned thread...");
        // Join means we wait for the spawned thread to finish.
        // The spawned thread is waiting for us to release the lock.
        // We are waiting for the spawned thread.
        let res = handle.join();
        println!("Spawned thread finished: {:?}", res);
        true
    });

    println!("Search result: {:?}", result);
    tx.send(()).unwrap(); // Signal success
}
