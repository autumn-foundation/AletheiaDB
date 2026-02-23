use aletheiadb::index::adjacency::AdjacencyIndex;
use aletheiadb::core::id::{NodeId, EdgeId};
use aletheiadb::core::interning::InternedString;
use std::collections::HashMap;

#[test]
#[should_panic(expected = "CSR invariant: offsets.len() == node_ids.len() + 1")]
fn test_import_csr_insufficient_offsets() {
    let node_ids = vec![1];
    let offsets = vec![0]; // Missing end offset (should be [0, 1])
    let edge_ids = vec![100];
    let mut edges_map = HashMap::new();
    edges_map.insert(EdgeId::new(100).unwrap(), (NodeId::new(2).unwrap(), InternedString::from_raw(0)));

    let _index = AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map);
}

#[test]
#[should_panic(expected = "CSR invariant: last offset matches edge count")]
fn test_import_csr_offset_out_of_bounds() {
    let node_ids = vec![1];
    let offsets = vec![0, 5]; // Claims to have 5 edges
    let edge_ids = vec![100]; // But actually 1 edge
    let mut edges_map = HashMap::new();
    edges_map.insert(EdgeId::new(100).unwrap(), (NodeId::new(2).unwrap(), InternedString::from_raw(0)));

    let _index = AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map);
}
