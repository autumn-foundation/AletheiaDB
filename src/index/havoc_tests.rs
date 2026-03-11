use crate::core::id::MAX_VALID_ID;
use crate::index::AdjacencyIndex;
use proptest::prelude::*;
use std::collections::HashMap;

proptest! {
    #[test]
    fn test_import_csr_havoc_node_id(
        invalid_id in (MAX_VALID_ID + 1)..=u64::MAX
    ) {
        let node_ids = vec![invalid_id];
        let offsets = vec![0, 1];
        let edge_ids = vec![100];
        let edge_map = HashMap::default();

        let result = std::panic::catch_unwind(|| {
            AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edge_map)
        });

        // We expect it to panic because the ID is invalid.
        assert!(result.is_err(), "import_csr accepted an invalid NodeId > MAX_VALID_ID without panicking");
    }
}
