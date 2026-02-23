
#[cfg(test)]
mod reproduction_test {
    use aletheiadb::index::adjacency::AdjacencyIndex;
    use aletheiadb::core::id::{NodeId, EdgeId};
    use aletheiadb::core::interning::InternedString;
    use std::collections::HashMap;

    #[test]
    #[should_panic(expected = "CSR offsets length mismatch")]
    fn test_import_csr_missing_validation_panic() {
        // Create corrupted CSR data: offsets too short for node_ids
        let node_ids = vec![1]; // 1 node
        let offsets = vec![0];  // Should be [0, N], length 2. Here length 1.
        let edge_ids = vec![100]; // 1 edge to bypass empty check

        let edges_map = HashMap::new();

        // This should panic immediately due to validation
        let _ = AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map);
    }

    #[test]
    #[should_panic(expected = "CSR last offset mismatch")]
    fn test_import_csr_out_of_bounds_offsets() {
        // Create corrupted CSR data: offsets point outside edges array
        let node_ids = vec![1];
        let offsets = vec![0, 100]; // 100 is > edges.len() (1)
        let edge_ids = vec![100]; // 1 edge
        let edges_map = HashMap::new();

        // This should panic immediately due to validation
        let _ = AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map);
    }

    #[test]
    fn test_import_csr_empty_input_no_panic() {
        // Regression test: Empty input should NOT panic
        let node_ids = vec![];
        let offsets = vec![];
        let edge_ids = vec![];
        let edges_map = HashMap::new();

        let index = AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map);
        assert_eq!(index.edge_count(), 0);
    }
}
