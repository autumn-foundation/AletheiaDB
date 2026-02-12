use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

#[test]
fn test_spawned_thread_deadlock() {
    // Set short timeout to make test faster
    aletheiadb::index::vector::hnsw::set_lock_timeout_internal(Duration::from_millis(100));

    // Setup index
    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );
    let id1 = NodeId::new(1).unwrap();
    index.add(id1, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let index_clone = index.clone();
    let (tx, rx) = mpsc::channel();

    // Spawn a thread to run the search
    thread::spawn(move || {
        let result = index_clone.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 1, |id| {
            let (inner_tx, inner_rx) = mpsc::channel();
            let index_inner = index_clone.clone();

            thread::spawn(move || {
                let new_id = NodeId::new(2).unwrap();
                // Deadlock point: needs Write lock
                // If not fixed, this blocks forever waiting for Read lock (held by search)
                // If fixed, this errors after 10s
                let res = index_inner.add(new_id, &[0.0, 1.0, 0.0, 0.0]);
                // Send result back (ignore error if receiver dropped)
                let _ = inner_tx.send(res);
            });

            // Deadlock point: waits for thread (which waits for us)
            // We use a blocking recv here to ensure deadlock if not fixed
            let _ = inner_rx.recv();
            true
        });
        // Send result back (ignore error if receiver dropped)
        let _ = tx.send(result);
    });

    // Main test waits for search completion
    // If fixed (with 10s timeout), this should complete in ~10s.
    // If deadlocked, this will hit the 15s timeout and panic.
    //
    // Havoc: We set the timeout to 5s initially to prove it fails fast during Red Phase.
    // Note: When we fix it, we will need to ensure the test waits long enough for the timeout (10s).
    // So for the reproduction to fail *fast*, we use 5s.
    // But then the fix won't pass because 10s > 5s.
    //
    // Strategy: Use 2s timeout. Assert panic.
    // Then when we fix, we will modify the test or just verify it passes with 15s.
    // Actually, I'll set it to 15s now. It's a bit slow for "fail fast" but correct for the final state.
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(_) => println!("Search completed successfully (Deadlock broken via timeout)"),
        Err(_) => panic!("Test timed out - Deadlock detected!"),
    }
}
