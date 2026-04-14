//! Property-based tests for vector index invariants.

use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use proptest::prelude::*;

/// Generate valid vector with given dimensions.
fn vector_strategy(dims: usize) -> impl Strategy<Value = Vec<f32>> {
    proptest::collection::vec(-1.0f32..=1.0, dims)
}

proptest! {
    /// Invariant: search results are always sorted by similarity (descending).
    #[test]
    fn prop_results_sorted(
        vectors in proptest::collection::vec(vector_strategy(128), 10..50),
        query in vector_strategy(128),
        k in 1usize..20
    ) {
        let dims = 128;
        let index = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
            .build()
            .unwrap();

        for (i, vec) in vectors.iter().enumerate() {
            let node = NodeId::new(i as u64 + 1).unwrap();
            index.add(node, vec).unwrap();
        }

        let results = index.search(&query, k).unwrap();

        // Verify sorted descending
        for i in 1..results.len() {
            prop_assert!(
                results[i-1].1 >= results[i].1,
                "Results not sorted: {} > {} at positions {}, {}",
                results[i].1, results[i-1].1, i-1, i
            );
        }
    }

    /// Invariant: delete followed by search never returns deleted ID.
    #[test]
    fn prop_delete_removes_from_results(
        vectors in proptest::collection::vec(vector_strategy(64), 20..100),
        delete_indices in proptest::collection::vec(0usize..20, 1..10),
        query in vector_strategy(64)
    ) {
        let dims = 64;
        let index = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
            .build()
            .unwrap();

        // Add all vectors
        let mut node_ids: Vec<NodeId> = Vec::new();
        for (i, vec) in vectors.iter().enumerate() {
            let node = NodeId::new(i as u64 + 1).unwrap();
            node_ids.push(node);
            index.add(node, vec).unwrap();
        }

        // Delete some
        let mut deleted: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        for idx in delete_indices {
            let idx = idx % node_ids.len();
            let node = node_ids[idx];
            if !deleted.contains(&node) {
                index.remove(node).unwrap();
                deleted.insert(node);
            }
        }

        // Search should never return deleted IDs
        let results = index.search(&query, 100).unwrap();

        for (id, _) in results {
            prop_assert!(
                !deleted.contains(&id),
                "Deleted node {:?} found in search results",
                id
            );
        }
    }

    /// Invariant: len() equals number of adds minus number of removes.
    #[test]
    fn prop_len_tracks_operations(
        add_count in 10usize..100,
        remove_indices in proptest::collection::vec(0usize..10, 0..5)
    ) {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        // Add vectors
        for i in 0..add_count {
            let node = NodeId::new(i as u64 + 1).unwrap();
            index.add(node, &[i as f32, 0.0, 0.0, 0.0]).unwrap();
        }

        prop_assert_eq!(index.len(), add_count);

        // Remove some (avoiding duplicates)
        let mut removed = 0;
        let mut removed_set: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for idx in remove_indices {
            let idx = idx % add_count;
            if !removed_set.contains(&idx) {
                let node = NodeId::new(idx as u64 + 1).unwrap();
                index.remove(node).unwrap();
                removed += 1;
                removed_set.insert(idx);
            }
        }

        prop_assert_eq!(index.len(), add_count - removed);
    }
}
