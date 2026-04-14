use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};

#[test]
fn test_hnsw_full_lifecycle_coverage() {
    // This integration test ensures that the core HNSW operations are covered
    // by executing them against the public API.
    // This targets lines in src/index/vector/hnsw.rs that were missed by unit tests.

    // 1. Create Index
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node_id = NodeId::new(1).unwrap();
    let vec1 = [1.0, 0.0, 0.0, 0.0];
    let vec2 = [0.0, 1.0, 0.0, 0.0];

    // 2. Add (Vacant Path) - Covers lines ~1080+
    index.add(node_id, &vec1).unwrap();
    assert_eq!(index.len(), 1);

    // 3. Add (Occupied Path) - Covers lines ~900+
    // This is critical for the "Concurrent modification detected" block coverage
    index.add(node_id, &vec2).unwrap();
    assert_eq!(index.len(), 1); // Should still be 1 (update)

    // 4. Search (Happy Path) - Covers search logic and maybe_block_in_place
    let results = index.search(&[0.0, 1.0, 0.0, 0.0], 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node_id);
    // Should match vec2 (similarity ~1.0)
    assert!(results[0].1 > 0.99);

    // 5. Search with Filter
    let filtered = index.search_with_filter(&vec2, 10, |_| true).unwrap();
    assert_eq!(filtered.len(), 1);

    let filtered_empty = index.search_with_filter(&vec2, 10, |_| false).unwrap();
    assert_eq!(filtered_empty.len(), 0);

    // 6. Remove - Covers remove logic
    index.remove(node_id).unwrap();
    assert_eq!(index.len(), 0); // Might be 0 or 1 depending on usearch tombstone behavior, but remove() should succeed

    // 7. Search after remove
    let results_empty = index.search(&vec2, 10).unwrap();
    assert_eq!(results_empty.len(), 0);
}
