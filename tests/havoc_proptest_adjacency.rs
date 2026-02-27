use aletheiadb::core::hasher::IdentityHasher;
use aletheiadb::index::adjacency::AdjacencyIndex;
use proptest::prelude::*;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

proptest! {
    #[test]
    fn test_havoc_import_csr_random_inputs(
        node_ids in proptest::collection::vec(any::<u64>(), 0..10),
        offsets in proptest::collection::vec(any::<u64>(), 0..15),
        edge_ids in proptest::collection::vec(any::<u64>(), 0..100)
    ) {
        let edges_map = HashMap::with_hasher(BuildHasherDefault::<IdentityHasher>::default());

        let result = std::panic::catch_unwind(|| {
            AdjacencyIndex::import_csr(node_ids.clone(), offsets.clone(), edge_ids.clone(), &edges_map)
        });

        // If it didn't panic, it must mean all CSR invariants are strictly valid!
        if let Ok(index) = result {
            // It was a valid index, so invariants must hold
            // Note: If offsets and edge_ids are empty, the index falls back to an empty one internally.
            if offsets.is_empty() || edge_ids.is_empty() {
                // Should be an empty index
                prop_assert_eq!(index.node_count(), 0);
            } else {
                prop_assert_eq!(offsets.len(), node_ids.len() + 1);
                if let Some(&last) = offsets.last() {
                    prop_assert_eq!(last as usize, edge_ids.len());
                }
                for i in 0..offsets.len().saturating_sub(1) {
                    prop_assert!(offsets[i] <= offsets[i + 1]);
                }

                // We can also try querying it to make sure it doesn't crash on get_adjacency!
                for node_id in &node_ids {
                    let _ = index.get_adjacency(aletheiadb::core::id::NodeId::new(*node_id).unwrap());
                }
            }
        }
    }
}
