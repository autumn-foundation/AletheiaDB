use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
#[ignore]
fn test_hnsw_reentrant_deadlock() {
    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    let id1 = NodeId::new(1).unwrap();
    index.add(id1, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let index_clone = index.clone();

    // We expect this to hang or panic (if we set a timeout)
    // To detect hang without hanging the test runner forever, we can spawn a thread and join with timeout

    let (tx, rx) = std::sync::mpsc::channel();

    thread::spawn(move || {
        println!("Starting search with filter...");
        // This closure holds a Read lock on inner
        // The closure attempts to access index again (Needs Read or Write lock)
        // If RwLock is not re-entrant (parking_lot is NOT), this deadlocks immediately.

        // Note: For parking_lot::RwLock, attempting to acquire a read lock recursively
        // while holding a read lock IS allowed ONLY IF there are no pending writers.
        // However, acquiring a WRITE lock recursively is DEFINITELY a deadlock.
        // Let's test the WRITE lock case first as it's the most severe.

        let result = index_clone.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 1, |id| {
            println!("Inside filter for {:?}", id);

            // Attempt to modify index (Needs Write lock) while holding Read lock.
            // This is a classic deadlock.
            // The thread holds Read, wants Write. It cannot get Write until Read is released.
            // Read is released only after filter returns.
            // Filter returns only after Write is acquired.
            // DEADLOCK.

            // We use a different ID to avoid logic errors, but the lock is global for the index.
            let new_id = NodeId::new(id.as_u64() + 100).unwrap();
            let _ = index_clone.add(new_id, &[0.0, 1.0, 0.0, 0.0]);

            true
        });

        println!("Search finished with result: {:?}", result);
        tx.send(()).unwrap();
    });

    // Wait for 2 seconds. If not done, it's a deadlock.
    if rx.recv_timeout(Duration::from_secs(2)).is_err() {
        panic!("DEADLOCK DETECTED: search_with_filter closure caused re-entrancy deadlock!");
    }
}
