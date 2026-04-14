use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};

#[test]
fn test_warden_coverage_search_happy_path() {
    // This test explicitly exercises the search function to ensure code coverage
    // hits the "Happy Path" lines inside the retry loop.
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node_id = NodeId::new(1).unwrap();
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    // Call search - should hit lines inside retry loop
    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node_id);
}

#[test]
fn test_warden_coverage_search_with_filter_happy_path() {
    // This test exercises search_with_filter to ensure coverage
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node_id = NodeId::new(1).unwrap();
    index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    // Call search_with_filter
    let results = index
        .search_with_filter(&[1.0, 0.0, 0.0, 0.0], 10, |_| true)
        .unwrap();
    assert_eq!(results.len(), 1);
}
