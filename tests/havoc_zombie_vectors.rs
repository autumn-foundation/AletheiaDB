use aletheiadb::index::vector::{HnswIndexBuilder, DistanceMetric, HnswIndex, HnswConfig};
use aletheiadb::index::VectorIndex;
use aletheiadb::core::id::NodeId;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::thread;

/// 👺 HAVOC: ZOMBIE VECTOR HUNTER
///
/// Attempts to reproduce a race condition between `add()` and `save()`.
///
/// The Vulnerability:
/// If `save()` collects mappings (snapshot A) and then acquires the index lock
/// (snapshot B), an `add()` operation can slip in between A and B.
///
/// Sequence:
/// 1. Save: Collect mappings (does not include Vector V)
/// 2. Add: Insert Vector V into Index
/// 3. Add: Insert Vector V into Mappings
/// 4. Save: Lock Index, Save Index (includes Vector V)
/// 5. Save: Write Mappings (does NOT include Vector V)
///
/// Result: Index on disk has V, but Mappings file does not.
/// When loaded, V is a "Zombie" - it exists in the graph but cannot be mapped back to a NodeId.
#[test]
fn test_havoc_zombie_vectors() {
    // Run multiple iterations to increase probability of hitting the race window.
    // The window is small (between collecting mappings and acquiring read lock).
    let iterations = 10;

    for i in 0..iterations {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zombie.index");

        let index = Arc::new(
            HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .build()
                .unwrap()
        );

        let running = Arc::new(AtomicBool::new(true));
        let index_saver = index.clone();
        let running_saver = running.clone();

        // Path needs to be owned by the thread or cloned
        let path_saver = path.clone();

        // SAVER THREAD: Continuously saves the index
        let saver = thread::spawn(move || {
             while running_saver.load(Ordering::Relaxed) {
                 // We ignore errors here because we might be saving while the writer
                 // is hammering the index, and we only care if a *successful* save
                 // produces a corrupted file.
                 let _ = index_saver.save(&path_saver);
                 thread::yield_now();
             }
        });

        // WRITER THREAD: Adds vectors
        // We add a moderate amount of vectors to ensure the 'save' operation
        // takes enough time to overlap with 'add' operations.
        for j in 0..2000 {
            let id = NodeId::new(j as u64).unwrap();
            let vec = vec![0.1, 0.2, 0.3, 0.4];
            // We unwrap here because adding should always succeed in memory
            index.add(id, &vec).unwrap();
        }

        // Stop saver
        running.store(false, Ordering::Relaxed);
        saver.join().unwrap();

        // Now verification.
        // We load the index from disk.
        // Note: The file on disk represents the state of the LAST successful save.
        // It might not contain all 2000 vectors, which is fine.
        // BUT: For every vector it contains (inner.len()), it MUST have a mapping.
        if path.exists() {
            // Check if the mappings file also exists (it should if save worked)
            let mappings_path = path.with_extension("usearch.mappings");
            if mappings_path.exists() {
                 let loaded = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine))
                    .expect("Failed to load index");

                let inner_count = loaded.len();
                let map_count = loaded.len_mappings();

                if inner_count > map_count {
                    panic!(
                        "👺 HAVOC SUCCESS: ZOMBIE VECTORS FOUND! \n\
                         Iteration: {}\n\
                         Index Vectors (Inner): {}\n\
                         Mapped Vectors (ID Map): {}\n\
                         Zombies: {}",
                        i, inner_count, map_count, inner_count - map_count
                    );
                }
            }
        }
    }
}
