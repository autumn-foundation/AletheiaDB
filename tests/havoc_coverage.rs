use aletheiadb::index::vector::{HnswIndexBuilder, DistanceMetric};
use aletheiadb::index::VectorIndex;
use aletheiadb::core::id::NodeId;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use aletheiadb::index::vector::hnsw::{LOCK_TIMEOUT_MS, TEST_ADD_DELAY_MS};

#[test]
fn test_add_rollback_timeout() {
    // This test forces the "rollback" path in add() to timeout.
    // 1. T1 starts add(). Adds to inner. Pauses at hook.
    // 2. T2 starts add(). Adds to inner. Adds to id_mapping. T2 wins.
    // 3. T3 acquires READ lock on inner (blocking T1's rollback).
    // 4. T1 resumes. Finds id_mapping occupied. Tries to rollback (acquire write lock).
    // 5. T1 times out.

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap()
    );

    // Reduce lock timeout to 100ms
    LOCK_TIMEOUT_MS.store(100, std::sync::atomic::Ordering::Relaxed);

    let index_t1 = index.clone();
    let index_t2 = index.clone();
    let index_t3 = index.clone();

    let node_id = NodeId::new(1).unwrap();

    // Step 1: Set long delay
    // T1 will reach the hook, read 2000ms, and sleep.
    TEST_ADD_DELAY_MS.store(2000, std::sync::atomic::Ordering::Relaxed);

    // Step 2: Spawn T1
    // We clone index_t1 again because we moved it in previous failed attempt? No, it's fine.
    // But we need to use a new variable for the closure to avoid move errors if we reused code.
    let index_t1_clone = index_t1.clone();
    let t1 = thread::spawn(move || {
        let vec = vec![1.0, 0.0, 0.0, 0.0];
        index_t1_clone.add(node_id, &vec)
    });

    // Wait for T1 to reach the hook (it performs inner.add first, which is fast)
    thread::sleep(Duration::from_millis(200));

    // Step 3: Clear delay for T2
    TEST_ADD_DELAY_MS.store(0, std::sync::atomic::Ordering::Relaxed);

    // Step 4: T2 runs and wins the ID
    index_t2.add(node_id, &[0.0, 1.0, 0.0, 0.0]).unwrap();

    // Step 5: T3 acquires READ lock to block T1's rollback
    let (tx, rx) = std::sync::mpsc::channel();
    let _t3 = thread::spawn(move || {
        // Hold lock for 3s (T1 wakes at 2s, timeout is 0.1s in tests)
        let _ = index_t3.search_with_filter(&[0.0, 0.0, 0.0, 0.0], 1, |_| {
            tx.send(()).unwrap();
            thread::sleep(Duration::from_secs(3));
            true
        });
    });

    // Wait for T3 to acquire lock
    rx.recv().unwrap();

    // Step 6: Wait for T1 result
    let result = t1.join().unwrap();

    // Reset flags just in case
    TEST_ADD_DELAY_MS.store(0, std::sync::atomic::Ordering::Relaxed);
    LOCK_TIMEOUT_MS.store(10_000, std::sync::atomic::Ordering::Relaxed);

    assert!(result.is_err(), "T1 should have failed due to rollback timeout");
    match result {
        Err(aletheiadb::utils::Error::Vector(aletheiadb::utils::error::VectorError::IndexError(msg))) => {
            assert!(msg.contains("timed out in rollback"), "Expected rollback timeout error, got: {}", msg);
        },
        _ => panic!("Expected IndexError with rollback timeout message, got {:?}", result),
    }
}

#[test]
fn test_filter_ghost_key() {
    // Covers the `None` path in search_with_filter closure (key not in reverse_mapping)

    // We need to bypass the public API to insert a ghost key.
    // HnswIndex structure fields are private (not `pub`).
    // But we are in `tests/` which is an integration test. We cannot access private fields.
    //
    // However, we can use the fact that `HnswIndex` is a wrapper around `usearch::Index`.
    // The `inner` field is private.
    //
    // Is there any public method that modifies inner but NOT mapping? No.
    // Is there any way to desynchronize them?
    // Maybe `load` with a corrupted mapping file?
    //
    // If we load an index where `mappings` file is missing an entry that exists in the `.usearch` file.
    // 1. Create index, add item X.
    // 2. Save index.
    // 3. Manually edit mappings file to remove item X.
    // 4. Load index. `usearch` has X, `id_mapping` does not.
    // 5. Search. `usearch` returns X. Filter gets X. `reverse_mapping.get(X)` returns None.

    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("ghost.index");

    // 1. Create and populate
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();
    let node_id = NodeId::new(1).unwrap();
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap(); // Key will be 0
    index.save(&index_path).unwrap();

    // 2. Corrupt mappings file (truncate data)
    // Format V2: Header (27 bytes) + Data (16 bytes * count) + CRC (4 bytes)
    // We want to keep header but remove data.
    // But `load_mappings_with_integrity` checks CRC and size.
    // So we need to legitimately create a mappings file with 0 entries but valid header/CRC.

    let mappings_path = index_path.with_extension("usearch.mappings");

    // Create a new empty index just to generate a valid empty mappings file
    let empty_path = dir.path().join("empty.index");
    let empty_index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();
    empty_index.save(&empty_path).unwrap();

    // Overwrite the original mappings file with the empty one
    let empty_mappings_path = empty_path.with_extension("usearch.mappings");
    std::fs::copy(empty_mappings_path, &mappings_path).unwrap();

    // 3. Load the "corrupted" index
    // HnswIndex::load will load `usearch` (1 vector) and `mappings` (0 vectors).
    // It should succeed (validation is minimal).
    let loaded_index = aletheiadb::index::vector::HnswIndex::load(
        &index_path,
        aletheiadb::index::vector::HnswConfig::new(4, DistanceMetric::Cosine)
    ).unwrap();

    assert_eq!(loaded_index.len(), 1); // inner has 1
    // But mapping is empty.

    // 4. Search
    // Should return the ghost key. Filter should see it, fail to find ID, return false (exclude).
    let results = loaded_index.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 10, |_| true).unwrap();

    // Since filter returns false for the ghost key, results should be empty.
    assert!(results.is_empty(), "Ghost key should be filtered out because it maps to None");
}
