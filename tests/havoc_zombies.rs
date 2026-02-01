use gallifreydb::core::id::NodeId;
use gallifreydb::index::VectorIndex;
use gallifreydb::index::vector::hnsw::{
    HIT_RACE_CONDITION_REMOVED, HIT_RACE_CONDITION_REPLACED, INJECT_RACE_DELAY,
};
use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Barrier};
use std::thread;

#[test]
fn test_zombie_vectors_race() {
    // 👺 Havoc Test: Zombie Vector Race Condition
    //
    // This test reproduces a race condition between `add` and `remove` operations
    // that leads to "zombie" vectors leaking into the underlying HNSW index.

    // Retry loop to handle CI flakiness where race condition might not trigger immediately
    for attempt in 1..=5 {
        println!("Attempt {}/5 to trigger race condition...", attempt);

        // Reset counters
        HIT_RACE_CONDITION_REMOVED.store(0, Ordering::Relaxed);
        HIT_RACE_CONDITION_REPLACED.store(0, Ordering::Relaxed);

        let index = Arc::new(
            HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .build()
                .unwrap(),
        );

        // Enable race condition injection to ensure coverage
        INJECT_RACE_DELAY.store(true, Ordering::Relaxed);

        // High contention to trigger the race
        let num_threads = 8;
        // Reduced iterations per attempt since delay is now 50ms
        let iterations = 50;

        println!(
            "Spawning {} threads for {} iterations each...",
            num_threads, iterations
        );

        let barrier = Arc::new(Barrier::new(num_threads));
        let mut handles = vec![];

        // We fight over a single NodeId to maximize collisions
        let target_id = NodeId::new(666).unwrap();

        for t_id in 0..num_threads {
            let index = index.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                let vec = vec![0.1 * (t_id as f32), 0.0, 0.0, 0.0];
                barrier.wait();

                for _ in 0..iterations {
                    // Add and immediately remove
                    // We ignore errors because concurrent ops might fail (e.g. remove non-existent)
                    // but the internal state must remain consistent.
                    let _ = index.add(target_id, &vec);
                    let _ = index.remove(target_id);

                    // Yield to encourage context switching
                    std::thread::yield_now();
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // Disable race injection
        INJECT_RACE_DELAY.store(false, Ordering::Relaxed);

        // Final cleanup to ensure map is definitively empty
        let _ = index.remove(target_id);

        // If the race condition exists, we might have zombie vectors in the index
        // even though the map is empty.
        // index.len() reports the number of vectors in usearch.

        let zombie_count = index.len();
        if zombie_count > 0 {
            println!(
                "👺 HAVOC DETECTED WEAKNESS: Found {} zombie vectors in index!",
                zombie_count
            );
        } else {
            println!("Boring. No zombies found.");
        }

        assert_eq!(
            zombie_count, 0,
            "Race condition detected: {} zombie vectors found in index (Memory Leak)",
            zombie_count
        );

        let removed_hits = HIT_RACE_CONDITION_REMOVED.load(Ordering::Relaxed);
        let replaced_hits = HIT_RACE_CONDITION_REPLACED.load(Ordering::Relaxed);
        println!(
            "Race condition hits: Removed={}, Replaced={}",
            removed_hits, replaced_hits
        );

        // Verify that we actually exercised the fix logic
        if removed_hits > 0 || replaced_hits > 0 {
            println!("Success! Race condition triggered and handled.");
            return;
        }

        println!(
            "Race condition not triggered in attempt {}. Retrying...",
            attempt
        );
    }

    panic!("Fix logic was not exercised by the test after 5 attempts! (Flaky CI runner?)");
}
