#[cfg(test)]
mod tests {
    use gallifreydb::core::id::NodeId;
    use gallifreydb::index::VectorIndex;
    use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder};

    #[test]
    fn test_usearch_sorted_results() {
        let index = HnswIndexBuilder::new(3, DistanceMetric::Cosine)
            .build()
            .unwrap();

        let n1 = NodeId::new(1).unwrap();
        let n2 = NodeId::new(2).unwrap();
        let n3 = NodeId::new(3).unwrap();

        // 1.0, 0.0, 0.0
        index.add(n1, &[1.0, 0.0, 0.0]).unwrap();
        // 0.9, 0.1, 0.0
        index.add(n2, &[0.9, 0.1, 0.0]).unwrap();
        // 0.0, 1.0, 0.0
        index.add(n3, &[0.0, 1.0, 0.0]).unwrap();

        // Query: 1.0, 0.0, 0.0
        // Expected order: n1 (sim=1.0), n2 (sim~=0.9), n3 (sim=0.0)
        let results = index.search(&[1.0, 0.0, 0.0], 3).unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].0, n1);
        assert_eq!(results[1].0, n2);
        assert_eq!(results[2].0, n3);

        // Verify similarity is strictly descending
        assert!(results[0].1 >= results[1].1);
        assert!(results[1].1 >= results[2].1);
    }
}
