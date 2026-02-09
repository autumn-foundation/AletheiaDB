use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use aletheiadb::index::VectorIndex;
use std::sync::Arc;
use std::thread;

// Havoc: This test attempts to trigger the race condition between HnswIndex::add and HnswIndex::save.
// If add() updates the index but fails/pauses before updating the mapping, save() might capture
// a "Phantom Vector" (vector in index, but no ID mapping).
#[test]
fn test_havoc_save_consistency_race() {
    let iterations = 100;

    for i in 0..iterations {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");

        // Create index
        let index = Arc::new(HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap());

        let index_clone_add = Arc::clone(&index);
        let index_clone_save = Arc::clone(&index);
        let path_clone = path.clone();

        // Spawn thread to add a vector
        let handle_add = thread::spawn(move || {
            let id = NodeId::new(1).unwrap();
            let vec = vec![1.0, 0.0, 0.0, 0.0];
            // Loop a few times to increase race probability
            for _ in 0..10 {
                // We add/overwrite the same ID
                index_clone_add.add(id, &vec).unwrap();
                // Tiny yield to allow interleaving
                thread::yield_now();
            }
        });

        // Spawn thread to save the index repeatedly
        let handle_save = thread::spawn(move || {
            for _ in 0..10 {
                // Save to the same path (overwriting)
                // We ignore errors because concurrent saves/adds might race on file I/O
                let _ = index_clone_save.save(&path_clone);
                thread::yield_now();
            }
        });

        handle_add.join().unwrap();
        handle_save.join().unwrap();

        // Load the index and check consistency
        // Note: The file might be corrupted if save() was interrupted or partial,
        // but HnswIndex::load should handle corruption gracefully or return error.
        // What we care about is successful load but inconsistent state.

        if let Ok(loaded) = aletheiadb::index::HnswIndex::load(
            &path,
            aletheiadb::index::vector::HnswConfig::new(4, DistanceMetric::Cosine)
        ) {
            let len = loaded.len();
            if len > 0 {
                // If index has vectors, we MUST be able to find them.
                // Search for the vector we added.
                let results = loaded.search(&[1.0, 0.0, 0.0, 0.0], 10).unwrap();

                // If results are empty but len > 0, we have a Phantom Vector!
                if results.is_empty() {
                    panic!(
                        "Havoc detected Phantom Vector! Iteration {}: Index has {} vectors but search returned 0 results. \
                        This means the vector exists in the usearch index but is missing from the ID mapping.",
                        i, len
                    );
                }
            }
        }
    }
}
