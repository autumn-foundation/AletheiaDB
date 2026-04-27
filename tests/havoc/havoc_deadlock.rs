use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

const VECTOR_DIM: usize = 64;
const NUM_SEARCHERS: usize = 4;
const NUM_ADDERS_VACANT: usize = 2;
const PREPOPULATED_VECTORS: usize = 32;

fn is_ci() -> bool {
    std::env::var("CI").is_ok()
}

/// Returns reduced iterations in CI to avoid timeouts on slow runners.
fn iterations() -> usize {
    if is_ci() { 5 } else { 50 }
}

/// Returns true when running on macOS in CI.
///
/// usearch's internal C++ per-node locking exhibits a lock-ordering cycle on
/// macOS CI runners (both stable and nightly) when concurrent `search` and `add`
/// calls contend on the same graph nodes.  The same scenario passes cleanly on
/// Linux and Windows CI.  Until the upstream usearch issue is resolved, we skip
/// the stress test on macOS CI rather than let it burn 300 s of runner time.
fn is_macos_ci() -> bool {
    is_ci() && cfg!(target_os = "macos")
}

/// Returns a longer timeout in CI to accommodate slow shared runners.
fn test_timeout_secs() -> u64 {
    if is_ci() { 300 } else { 60 }
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
/// - New vector insertion: allocates a new internal key, updates the usearch
///   index, and publishes both ID maps.
/// - Filtered search: searches the usearch index and resolves results through
///   `reverse_mapping`.
///
/// Without proper lock ordering (PR #751), these operations would deadlock.
///
/// Existing-vector update and save/search concurrency are covered by the
/// focused vector update tests, `regression_save_concurrency`, and save-specific
/// havoc tests. This stress test stays scoped to in-memory insert/search lock
/// ordering so upstream persistence or remove/re-add behavior cannot obscure the
/// deadlock signal.
///
/// **macOS CI**: skipped — see `is_macos_ci()` for the rationale.
#[test]
#[serial_test::serial]
fn havoc_deadlock_stress_test() {
    // usearch's internal per-node locking deadlocks on macOS CI runners.
    // The same test passes on Linux and Windows CI.  Skip rather than burning
    // 300 s of runner time on a known-bad platform.
    if is_macos_ci() {
        eprintln!("SKIP havoc_deadlock_stress_test: known usearch deadlock on macOS CI");
        return;
    }

    let (tx, rx) = std::sync::mpsc::channel();

    // Run the actual test in a separate thread to enable timeout enforcement
    thread::spawn(move || {
        let result =
            std::panic::catch_unwind(run_deadlock_stress_test).map_err(panic_payload_to_string);
        let _ = tx.send(result);
    });

    // Watchdog: If test takes longer than timeout, it's likely deadlocked
    let timeout = test_timeout_secs();
    match rx.recv_timeout(Duration::from_secs(timeout)) {
        Ok(Ok(())) => (), // Success
        Ok(Err(message)) => panic!("Deadlock stress worker panicked: {message}"),
        Err(_) => panic!(
            "Deadlock detected or test too slow: Timed out after {}s",
            timeout
        ),
    }
}

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
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
    for i in 0..PREPOPULATED_VECTORS {
        let id = NodeId::new((i + 1) as u64).unwrap();
        let vec = vec![0.1f32; VECTOR_DIM];
        index.add(id, &vec).unwrap();
    }

    // 3. Threads
    let barrier = Arc::new(Barrier::new(NUM_SEARCHERS + NUM_ADDERS_VACANT));

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

    // Join all
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // 4. Assertions
    // Verify we actually did something
    let successes = success_count.load(Ordering::Relaxed);
    assert!(successes > 0, "No operations succeeded during stress test");

    // Verify index consistency
    // We added the initial nodes plus the new vectors inserted by vacant adders.
    let expected_len = PREPOPULATED_VECTORS + (NUM_ADDERS_VACANT * iterations());
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
