#![cfg(feature = "test-utils")]

use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::hnsw::{LOCK_TIMEOUT_MS, TEST_ADD_DELAY_MS, TEST_ROLLBACK_DELAY_MS};
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_add_rollback_timeout() {
    // This test forces the "rollback" path in add() to timeout.
    // 1. T1 starts add(). Checks Vacant. Pauses at ADD hook.
    // 2. T2 starts add(). Checks Vacant. Adds. Inserts to id_mapping. T2 wins.
    // 3. T1 resumes. Adds. Checks id_mapping (Occupied). Enters rollback.
    // 4. T1 pauses at ROLLBACK hook.
    // 5. T3 starts. Acquires READ lock.
    // 6. T1 resumes. Tries to acquire WRITE lock for rollback. Times out.

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    // Reduce lock timeout to 100ms
    LOCK_TIMEOUT_MS.store(100, std::sync::atomic::Ordering::Relaxed);

    let index_t1 = index.clone();
    let index_t2 = index.clone();
    let index_t3 = index.clone();

    let node_id = NodeId::new(1).unwrap();

    // Step 1: Pause T1 at start (after Vacant check)
    TEST_ADD_DELAY_MS.store(500, std::sync::atomic::Ordering::Relaxed);

    // Set rollback delay for later
    TEST_ROLLBACK_DELAY_MS.store(2000, std::sync::atomic::Ordering::Relaxed);

    // Spawn T1
    let t1 = thread::spawn(move || {
        let vec = vec![1.0, 0.0, 0.0, 0.0];
        index_t1.add(node_id, &vec)
    });

    // Wait for T1 to reach the ADD hook
    thread::sleep(Duration::from_millis(100));

    // Step 2: T2 runs and wins the ID
    // T2 sees Vacant because T1 hasn't inserted yet
    TEST_ADD_DELAY_MS.store(0, std::sync::atomic::Ordering::Relaxed); // Let T2 run fast
    index_t2.add(node_id, &[0.0, 1.0, 0.0, 0.0]).unwrap();

    // Restore T1's delay (though it might have already passed the check if we're slow,
    // but the sleep is inside the hook, so T1 is already sleeping).
    // Actually, T1 read the atomic value ONCE. So changing it now doesn't affect T1's current sleep.

    // Wait for T1 to wake up from ADD hook, do the work, and hit ROLLBACK hook.
    // T1 sleeps for 500ms total. We are at ~100ms.
    // T1 will wake at ~500ms.
    // T1 will see T2's entry.
    // T1 will hit ROLLBACK hook and sleep for 2000ms.

    thread::sleep(Duration::from_millis(600));
    // Now T1 should be in rollback sleep.

    // Step 3: T3 acquires READ lock to block T1's rollback
    let (tx, rx) = std::sync::mpsc::channel();
    let _t3 = thread::spawn(move || {
        // Hold lock for 3s (blocks T1's rollback write lock)
        let _ = index_t3.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 1, |_| {
            tx.send(()).unwrap();
            thread::sleep(Duration::from_secs(3));
            true
        });
    });

    // Wait for T3 to acquire lock
    rx.recv().unwrap();

    // Step 4: Wait for T1 result
    let result = t1.join().unwrap();

    // Reset flags
    TEST_ADD_DELAY_MS.store(0, std::sync::atomic::Ordering::Relaxed);
    TEST_ROLLBACK_DELAY_MS.store(0, std::sync::atomic::Ordering::Relaxed);
    LOCK_TIMEOUT_MS.store(5000, std::sync::atomic::Ordering::Relaxed);

    assert!(
        result.is_err(),
        "T1 should have failed due to rollback timeout"
    );
    match result {
        Err(aletheiadb::utils::Error::Vector(
            aletheiadb::utils::error::VectorError::IndexError(msg),
        )) => {
            // Verify it was the rollback timeout, not the add timeout
            assert!(
                msg.contains("during rollback"),
                "Expected rollback timeout error, got: {}",
                msg
            );
        }
        _ => panic!(
            "Expected IndexError with rollback timeout message, got {:?}",
            result
        ),
    }
}

#[test]
fn test_filter_ghost_key() {
    // Covers the `None` path in search_with_filter closure (key not in reverse_mapping)

    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("ghost.index");

    // 1. Create and populate
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();
    let node_id = NodeId::new(1).unwrap();
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap(); // Key will be 0
    index.save(&index_path).unwrap();

    // 2. Corrupt mappings file (truncate data)
    let mappings_path = index_path.with_extension("usearch.mappings");

    // Create a new empty index just to generate a valid empty mappings file
    let empty_path = dir.path().join("empty.index");
    let empty_index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();
    empty_index.save(&empty_path).unwrap();

    // Overwrite the original mappings file with the empty one
    let empty_mappings_path = empty_path.with_extension("usearch.mappings");
    std::fs::copy(empty_mappings_path, &mappings_path).unwrap();

    // 3. Load the "corrupted" index
    let loaded_index = aletheiadb::index::vector::HnswIndex::load(
        &index_path,
        aletheiadb::index::vector::HnswConfig::new(4, DistanceMetric::Cosine),
    )
    .unwrap();

    assert_eq!(loaded_index.len(), 1); // inner has 1
    // But mapping is empty.

    // 4. Search
    // Should return the ghost key. Filter should see it, fail to find ID, return false (exclude).
    let results = loaded_index
        .search_with_filter(&[1.0, 0.0, 0.0, 0.0], 10, |_| true)
        .unwrap();

    // Since filter returns false for the ghost key, results should be empty.
    assert!(
        results.is_empty(),
        "Ghost key should be filtered out because it maps to None"
    );
}
