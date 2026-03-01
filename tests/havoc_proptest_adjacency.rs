use aletheiadb::index::adjacency::AdjacencyIndex;
use proptest::prelude::*;
use std::collections::HashMap;

proptest! {
    #[test]
    fn prop_import_csr_no_panic(
        node_ids in proptest::collection::vec(any::<u64>(), 0..100),
        offsets in proptest::collection::vec(any::<u64>(), 0..100),
        edge_ids in proptest::collection::vec(any::<u64>(), 0..100),
    ) {
        // We only care about ensuring `import_csr` doesn't panic on invalid inputs
        let edges_map = HashMap::with_hasher(std::hash::BuildHasherDefault::default());
        let _ = AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map);
    }
}
