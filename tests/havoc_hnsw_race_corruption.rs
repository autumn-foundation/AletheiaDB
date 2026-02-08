use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::{Arc, Barrier};
use std::thread;

/// Regression test for "Zombie Vector" corruption.
///
/// Previously, a race condition in `HnswIndex::add`'s Vacant path allowed concurrent
/// additions of the same NodeId to overwrite the `id_mapping` entry. This resulted
/// in one thread "winning" the mapping but the other thread (the "loser") rolling
/// back its vector from the inner index.
///
/// If the loser's vector ID was the one that ended up in the mapping (because
/// `insert` overwrites), the index would end up with a mapping pointing to a
/// removed vector.
///
/// This test reproduces the race by spawning two threads that attempt to add the
/// same NodeId simultaneously.
#[test]
fn test_hnsw_race_corruption() {
    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    let iterations = 1000;
    let barrier = Arc::new(Barrier::new(2));

    for i in 0..iterations {
        let id = NodeId::new(i as u64).unwrap();
        let vec = vec![0.1, 0.2, 0.3, 0.4];

        let mut handles = vec![];
        for _ in 0..2 {
            let index_clone = index.clone();
            let barrier_clone = barrier.clone();
            let vec_clone = vec.clone();

            handles.push(thread::spawn(move || {
                // Sync to maximize chance of hitting Vacant path simultaneously
                barrier_clone.wait();
                // We ignore the result because one might fail with "Concurrent add detected"
                // which is EXPECTED. We care about the STATE after.
                let _ = index_clone.add(id, &vec_clone);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // Verify Consistency

        // 1. We should be able to remove the node.
        // If corruption happened, id might map to a key that was removed from inner index.
        if let Err(e) = index.remove(id) {
             // If remove fails, check if search finds it.
             // If search finds it, but remove fails, that's definitely corruption.
             println!("Remove failed for iteration {}: {}", i, e);
        }

        // 2. Search should NOT find the node after removal.
        // If the "Zombie Vector" exists (key A), but id mapped to key B (which was removed),
        // then key A remains in the index and will be found by search.
        let results = index.search(&vec, 1).unwrap();

        for (found_id, _) in results {
            if found_id == id {
                panic!("CORRUPTION DETECTED at iteration {}: Node {:?} was removed but still found in search! (Zombie Vector)", i, id);
            }
        }
    }
}
