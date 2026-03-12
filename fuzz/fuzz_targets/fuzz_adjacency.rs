#![no_main]

use libfuzzer_sys::fuzz_target;
use aletheiadb::index::AdjacencyIndex;
use aletheiadb::core::id::{NodeId, EdgeId};
use aletheiadb::core::interning::InternedString;
use std::collections::HashMap;

fuzz_target!(|data: (Vec<u64>, Vec<u64>, Vec<u64>)| {
    let (node_ids, offsets, edge_ids) = data;

    // Create a dummy edges map for edge IDs
    let mut edges_map = HashMap::with_hasher(std::hash::BuildHasherDefault::<aletheiadb::core::hasher::IdentityHasher>::default());

    for &edge_id in &edge_ids {
        if let Ok(id) = EdgeId::new(edge_id) {
            edges_map.insert(id, (NodeId::new(1).unwrap(), InternedString::from_raw(0)));
        }
    }

    // Attempt to import CSR
    let _ = AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map);
});
