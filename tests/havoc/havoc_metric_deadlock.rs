use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{
    DistanceMetric, HnswIndex, HnswIndexBuilder, Quantization, VectorIndex,
};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

#[test]
fn test_custom_metric_reentrancy_deadlock() {
    // This test reproduces a deadlock where a custom metric function calls back into the index (e.g. via add).
    // The search holds a read lock on the inner index, and add tries to acquire a write lock.
    // We expect this to be prevented by the re-entrancy guard, returning an error instead of deadlocking.

    // Shared storage for the index so the closure can access it
    let shared_index: Arc<RwLock<Option<Arc<HnswIndex>>>> = Arc::new(RwLock::new(None));
    let shared_index_clone = shared_index.clone();

    // Channel to signal that the metric was executed
    let (tx, rx) = std::sync::mpsc::channel();

    let metric = move |_a: &[f32], _b: &[f32]| -> f32 {
        let _ = tx.send(());
        // Fix clippy::match_result_ok by matching Ok(guard) directly
        if let Ok(guard) = shared_index_clone.read() {
            // Fix clippy::collapsible_if by allowing it (since let_chains are unstable)
            #[allow(clippy::collapsible_if)]
            if let Some(index) = guard.as_ref() {
                // This call should NOT deadlock. It should return an error.
                let result = index.add(NodeId::new(999).unwrap(), &[0.0, 0.0, 0.0, 0.0]);

                // Verify we get the expected error
                assert!(result.is_err(), "Re-entrant add should fail");
                let err = result.unwrap_err();
                assert!(
                    err.to_string().contains("Cannot modify index from within"),
                    "Error should indicate re-entrancy prevention, got: {}",
                    err
                );
            }
        }
        0.0
    };

    let builder = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .quantization(Quantization::F32)
        .with_custom_metric("deadlock_metric", metric);

    let index = Arc::new(builder.build().unwrap());

    // Add initial data to ensure the metric is called
    index
        .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
        .unwrap();

    *shared_index.write().unwrap() = Some(index.clone());

    // Spawn a thread to run the search
    let index_clone = index.clone();
    let handle = thread::spawn(move || {
        // Search triggers the metric
        let _ = index_clone.search(&[0.5, 0.5, 0.0, 0.0], 1);
    });

    // Wait for the metric to be called
    if rx.recv_timeout(Duration::from_secs(5)).is_err() {
        panic!("Metric was not called within timeout");
    }

    // Wait for the thread to join
    // If the fix works, this should happen immediately
    // If it deadlocks, this would hang (but test runner would kill it eventually)
    handle.join().expect("Search thread panicked");

    println!("Test passed: Deadlock prevented, re-entrant call rejected.");
}
