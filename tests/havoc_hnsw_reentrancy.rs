use aletheiadb::index::vector::{HnswIndexBuilder, DistanceMetric, VectorIndex};
use aletheiadb::core::id::NodeId;

#[test]
fn test_reentrant_search_returns_error() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    // Add data so search has something to iterate
    index.add(NodeId::new(1).unwrap(), &[0.1, 0.2, 0.3, 0.4]).unwrap();

    let q = vec![0.1, 0.2, 0.3, 0.4];

    // Perform filtered search with a predicate that calls search recursively
    let result = index.search_with_filter(&q, 10, |_id| {
        // Attempt recursive search
        let inner_result = index.search(&q, 1);

        // Assert it failed with correct error
        assert!(inner_result.is_err(), "Recursive search should fail");
        let err = inner_result.unwrap_err();
        assert!(
            err.to_string().contains("Cannot perform search from within"),
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

    index.add(NodeId::new(1).unwrap(), &[0.1, 0.2, 0.3, 0.4]).unwrap();

    let q = vec![0.1, 0.2, 0.3, 0.4];

    let result = index.search_with_filter(&q, 10, |_id| {
        // Attempt recursive search_with_filter
        let inner_result = index.search_with_filter(&q, 1, |_| true);

        assert!(inner_result.is_err(), "Recursive search_with_filter should fail");
        let err = inner_result.unwrap_err();
        assert!(
            err.to_string().contains("Cannot perform search_with_filter from within"),
            "Error message should indicate re-entrancy prevention, got: {}",
            err
        );

        true
    });

    assert!(result.is_ok());
}
