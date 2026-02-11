use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

const VECTOR_DIM: usize = 384;
const NUM_SEARCHERS: usize = 4;
const NUM_ADDERS_VACANT: usize = 2;
const NUM_ADDERS_OCCUPIED: usize = 2;
const NUM_SAVERS: usize = 1;

fn is_ci() -> bool {
    std::env::var("CI").is_ok()
}

/// Returns reduced iterations in CI to avoid timeouts on slow runners.
fn iterations() -> usize {
    if is_ci() { 10 } else { 100 }
}

/// Returns a longer timeout in CI to accommodate slow shared runners.
fn test_timeout_secs() -> u64 {
    if is_ci() { 120 } else { 60 }
}

/// Chaos Engineering: Concurrency Stress Test
///
/// This test verifies that the `HnswIndex` implementation is free from deadlocks
/// under high contention from mixed operations.
///
/// **Lock Ordering Verified:**
/// 1. `id_mapping` (DashMap) -> `inner` (usearch RwLock) -> `reverse_mapping` (DashMap)
///
/// **Scenarios:**
/// - `add` (Vacant): Locks `id_mapping` -> `inner` -> `reverse_mapping`
/// - `add` (Occupied): Locks `id_mapping` -> `inner`
/// - `search_with_filter`: Locks `inner` -> `reverse_mapping`
/// - `save`: Locks `id_mapping` (iter) -> `inner` (read)
///
/// Without proper lock ordering (PR #751), these operations would deadlock.
#[test]
fn havoc_deadlock_stress_test() {
    let (tx, rx) = std::sync::mpsc::channel();

    // Run the actual test in a separate thread to enable timeout enforcement
    thread::spawn(move || {
        run_deadlock_stress_test();
        tx.send(()).unwrap();
    });

    // Watchdog: If test takes longer than timeout, it's likely deadlocked
    let timeout = test_timeout_secs();
    match rx.recv_timeout(Duration::from_secs(timeout)) {
        Ok(_) => (), // Success
        Err(_) => panic!(
            "Deadlock detected or test too slow: Timed out after {}s",
            timeout
        ),
    }
}

fn run_deadlock_stress_test() {
    // 1. Setup HnswIndex
    // Use low M and EF to make operations faster (more contention)
    let index = HnswIndexBuilder::new(VECTOR_DIM, DistanceMetric::Cosine)
        .m(8)
        .ef_construction(64)
        .build()
        .expect("Failed to build index");

    let index = Arc::new(index);

    // Track successful operations
    let success_count = Arc::new(AtomicUsize::new(0));

    // 2. Pre-populate some data
    for i in 0..100 {
        let id = NodeId::new(i + 1).unwrap();
        let vec = vec![0.1f32; VECTOR_DIM];
        index.add(id, &vec).unwrap();
    }

    // 3. Threads
    let barrier = Arc::new(Barrier::new(
        NUM_SEARCHERS + NUM_ADDERS_VACANT + NUM_ADDERS_OCCUPIED + NUM_SAVERS,
    ));

    let mut handles = vec![];

    // Searchers using filter (locks inner -> reverse_mapping)
    for _ in 0..NUM_SEARCHERS {
        let index = index.clone();
        let barrier = barrier.clone();
        let counter = success_count.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let query = vec![0.1f32; VECTOR_DIM];
            for _ in 0..iterations() {
                // Filter that accesses node ID (reverse mapping)
                // We access the node ID to force the closure to actually do something with the mapping
                let result = index.search_with_filter(&query, 10, |id| id.as_u64() % 2 == 0);
                if result.is_ok() {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                thread::yield_now(); // Prevent starvation
            }
        }));
    }

    // Adders (Vacant) (locks id_mapping -> reverse_mapping -> inner)
    for t in 0..NUM_ADDERS_VACANT {
        let index = index.clone();
        let barrier = barrier.clone();
        let counter = success_count.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let vec = vec![0.1f32; VECTOR_DIM];
            for i in 0..iterations() {
                let id_val = 1000 + (t * 1000) + i;
                let id = NodeId::new(id_val as u64).unwrap();
                match index.add(id, &vec) {
                    Ok(_) => {
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => eprintln!("Add (vacant) error: {}", e),
                }
                thread::yield_now(); // Prevent starvation
            }
        }));
    }

    // Adders (Occupied) (locks id_mapping -> inner)
    for _t in 0..NUM_ADDERS_OCCUPIED {
        let index = index.clone();
        let barrier = barrier.clone();
        let counter = success_count.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let vec = vec![0.2f32; VECTOR_DIM];
            for i in 0..iterations() {
                // Update existing nodes 1..100
                // Use a different node each time to hit different shards
                let id_val = (i % 100) + 1;
                let id = NodeId::new(id_val as u64).unwrap();
                match index.add(id, &vec) {
                    Ok(_) => {
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => eprintln!("Add (occupied) error: {}", e),
                }
                thread::yield_now(); // Prevent starvation
            }
        }));
    }

    // Savers (iterates id_mapping)
    for _ in 0..NUM_SAVERS {
        let index = index.clone();
        let barrier = barrier.clone();
        let counter = success_count.clone();
        handles.push(thread::spawn(move || {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("index.usearch");
            barrier.wait();
            // Use iterations() here too so saver load scales with config
            for _ in 0..iterations() {
                match index.save(&path) {
                    Ok(_) => {
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => eprintln!("Save error: {}", e),
                }
                // Sleep slightly to allow other threads to make progress and create different lock states
                thread::sleep(Duration::from_millis(10));
            }
        }));
    }

    // Join all
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // 4. Assertions
    // Verify we actually did something
    let successes = success_count.load(Ordering::Relaxed);
    assert!(successes > 0, "No operations succeeded during stress test");

    // Verify index consistency
    // We added 100 initial nodes + (NUM_ADDERS_VACANT * iterations()) new nodes
    // Updates do not change count
    let expected_len = 100 + (NUM_ADDERS_VACANT * iterations());
    assert_eq!(
        index.len(),
        expected_len,
        "Index length mismatch after stress test"
    );

    // Verify a random sample of nodes exists
    for i in 0..iterations() {
        let id_val = 1000 + i; // From first adder
        let id = NodeId::new(id_val as u64).unwrap();
        // Just verify we can remove it (it exists)
        assert!(
            index.remove(id).is_ok(),
            "Failed to find expected node {} after test",
            id_val
        );
    }
}
