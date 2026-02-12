use aletheiadb::index::vector::{HnswIndexBuilder, DistanceMetric};
use aletheiadb::index::VectorIndex; // Import trait for add/search methods
use aletheiadb::core::id::NodeId;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_deadlock_spawn_in_filter() {
    // Skip on CI/slow environments if needed, but for now we want to catch this.
    // The timeout is short (2s) to fail fast.

    let (tx, rx) = std::sync::mpsc::channel();

    thread::spawn(move || {
        let index = Arc::new(
            HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .build()
                .unwrap()
        );

        // Add initial data
        for i in 1..=10 {
            index.add(NodeId::new(i).unwrap(), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        }

        let index_clone = index.clone();

        // Trigger Deadlock
        // search_with_filter acquires READ lock
        // Use an atomic to ensure we only try the deadlock ONCE to save time
        // (otherwise it runs for every candidate * timeout)
        let tried = std::sync::atomic::AtomicBool::new(false);
        let index_clone_ref = &index_clone; // borrow for closure

        let _ = index.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 5, |_id| {
            // Only try once
            if !tried.swap(true, std::sync::atomic::Ordering::Relaxed) {
                let index_inner = index_clone_ref.clone();
                // Spawn thread to acquire WRITE lock
                let t = thread::spawn(move || {
                    let new_id = NodeId::new(100).unwrap();
                    // This blocks waiting for READ lock (held by parent)
                    // If we fix it, this will timeout and return Err
                    index_inner.add(new_id, &[0.0, 1.0, 0.0, 0.0])
                });

                // Parent waits for child
                // DEADLOCK: Parent holds Read, waits for Child. Child waits for Write (blocked by Read).
                let res = t.join().unwrap();

                // If we fixed it, res is Err(Timeout). If not fixed, we never reach here.
                res.is_ok()
            } else {
                false
            }
        });

        tx.send(()).unwrap();
    });

    // Wait for 15 seconds. If deadlock, we timeout.
    // The index timeout is 10s, so we need to wait longer than that.
    if rx.recv_timeout(Duration::from_secs(15)).is_err() {
        panic!("DEADLOCK DETECTED! The system hung while trying to acquire locks recursively across threads.");
    }
}

#[test]
fn test_remove_lock_timeout() {
    // This test ensures that index.remove() correctly times out if the lock is held.
    // This covers the error handling path in remove().

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap()
    );

    let node_id = NodeId::new(1).unwrap();
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let index_clone = index.clone();
    let (tx, rx) = std::sync::mpsc::channel();

    // Spawn a thread to hold the read lock for longer than the timeout (10s)
    thread::spawn(move || {
        // search_with_filter acquires a read lock
        let _ = index_clone.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 1, |_| {
            tx.send(()).unwrap(); // Signal that we have the lock
            thread::sleep(Duration::from_secs(20)); // Hold it for 20s > 10s timeout
            true
        });
    });

    // Wait for thread to acquire lock
    rx.recv().unwrap();

    // Try to remove - this requires a write lock
    // Should block for 10s then fail with timeout error
    let start = std::time::Instant::now();
    let result = index.remove(node_id);
    let duration = start.elapsed();

    assert!(result.is_err(), "remove() should have failed due to timeout");

    // Verify it took at least 10s (approximate check)
    assert!(duration.as_secs() >= 9, "remove() returned too quickly ({}s), didn't wait for timeout", duration.as_secs());

    match result {
        Err(aletheiadb::utils::Error::Vector(aletheiadb::utils::error::VectorError::IndexError(msg))) => {
            assert!(msg.contains("timed out"), "Error message should mention timeout");
        },
        _ => panic!("Expected IndexError with timeout message, got {:?}", result),
    }
}
