// Regression Test: Concurrent Save and Search
// Ensures that `HnswIndex::save` does not block search operations.
// Fixes "Stop the World" DoS vulnerability where save held a write lock on the index.

use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn regression_save_allows_concurrent_search() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("regression.index");

    // Create index
    let index = Arc::new(
        HnswIndexBuilder::new(384, DistanceMetric::Cosine)
            .m(16)
            .ef_construction(100)
            .build()
            .unwrap(),
    );

    // Populate with significant data (100k vectors)
    // This makes save take non-trivial time
    println!("Populating index with 100k vectors...");
    let count = 100_000;
    let vector = vec![0.1f32; 384];

    // Batch add to speed up setup
    for i in 0..count {
        index.add(NodeId::new(i + 1).unwrap(), &vector).unwrap();
    }
    println!("Population complete.");

    let barrier = Arc::new(Barrier::new(2));

    // Thread 1: The Saver
    let index_clone1 = index.clone();
    let barrier_clone1 = barrier.clone();
    let save_path = path.clone();
    let save_handle = thread::spawn(move || {
        barrier_clone1.wait();
        let start = Instant::now();
        println!("Starting SAVE operation...");
        index_clone1.save(&save_path).unwrap();
        let duration = start.elapsed();
        println!("SAVE complete in {:?}", duration);
        duration
    });

    // Thread 2: The Searcher
    // Wait a tiny bit to ensure Save has started and acquired locks
    let index_clone2 = index.clone();
    let barrier_clone2 = barrier.clone();
    let search_handle = thread::spawn(move || {
        barrier_clone2.wait();
        // Give save thread a head start to acquire lock
        thread::sleep(Duration::from_millis(50));

        let start = Instant::now();
        println!("Starting SEARCH operation...");
        // This search should return instantly if not blocked
        let _ = index_clone2.search(&vector, 10).unwrap();
        let duration = start.elapsed();
        println!("SEARCH complete in {:?}", duration);
        duration
    });

    let save_duration = save_handle.join().unwrap();
    let search_duration = search_handle.join().unwrap();

    // Verification:
    // If search was blocked, it would take roughly same time as save (e.g., > 100ms).
    // If concurrent, search should take < 10ms (or at least significantly less than save).

    // If save was too fast (< 50ms), the test is inconclusive because search started after save finished.
    if save_duration < Duration::from_millis(50) {
        println!(
            "WARNING: Save was too fast ({:?}) to effectively test concurrency.",
            save_duration
        );
        // We can't fail here, but we should note it.
        return;
    }

    // Search should be much faster than save if concurrent.
    // If search > 100ms and save > 100ms, it was likely blocked.
    if search_duration > Duration::from_millis(100) {
        panic!(
            "Search was blocked by Save operation! Latency: {:?} (Save took {:?})",
            search_duration, save_duration
        );
    } else {
        println!(
            "SUCCESS: Search was fast ({:?}) while save took {:?}.",
            search_duration, save_duration
        );
    }
}
