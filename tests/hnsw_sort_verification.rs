#[cfg(test)]
mod tests {
    use gallifreydb::index::vector::{HnswIndexBuilder, DistanceMetric};
    use gallifreydb::core::id::NodeId;
    use gallifreydb::index::VectorIndex;
    use rand::Rng;

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

    #[test]
    fn test_usearch_sorted_results_comprehensive() {
        // Test multiple metrics
        for metric in [DistanceMetric::Cosine, DistanceMetric::Euclidean, DistanceMetric::DotProduct] {
            let index = HnswIndexBuilder::new(128, metric).build().unwrap();

            // Add 100 random vectors
            let mut rng = rand::thread_rng();
            for i in 1..=100 {
                let vec: Vec<f32> = (0..128).map(|_| rng.r#gen()).collect();
                index.add(NodeId::new(i).unwrap(), &vec).unwrap();
            }

            // Query with random vector
            let query: Vec<f32> = (0..128).map(|_| rng.r#gen()).collect();

            // Case 1: Standard search (k=10)
            let results = index.search(&query, 10).unwrap();
            assert_eq!(results.len(), 10);
            for i in 0..results.len()-1 {
                assert!(results[i].1 >= results[i+1].1,
                    "Similarity not descending for {:?}: {} < {}",
                    metric, results[i].1, results[i+1].1);
            }

            // Case 2: Edge case - single result (k=1)
            let results_single = index.search(&query, 1).unwrap();
            assert_eq!(results_single.len(), 1);
            // Verify the single result matches the top result from k=10
            assert_eq!(results_single[0].0, results[0].0);

            // Case 3: Edge case - k > available
            let results_all = index.search(&query, 200).unwrap();
            assert_eq!(results_all.len(), 100); // Should return all 100
            for i in 0..results_all.len()-1 {
                assert!(results_all[i].1 >= results_all[i+1].1,
                    "Similarity not descending for {:?} with k > N: {} < {}",
                    metric, results_all[i].1, results_all[i+1].1);
            }
        }
    }
}
