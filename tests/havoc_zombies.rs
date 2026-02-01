use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::hnsw::{
    HIT_RACE_CONDITION_REMOVED, HIT_RACE_CONDITION_REPLACED, INJECT_RACE_DELAY,
};
use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder};
use gallifreydb::index::VectorIndex;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_zombie_vectors_race_coverage() {
    // 👺 Havoc Test: Zombie Vector Race Condition Coverage
    //
    // This test deterministically exercises the two race condition fix paths
    // in HnswIndex::add:
    // 1. "Removed": ID mapping is gone when lock is acquired.
    // 2. "Replaced": ID mapping points to a different key (from a concurrent add).

    // Reset counters
    HIT_RACE_CONDITION_REMOVED.store(0, Ordering::Relaxed);
    HIT_RACE_CONDITION_REPLACED.store(0, Ordering::Relaxed);

    // --- Scenario 1: Deterministic "Removed" Path ---
    {
        println!("Testing 'Removed' path...");
        let index = Arc::new(
            HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .build()
                .unwrap(),
        );
        let id = NodeId::new(100).unwrap();
        let vec = vec![0.1, 0.2, 0.3, 0.4];

        // Enable delay to open the race window
        INJECT_RACE_DELAY.store(true, Ordering::Relaxed);

        let index_clone = index.clone();
        let vec_clone = vec.clone();
        let handle = thread::spawn(move || {
            // This add will:
            // 1. Alloc key
            // 2. Insert into map
            // 3. Sleep 50ms (INJECT_RACE_DELAY)
            // 4. Try to acquire lock -> Verify map -> Fail
            index_clone.add(id, &vec_clone).unwrap();
        });

        // Main thread: wait a bit to ensure background thread is in sleep
        thread::sleep(Duration::from_millis(10));

        // Remove the ID. This removes it from the map.
        // The background thread is sleeping/waiting for lock.
        index.remove(id).unwrap();

        // Wait for background thread to finish
        handle.join().unwrap();

        // Assert "Removed" counter incremented
        let removed = HIT_RACE_CONDITION_REMOVED.load(Ordering::Relaxed);
        assert!(
            removed > 0,
            "Failed to hit 'Removed' race path. Count: {}",
            removed
        );
        println!("'Removed' path hit {} times.", removed);
    }

    // --- Scenario 2: Deterministic "Replaced" Path ---
    {
        println!("Testing 'Replaced' path...");
        let index = Arc::new(
            HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .build()
                .unwrap(),
        );
        let id = NodeId::new(200).unwrap();
        let vec1 = vec![0.1, 0.2, 0.3, 0.4];
        let vec2 = vec![0.5, 0.6, 0.7, 0.8];

        // Enable delay
        INJECT_RACE_DELAY.store(true, Ordering::Relaxed);

        let index_clone = index.clone();
        let vec1_clone = vec1.clone();
        let handle = thread::spawn(move || {
            // This add will:
            // 1. Alloc key K1
            // 2. Insert ID->K1 into map
            // 3. Sleep 50ms
            // 4. Try to acquire lock -> Verify map -> Fail (because map now has ID->K2)
            index_clone.add(id, &vec1_clone).unwrap();
        });

        // Main thread: wait a bit
        thread::sleep(Duration::from_millis(10));

        // Replace the ID.
        // We must REMOVE then ADD to force a new key allocation.
        // HnswIndex updates reuse keys if map entry exists, so straight add() would effectively be a "wait and acquire" valid update.
        // By removing, we clear the map. Then adding allocates a NEW key (K2).
        index.remove(id).unwrap();
        index.add(id, &vec2).unwrap();

        // Wait for background thread to finish
        handle.join().unwrap();

        // Assert "Replaced" counter incremented
        let replaced = HIT_RACE_CONDITION_REPLACED.load(Ordering::Relaxed);
        assert!(
            replaced > 0,
            "Failed to hit 'Replaced' race path. Count: {}",
            replaced
        );
        println!("'Replaced' path hit {} times.", replaced);
    }

    // Disable injection
    INJECT_RACE_DELAY.store(false, Ordering::Relaxed);
}
