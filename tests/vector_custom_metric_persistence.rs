use gallifreydb::index::vector::{HnswIndexBuilder, DistanceMetric, Quantization, HnswConfig, HnswIndex};
use gallifreydb::index::VectorIndex;
use gallifreydb::core::id::NodeId;
use tempfile::tempdir;
use std::sync::{Arc, Mutex};

#[test]
fn test_custom_metric_persistence() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("metric_test.index");
    let dims = 4;

    // Shared counter to verify metric is called
    let call_count = Arc::new(Mutex::new(0));
    let call_count_clone = call_count.clone();

    // 1. Create index with custom metric
    let index = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
        .with_custom_metric("counting_metric", move |a, b| {
            let mut guard = call_count_clone.lock().unwrap();
            *guard += 1;
            // Simple euclidean
            a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum()
        })
        .build()
        .unwrap();

    let node1 = NodeId::new(1).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    // Reset count
    {
        let mut guard = call_count.lock().unwrap();
        *guard = 0;
    }

    // Verify it works before save
    index.search(&[1.0, 0.0, 0.0, 0.0], 1).unwrap();
    {
        let guard = call_count.lock().unwrap();
        assert!(*guard > 0, "Metric should be called before save");
    }

    index.save(&index_path).unwrap();
    drop(index);

    // 2. Load index with SAME config (including the closure)
    // Note: In a real app, the closure would need to be supplied again since code isn't serialized.
    let call_count_2 = Arc::new(Mutex::new(0));
    let call_count_2_clone = call_count_2.clone();

    let config = HnswConfig::new(dims, DistanceMetric::Cosine)
        .with_custom_metric("counting_metric", move |a, b| {
            let mut guard = call_count_2_clone.lock().unwrap();
            *guard += 1;
            a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum()
        });

    let loaded_index = HnswIndex::load(&index_path, config).unwrap();

    // 3. Search
    loaded_index.search(&[1.0, 0.0, 0.0, 0.0], 1).unwrap();

    // 4. Verify metric was called
    let guard = call_count_2.lock().unwrap();
    assert!(*guard > 0, "Metric SHOULD be called after load, but it was ignored!");
}
