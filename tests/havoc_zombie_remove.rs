use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{
    DistanceMetric, HnswConfig, HnswIndex, HnswIndexBuilder, VectorIndex,
};
use std::sync::{Arc, Barrier};
use std::thread;

#[test]
fn test_havoc_zombie_remove_race() {
    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("zombie_test.index");

    // Config: small M/ef to make operations fast enough for tight loops
    let config = HnswConfig::new(4, DistanceMetric::Cosine)
        .with_m(8)
        .with_ef_construction(20);

    // Create index
    let index = HnswIndexBuilder::from_config(&config).build().unwrap();
    // Add initial vectors
    let count = 1000;
    for i in 0..count {
        let id = NodeId::new(i + 1).unwrap();
        let vec = vec![0.1, 0.2, 0.3, 0.4];
        index.add(id, &vec).unwrap();
    }
    index.save(&index_path).unwrap();

    let index = Arc::new(index);
    let num_iterations = 20;
    let barrier = Arc::new(Barrier::new(3)); // 1 remover + 1 adder + 1 saver

    let mut handles = vec![];

    // Thread 1: Remover
    {
        let index = index.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..num_iterations {
                // Remove a batch of IDs
                for i in (0..count).step_by(2) {
                    let id = NodeId::new(i + 1).unwrap();
                    let _ = index.remove(id);
                }
                // Yield to allow saver to interleave
                thread::yield_now();
            }
        }));
    }

    // Thread 2: Adder (to keep index size non-zero)
    {
        let index = index.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..num_iterations {
                // Add back the removed IDs
                for i in (0..count).step_by(2) {
                    let id = NodeId::new(i + 1).unwrap();
                    let vec = vec![0.1, 0.2, 0.3, 0.4];
                    let _ = index.add(id, &vec);
                }
                thread::yield_now();
            }
        }));
    }

    // Thread 3: Saver & Verifier
    {
        let index = index.clone(); // Not strictly needed for save (we save to path), but keeps arc alive
        let path = index_path.clone();
        let config = config.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let mut zombie_detected = false;
            for _ in 0..num_iterations {
                // Save the index (this is the operation we suspect is raced)
                // Note: we call save on the ARC index, which calls save_internal
                if let Err(e) = index.save(&path) {
                    eprintln!("Save failed: {}", e);
                    continue;
                }

                // Immediately load it back to check for consistency
                // We load into a separate instance
                match HnswIndex::load(&path, config.clone()) {
                    Ok(loaded) => {
                        let inner_len = loaded.len();
                        let mappings_len = loaded.len_mappings();

                        if inner_len > mappings_len {
                            println!(
                                "ZOMBIE DETECTED! Inner: {}, Mappings: {}",
                                inner_len, mappings_len
                            );
                            zombie_detected = true;
                            // Don't break immediately, let's see if it persists or happens often
                            // Actually, finding one is enough to fail the test.
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Load failed: {}", e);
                    }
                }

                // Sleep slightly to allow race conditions to manifest
                // thread::sleep(Duration::from_millis(1));
            }
            if zombie_detected {
                panic!("Havoc: Zombie vectors detected on disk due to non-atomic remove!");
            }
        }));
    }

    for handle in handles {
        if let Err(e) = handle.join() {
            // Propagate panic from threads
            std::panic::resume_unwind(e);
        }
    }
}
