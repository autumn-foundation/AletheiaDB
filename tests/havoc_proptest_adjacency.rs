use aletheiadb::core::id::NodeId;
use aletheiadb::index::adjacency::AdjacencyIndex;
use proptest::prelude::*;
use std::collections::HashMap;

proptest! {
    #[test]
    fn test_adjacency_index_no_panic(
        node_ids in proptest::collection::vec(0..100u64, 2..=2),
        mut offsets in proptest::collection::vec(0..100u64, 3..=3),
        edge_ids in proptest::collection::vec(0..100u64, 2..=2),
    ) {
        // force the invariant that the last offset matches edge_ids len
        offsets[2] = 2;

        let edges_map = HashMap::default();

        // This shouldn't panic
        let mut sorted_node_ids = node_ids.clone();
        sorted_node_ids.sort_unstable();
        sorted_node_ids.dedup();
        if sorted_node_ids.len() < 2 {
            return Ok(()); // skip if deduped is smaller
        }

        // It should gracefully return an error here, so we capture the result
        let result = AdjacencyIndex::import_csr(sorted_node_ids.clone(), offsets, edge_ids, &edges_map);

        if let Ok(index) = result {
            // try to get adjacency
            for &n in &sorted_node_ids {
                let _ = index.get_adjacency(NodeId::new(n).unwrap());
            }
        }
    }
}
