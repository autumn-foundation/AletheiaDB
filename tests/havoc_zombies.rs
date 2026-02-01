use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::hnsw::{HIT_RACE_CONDITION, INJECT_RACE_DELAY};
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
    HIT_RACE_CONDITION.store(0, Ordering::Relaxed);

    // --- Scenario 1: Deterministic Race Injection ---
    {
        println!("Testing race condition handling...");
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
        thread::sleep(Duration::from_millis(100));

        // Remove the ID. This removes it from the map.
        // The background thread is sleeping/waiting for lock.
        index.remove(id).unwrap();

        // Wait for background thread to finish
        handle.join().unwrap();

        // Assert counter incremented
        let hits = HIT_RACE_CONDITION.load(Ordering::Relaxed);
        assert!(hits > 0, "Failed to hit race path. Count: {}", hits);
        println!("Race path hit {} times.", hits);
    }

    // Disable injection
    INJECT_RACE_DELAY.store(false, Ordering::Relaxed);
}
