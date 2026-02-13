use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex, VectorIndex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_zombie_vector_consistency() {
    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("zombie.index");

    // Config: small M/ef to make saves fast and frequent
    let config = HnswConfig::new(4, DistanceMetric::Cosine)
        .with_m(8)
        .with_ef_construction(20);

    let index = Arc::new(HnswIndex::new(config.clone()).unwrap());

    let running = Arc::new(AtomicBool::new(true));
    let run_duration = Duration::from_secs(5);

    // Writer thread: continuously adds new vectors
    let writer_index = index.clone();
    let writer_running = running.clone();
    let writer_handle = thread::spawn(move || {
        let mut i = 0u64;
        while writer_running.load(Ordering::Relaxed) {
            i += 1;
            let id = NodeId::new(i).unwrap();
            let vec = [0.1, 0.2, 0.3, 0.4]; // Dummy vector
            writer_index.add(id, &vec).unwrap();
            // No sleep - maximize throughput to hit race condition
        }
        i // Return count
    });

    // Saver thread: continuously saves the index
    let saver_index = index.clone();
    let saver_running = running.clone();
    let saver_path = index_path.clone();
    let saver_handle = thread::spawn(move || {
        while saver_running.load(Ordering::Relaxed) {
            if let Err(e) = saver_index.save(&saver_path) {
                eprintln!("Save failed: {}", e);
            }
            thread::sleep(Duration::from_millis(10));
        }
    });

    // Run for a bit
    thread::sleep(run_duration);
    running.store(false, Ordering::Relaxed);

    let total_added = writer_handle.join().unwrap();
    saver_handle.join().unwrap();

    println!("Total added in memory: {}", total_added);

    // Now load the index from disk
    let loaded_index = HnswIndex::load(&index_path, config).expect("Failed to load index");
    let loaded_len = loaded_index.len();
    println!("Loaded index len (from header): {}", loaded_len);

    // Increase ef_search to ensure we find all items (since they are all identical)
    loaded_index.set_ef_search(loaded_len + 100);

    // Verify consistency:
    // Check mapping count directly. This is the source of truth for Zombie Vectors.
    // If len_mappings() < len(), we have Zombie Vectors (unreachable data).
    // If len_mappings() > len(), we have Ghost Vectors (harmless empty mappings).
    // We expect them to be equal or mappings > inner (due to our fix).
    let mapping_count = loaded_index.len_mappings();
    println!("Mapping count: {}", mapping_count);

    assert!(
        mapping_count >= loaded_len,
        "Zombie vectors detected! Inner: {}, Mappings: {}. We have {} zombies.",
        loaded_len, mapping_count, loaded_len.saturating_sub(mapping_count)
    );

    println!("Consistency check passed: Mappings ({}) >= Inner ({})", mapping_count, loaded_len);
}
