use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_hnsw_search_in_async_context() {
    // This test ensures that HnswIndex::search can be called from within a Tokio async context
    // without panicking or deadlocking. It serves as a regression test for the `block_in_place` implementation.

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    let node_id = NodeId::new(1).unwrap();
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    // Spawn a blocking task to simulate heavy load / potential blocking
    let index_clone = Arc::clone(&index);
    let handle = tokio::task::spawn_blocking(move || {
        // This is safe because spawn_blocking runs on a dedicated thread pool
        index_clone.search(&[1.0, 0.0, 0.0, 0.0], 10).unwrap()
    });

    let results = handle.await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node_id);

    // Now call it directly in the async task (which runs on a worker thread)
    // If search blocks significantly (e.g. sleep loop), it would stall the worker.
    // With block_in_place, it should yield the worker thread to the runtime.
    let index_clone2 = Arc::clone(&index);
    let result_direct = tokio::task::spawn(async move {
        // This call happens on a worker thread.
        // If HnswIndex uses block_in_place, this thread becomes a blocking thread temporarily,
        // and a new worker is spawned/woken up to replace it.
        index_clone2.search(&[1.0, 0.0, 0.0, 0.0], 10).unwrap()
    })
    .await
    .unwrap();

    assert_eq!(result_direct.len(), 1);
    assert_eq!(result_direct[0].0, node_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_hnsw_add_in_async_context() {
    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    let node_id = NodeId::new(1).unwrap();

    // Call add directly in async task
    let index_clone = Arc::clone(&index);
    tokio::task::spawn(async move {
        index_clone.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    })
    .await
    .unwrap();

    assert_eq!(index.len(), 1);
}
