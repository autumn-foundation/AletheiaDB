#![allow(clippy::manual_is_multiple_of)]
use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

#[test]
fn test_phantom_vector_race() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("havoc.index");

    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    let index = Arc::new(HnswIndex::new(config.clone()).unwrap());

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
    let index_clone = index.clone();
    let db_path_clone = db_path.clone();

    // Thread 1: Adder
    // Adds unique vectors continuously
    let adder = thread::spawn(move || {
        let mut i = 1u64;
        while running_clone.load(Ordering::Relaxed) {
            let node = NodeId::new(i).unwrap();
            let vector = vec![0.1, 0.2, 0.3, 0.4];
            // Ignore errors (might fail during save if we lock aggressively, though HnswIndex is concurrent)
            let _ = index_clone.add(node, &vector);
            i += 1;
            // Small sleep to allow save to interleave
            if i % 100 == 0 {
                thread::yield_now();
            }
        }
        i
    });

    // Thread 2: Saver
    // Saves continuously
    let index_clone2 = index.clone();
    let running_saver = running.clone();
    let saver = thread::spawn(move || {
        while running_saver.load(Ordering::Relaxed) {
            // Save to the same path repeatedly
            // We use a temp file and rename to ensure atomic replacement on disk?
            // HnswIndex::save saves directly.
            // But here we just want to catch it in a bad state.
            let _ = index_clone2.save(&db_path_clone);
            thread::yield_now();
        }
    });

    // Run for 2 seconds
    thread::sleep(Duration::from_secs(2));
    running.store(false, Ordering::Relaxed);

    let _total_added = adder.join().unwrap();
    saver.join().unwrap();

    // Now load the index from disk and check consistency
    if db_path.exists() {
        let loaded_index = HnswIndex::load(&db_path, config).unwrap();

        // HnswIndex has a len() method that returns usearch size
        let index_size = loaded_index.len();

        // We can get mappings via internal method or by iterating?
        // HnswIndex has `get_id_mappings` which is pub(crate).
        // But we are in tests/ so we can't access crate-private methods easily without hacking.
        // However, HnswIndex::len() is public.
        // And we can search.

        // Better way to check mappings count:
        // We can't access id_mapping directly.
        // But we can infer it.
        // If we have phantom vectors, index_size will be greater than the number of reachable nodes.
        // But we don't know which nodes are reachable.

        // Wait, I can use `aletheiadb::index::VectorIndex` trait methods?
        // No method to get mapping count.

        // But I can define a helper in the crate if I was in src/lib.rs.
        // Here I am in tests/.

        // I can cheat by reading the mappings file directly!
        let mappings_path = db_path.with_extension("usearch.mappings");
        let mappings_data = std::fs::read(&mappings_path).unwrap();
        // [MAGIC:4][VERSION:1][COUNT:8]
        let count_bytes: [u8; 8] = mappings_data[5..13].try_into().unwrap();
        let mappings_count = u64::from_le_bytes(count_bytes) as usize;

        println!("Index size (usearch): {}", index_size);
        println!("Mappings count: {}", mappings_count);

        assert_eq!(
            index_size, mappings_count,
            "CONSISTENCY FAILURE: Index has {} vectors but mappings file has {}. Phantom vectors detected!",
            index_size, mappings_count
        );
    } else {
        println!("Save never succeeded?");
    }
}
