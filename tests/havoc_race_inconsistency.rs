use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use aletheiadb::core::id::NodeId;
use std::sync::{Arc, Barrier};
use std::thread;

/// 👺 HAVOC RACE CONDITION TEST
///
/// Triggers the "Phantom Vector" race condition where a vector is added to the
/// usearch index but its mapping is removed, causing a memory leak and state inconsistency.
///
/// Race Sequence:
/// 1. Thread 1 (Add): Inserts into id_mapping (Vacant path) -> Releases lock
/// 2. Thread 2 (Remove): Removes from id_mapping and reverse_mapping
/// 3. Thread 2 (Remove): Removes from inner usearch index (no-op if key not yet added)
/// 4. Thread 1 (Add): Adds to inner usearch index
///
/// Result: Vector exists in usearch but has no mapping. It is invisible to search
/// (filtered out) but consumes memory and capacity.
#[test]
fn havoc_race_phantom_vector() {
    let index = Arc::new(
        HnswIndexBuilder::new(128, DistanceMetric::Cosine)
            .build()
            .expect("Failed to build index"),
    );

    let mut phantom_count = 0;
    let iterations = 500;

    println!("👺 Starting Havoc Race Test ({} iterations)...", iterations);

    for i in 0..iterations {
        let node_id = NodeId::new(i as u64 + 1).unwrap();
        let vector = vec![1.0; 128];

        let idx1 = index.clone();
        let idx2 = index.clone();
        let barrier = Arc::new(Barrier::new(2));
        let b1 = barrier.clone();
        let b2 = barrier.clone();

        // Thread 1: Add
        let t1 = thread::spawn(move || {
            b1.wait();
            idx1.add(node_id, &vector).unwrap();
        });

        // Thread 2: Remove
        let t2 = thread::spawn(move || {
            b2.wait();
            // Busy wait to try to hit the race window
            // The window is between mapping insertion and inner lock acquisition
            // This is tight, so we yield a bit
            if i % 2 == 0 {
                thread::yield_now();
            }
            idx2.remove(node_id).unwrap();
        });

        t1.join().unwrap();
        t2.join().unwrap();

        // Attempt to clean up
        // If the state is consistent, this should remove the vector (if it exists)
        // If inconsistent (Phantom), this will fail to remove it because mapping is missing
        let _ = index.remove(node_id);

        let current_len = index.len();
        if current_len > phantom_count {
            phantom_count = current_len;
            if phantom_count % 10 == 0 {
                println!("🔥 Phantom Vector Leaked! Count: {}", phantom_count);
            }
        }
    }

    if phantom_count > 0 {
        panic!("❌ FAILED: Leaked {} phantom vectors due to race condition.", phantom_count);
    } else {
        println!("✅ Survived without leaks (maybe lucky timing?)");
    }
}
