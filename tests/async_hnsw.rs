use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};

#[cfg(feature = "tokio")]
mod async_tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_search_in_multi_thread_async_context() {
        // This test runs in a multi-threaded runtime where block_in_place is supported
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let node1 = NodeId::new(1).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();

        // This should not block the runtime and should use block_in_place
        // We can't easily verify block_in_place was called without mocking,
        // but we can verify it doesn't panic and returns correct results
        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(results[0].0, node1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_search_in_current_thread_async_context() {
        // This test runs in a current_thread runtime where block_in_place would panic
        // The implementation should detect this and fall back to std::thread::sleep
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let node1 = NodeId::new(1).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();

        // This should NOT panic
        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(results[0].0, node1);
    }
}

#[test]
fn test_search_no_runtime() {
    // This test runs without any tokio runtime
    // The implementation should detect this and fall back to std::thread::sleep
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();
    let node1 = NodeId::new(1).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(results[0].0, node1);
}
