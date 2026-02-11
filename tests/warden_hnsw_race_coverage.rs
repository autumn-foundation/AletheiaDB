use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use aletheiadb::index::VectorIndex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const VECTOR_DIM: usize = 4;

/// This test is designed to force the race condition in `HnswIndex::add` where:
/// 1. Thread A finds an Occupied entry for Node X.
/// 2. Thread A drops the DashMap lock.
/// 3. Thread B modifies Node X (removes it or changes the key).
/// 4. Thread A acquires the Inner lock and checks `still_valid`.
///
/// To trigger this, we need high contention on a single NodeId with mixed Add and Remove operations.
#[test]
fn test_hnsw_optimistic_retry_coverage() {
    let index = HnswIndexBuilder::new(VECTOR_DIM, DistanceMetric::Cosine)
        .m(16)
        .ef_construction(64)
        .build()
        .expect("Failed to build index");
    let index = Arc::new(index);

    let stop = Arc::new(AtomicBool::new(false));
    let node_id = NodeId::new(1).unwrap();

    // Thread 1: Continuously ADD vector A
    let t1_index = index.clone();
    let t1_stop = stop.clone();
    let t1 = thread::spawn(move || {
        let vec = vec![1.0, 0.0, 0.0, 0.0];
        while !t1_stop.load(Ordering::Relaxed) {
            let _ = t1_index.add(node_id, &vec);
            // No sleep - maximize contention
        }
    });

    // Thread 2: Continuously REMOVE the node
    let t2_index = index.clone();
    let t2_stop = stop.clone();
    let t2 = thread::spawn(move || {
        while !t2_stop.load(Ordering::Relaxed) {
            let _ = t2_index.remove(node_id);
            // No sleep - maximize contention
        }
    });

    // Thread 3: Continuously ADD vector B (different key allocation potentially)
    let t3_index = index.clone();
    let t3_stop = stop.clone();
    let t3 = thread::spawn(move || {
        let vec = vec![0.0, 1.0, 0.0, 0.0];
        while !t3_stop.load(Ordering::Relaxed) {
            let _ = t3_index.add(node_id, &vec);
        }
    });

    // Run for enough time to statistically guarantee hitting the race
    thread::sleep(Duration::from_secs(2));

    stop.store(true, Ordering::Relaxed);

    t1.join().unwrap();
    t2.join().unwrap();
    t3.join().unwrap();
}
