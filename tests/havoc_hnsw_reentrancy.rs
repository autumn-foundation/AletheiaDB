use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{
    DistanceMetric, HnswIndex, HnswIndexBuilder, Quantization, VectorIndex,
};
use std::sync::{Arc, Mutex, Weak};

#[test]
fn test_reentrant_search_returns_error() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    // Add data so search has something to iterate
    index
        .add(NodeId::new(1).unwrap(), &[0.1, 0.2, 0.3, 0.4])
        .unwrap();

    let q = vec![0.1, 0.2, 0.3, 0.4];

    // Perform filtered search with a predicate that calls search recursively
    let result = index.search_with_filter(&q, 10, |_id| {
        // Attempt recursive search
        let inner_result = index.search(&q, 1);

        // Assert it failed with correct error
        assert!(inner_result.is_err(), "Recursive search should fail");
        let err = inner_result.unwrap_err();
        assert!(
            err.to_string()
                .contains("Cannot perform search from within"),
            "Error message should indicate re-entrancy prevention, got: {}",
            err
        );

        true
    });

    // The outer search should succeed
    assert!(result.is_ok());
}

#[test]
fn test_reentrant_search_with_filter_returns_error() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    index
        .add(NodeId::new(1).unwrap(), &[0.1, 0.2, 0.3, 0.4])
        .unwrap();

    let q = vec![0.1, 0.2, 0.3, 0.4];

    let result = index.search_with_filter(&q, 10, |_id| {
        // Attempt recursive search_with_filter
        let inner_result = index.search_with_filter(&q, 1, |_| true);

        assert!(
            inner_result.is_err(),
            "Recursive search_with_filter should fail"
        );
        let err = inner_result.unwrap_err();
        assert!(
            err.to_string()
                .contains("Cannot perform search_with_filter from within"),
            "Error message should indicate re-entrancy prevention, got: {}",
            err
        );

        true
    });

    assert!(result.is_ok());
}

#[test]
fn test_custom_metric_reentrancy_panic_on_len() {
    // Shared state to hold a reference to the index
    let shared_index: Arc<Mutex<Option<Weak<HnswIndex>>>> = Arc::new(Mutex::new(None));
    let shared_index_clone = shared_index.clone();

    // Define a malicious custom metric that calls len()
    let metric_fn = move |_: &[f32], _: &[f32]| -> f32 {
        let maybe_index = {
            let guard = shared_index_clone.lock().unwrap();
            guard.clone()
        };

        #[allow(clippy::collapsible_if)]
        if let Some(weak_index) = maybe_index {
            if let Some(index) = weak_index.upgrade() {
                // This should panic now (caught by wrapper), preventing deadlock
                let _ = index.len();
            }
        }
        0.0
    };

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .quantization(Quantization::F32)
        .with_custom_metric("panic_generator", metric_fn)
        .build()
        .unwrap();

    let index_arc = Arc::new(index);
    {
        let mut guard = shared_index.lock().unwrap();
        *guard = Some(Arc::downgrade(&index_arc));
    }

    index_arc
        .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
        .unwrap();

    // This add should trigger metric execution.
    // The metric panics (caught), returning f32::MAX.
    // The add should succeed (graceful degradation).
    let result = index_arc.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0]);

    assert!(
        result.is_ok(),
        "Add should succeed despite metric panic (caught)"
    );
}

#[test]
fn test_custom_metric_reentrancy_search_returns_error() {
    // Shared state to hold a reference to the index
    let shared_index: Arc<Mutex<Option<Weak<HnswIndex>>>> = Arc::new(Mutex::new(None));
    let shared_index_clone = shared_index.clone();
    let error_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let error_seen_clone = error_seen.clone();

    let metric_fn = move |_: &[f32], _: &[f32]| -> f32 {
        let maybe_index = {
            let guard = shared_index_clone.lock().unwrap();
            guard.clone()
        };

        #[allow(clippy::collapsible_if)]
        if let Some(weak_index) = maybe_index {
            if let Some(index) = weak_index.upgrade() {
                // This should return Error now (checked in search)
                let res = index.search(&[0.0; 4], 1);
                if let Err(err) = res {
                    if err
                        .to_string()
                        .contains("Cannot perform search from within a callback")
                    {
                        error_seen_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            }
        }
        0.0
    };

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .quantization(Quantization::F32)
        .with_custom_metric("error_generator", metric_fn)
        .build()
        .unwrap();

    let index_arc = Arc::new(index);
    {
        let mut guard = shared_index.lock().unwrap();
        *guard = Some(Arc::downgrade(&index_arc));
    }

    index_arc
        .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
        .unwrap();

    // This add triggers metric execution
    index_arc
        .add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0])
        .unwrap();

    assert!(
        error_seen.load(std::sync::atomic::Ordering::SeqCst),
        "Metric should have seen error from recursive search"
    );
}
