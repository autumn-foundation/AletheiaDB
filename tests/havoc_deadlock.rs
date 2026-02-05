use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder};
use gallifreydb::index::VectorIndex;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

#[test]
fn havoc_deadlock_stress_test() {
    // 1. Setup HnswIndex
    // Use low M and EF to make operations faster (more contention)
    let index = HnswIndexBuilder::new(384, DistanceMetric::Cosine)
        .m(8)
        .ef_construction(64)
        .build()
        .expect("Failed to build index");

    let index = Arc::new(index);

    // 2. Pre-populate some data
    for i in 0..100 {
        let id = NodeId::new(i + 1).unwrap();
        let vec = vec![0.1f32; 384];
        index.add(id, &vec).unwrap();
    }

    // 3. Threads
    let num_searchers = 4;
    let num_adders_vacant = 2;
    let num_adders_occupied = 2;
    let num_savers = 1;

    let barrier = Arc::new(Barrier::new(num_searchers + num_adders_vacant + num_adders_occupied + num_savers));

    let mut handles = vec![];

    // Searchers using filter (locks inner -> reverse_mapping)
    for _ in 0..num_searchers {
        let index = index.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let query = vec![0.1f32; 384];
            for _ in 0..10_000 {
                // Filter that accesses node ID (reverse mapping)
                // We access the node ID to force the closure to actually do something with the mapping
                let _ = index.search_with_filter(&query, 10, |id| {
                    id.as_u64() % 2 == 0
                });
            }
        }));
    }

    // Adders (Vacant) (locks id_mapping -> reverse_mapping -> inner)
    for t in 0..num_adders_vacant {
        let index = index.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let vec = vec![0.1f32; 384];
            for i in 0..10_000 {
                let id_val = 1000 + (t * 1000) + i;
                let id = NodeId::new(id_val as u64).unwrap();
                let _ = index.add(id, &vec);
            }
        }));
    }

    // Adders (Occupied) (locks id_mapping -> inner)
    for _t in 0..num_adders_occupied {
        let index = index.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let vec = vec![0.2f32; 384];
            for i in 0..10_000 {
                // Update existing nodes 1..100
                // Use a different node each time to hit different shards
                let id_val = (i % 100) + 1;
                let id = NodeId::new(id_val as u64).unwrap();
                let _ = index.add(id, &vec);
            }
        }));
    }

    // Savers (iterates id_mapping)
    for _ in 0..num_savers {
        let index = index.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("index.usearch");
            barrier.wait();
            for _ in 0..20 { // Increase saves slightly
                let _ = index.save(&path);
                // Sleep slightly to allow other threads to make progress and create different lock states
                thread::sleep(Duration::from_millis(5));
            }
        }));
    }

    // Join all
    for handle in handles {
        handle.join().unwrap();
    }
}
