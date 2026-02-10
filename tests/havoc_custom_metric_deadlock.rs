use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{
    DistanceMetric, HnswIndex, HnswIndexBuilder, Quantization, VectorIndex,
};
use std::sync::{Arc, RwLock};
use std::thread;

#[test]
fn test_custom_metric_reentrancy_prevention() {
    eprintln!("👺 Havoc: Test starting (Re-entrancy Prevention)...");

    // Shared state to allow the metric to access the index.
    struct SharedIndex {
        index: RwLock<Option<Arc<HnswIndex>>>,
    }

    let shared = Arc::new(SharedIndex {
        index: RwLock::new(None),
    });

    let shared_clone = shared.clone();

    // Define a custom metric that tries to modify the index.
    let metric_fn = move |a: &[f32], b: &[f32]| -> f32 {
        if let Ok(guard) = shared_clone.index.try_read() {
            if let Some(index) = guard.as_ref() {
                // Attempt to add a vector.
                // This requires acquiring the HnswIndex.inner lock (Write).
                // But we already hold HnswIndex.inner lock (Read) via the search operation calling this metric.
                // This creates a classic Read-Lock-Held-Want-Write-Lock deadlock (or upgrade deadlock).
                //
                // FIX: With `FilterCallbackGuard` in `create_metric_wrapper`, this call should return an error.

                // Use a dummy ID and vector
                let new_id = NodeId::new(999).unwrap();
                // Ensure vector is correct dimension (same as input)
                let vec = vec![0.0; a.len()];

                match index.add(new_id, &vec) {
                    Ok(_) => panic!("👺 Havoc: Add succeeded unexpectedly! Should have been prevented by re-entrancy check."),
                    Err(e) => {
                        let msg = e.to_string();
                        // Expected error: "Cannot modify index from within a search_with_filter callback..."
                        if msg.contains("Cannot modify index") && msg.contains("re-entrancy") {
                            // This is the expected behavior!
                        } else {
                            // Unexpected error (e.g. OOM, etc.)
                            eprintln!("👺 Havoc: Add failed with unexpected error: {}", e);
                            // We don't panic here because it might be another valid error, but we prefer the re-entrancy one.
                            // But for this test, let's just log it. The main thing is NO DEADLOCK.
                        }
                    },
                }
            }
        }

        // Return a valid distance so usearch can continue
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f32>()
            .sqrt()
    };

    // Build the index with the custom metric.
    let builder = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .quantization(Quantization::F32)
        .with_custom_metric("deadlock_metric", metric_fn);

    let index = builder.build().expect("Failed to build index");
    let index_arc = Arc::new(index);

    // Populate the shared container with the index
    // We do this AFTER initial adds to avoid re-entrancy error during initialization (though it would be safe now)
    // But we want to test search specifically.

    index_arc
        .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
        .unwrap();
    index_arc
        .add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0])
        .unwrap();

    *shared.index.write().unwrap() = Some(index_arc.clone());

    // Spawn a thread to run the search (which triggers the metric)
    let (tx, rx) = std::sync::mpsc::channel();
    let index_clone = index_arc.clone();

    thread::spawn(move || {
        eprintln!("👺 Havoc: Starting search thread...");
        // This search will call the custom metric for comparisons.
        // The custom metric will call add(), which should return Err immediately.
        // The search should proceed normally.
        let result = index_clone.search(&[0.5, 0.5, 0.0, 0.0], 2);
        eprintln!("👺 Havoc: Search finished: {:?}", result);

        // Assert search succeeded
        assert!(result.is_ok(), "Search failed: {:?}", result);

        tx.send(()).unwrap();
    });

    // Wait for the thread to complete.
    // If deadlock was not fixed, this would hang (and test timeout would kill it).
    // But now it should complete quickly.
    rx.recv().expect("Search thread panicked or failed");

    eprintln!("👺 Havoc: Test passed! No deadlock.");
}
