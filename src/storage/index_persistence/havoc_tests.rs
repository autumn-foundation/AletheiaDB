#[cfg(test)]
mod tests {
    use crate::core::id::IdGenerator;
    use crate::storage::current::CurrentStorage;
    use crate::storage::historical::HistoricalStorage;
    use crate::storage::index_persistence::IndexPersistenceManager;
    use parking_lot::RwLock;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn test_aletheiadb_recovery_corrupted_csr() {
        let dir = tempdir().unwrap();
        let path = dir.path();

        let manager = IndexPersistenceManager::new(path.to_path_buf());
        manager.ensure_directories().unwrap();

        let graph_idx_path = manager.graph_path().join("adjacency.idx");

        use crate::storage::index_persistence::formats::{
            GraphIndexData, PersistedEdge, PersistedNode, PersistedPropertyMap,
        };

        // We use label_idx: 1 in the mock data, so we must make sure index 1 is valid (0 is usually empty or uninitialized).
        // Since load_indexes_startup will reload the string interner from disk if manifest exists (we bypass manifest here,
        // wait we must bypass manifest error, so the interner won't be loaded from disk and will be empty?
        // Ah! If string interner is not loaded, it might fail to resolve labels!
        // `load_indexes_startup` will use the `GLOBAL_INTERNER`. Since we run in the same process, we can just populate it here!
        let lbl1 = crate::core::interning::GLOBAL_INTERNER
            .intern("label_node")
            .unwrap();
        let lbl2 = crate::core::interning::GLOBAL_INTERNER
            .intern("label_edge")
            .unwrap();

        let data = GraphIndexData {
            magic: *b"GGRP", // Graph Magic Bytes
            version: 1,
            node_count: 2,
            edge_count: 2,
            nodes: vec![
                PersistedNode {
                    id: 10,
                    label_idx: lbl1.as_u32(),
                    version_id: 1,
                    properties: PersistedPropertyMap { entries: vec![] },
                },
                PersistedNode {
                    id: 20,
                    label_idx: lbl1.as_u32(),
                    version_id: 2,
                    properties: PersistedPropertyMap { entries: vec![] },
                },
            ],
            edges: vec![
                PersistedEdge {
                    id: 0,
                    source_id: 10,
                    target_id: 20,
                    label_idx: lbl2.as_u32(),
                    version_id: 1,
                    properties: PersistedPropertyMap { entries: vec![] },
                },
                PersistedEdge {
                    id: 1,
                    source_id: 20,
                    target_id: 10,
                    label_idx: lbl2.as_u32(),
                    version_id: 2,
                    properties: PersistedPropertyMap { entries: vec![] },
                },
            ],
            outgoing_node_ids: vec![10, 20],
            outgoing_offsets: vec![0, 100, 2], // CORRUPT
            outgoing_neighbors: vec![0, 1],
            incoming_node_ids: vec![10, 20],
            incoming_offsets: vec![0, 100, 2], // CORRUPT
            incoming_neighbors: vec![1, 0],
        };

        crate::storage::index_persistence::graph::save_graph_index(&data, &graph_idx_path).unwrap();

        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let node_id_gen = Arc::new(IdGenerator::new());
        let edge_id_gen = Arc::new(IdGenerator::new());
        let version_id_gen = Arc::new(IdGenerator::new());

        let result =
            crate::storage::index_persistence::graph::load_graph_index(&graph_idx_path).unwrap();
        assert_eq!(result.node_count, 2);

        // Create a dummy manifest so that load_indexes_startup doesn't bail early, although it says "even if manifest failed"
        // Wait, it says: "Try to load manifest and string interner, but don't fail if manifest doesn't exist yet".
        // It does print a warning, but then goes on to load the graph index!

        crate::storage::index_persistence::operations::load_indexes_startup(
            &manager,
            &current,
            &historical,
            &node_id_gen,
            &edge_id_gen,
            &version_id_gen,
        );

        assert_eq!(current.node_count(), 2);
        assert_eq!(current.edge_count(), 2);

        // Verify adjacency is correctly reconstructed despite the corrupted CSR
        assert_eq!(
            current.out_degree(crate::core::id::NodeId::new(10).unwrap()),
            1
        );
        assert_eq!(
            current.out_degree(crate::core::id::NodeId::new(20).unwrap()),
            1
        );
    }
}
