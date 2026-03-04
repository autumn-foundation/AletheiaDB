use aletheiadb::AletheiaDB;
use aletheiadb::AletheiaDBConfig;
use aletheiadb::index::current::CurrentIndexes;
use aletheiadb::storage::index_persistence::IndexPersistenceManager;
use aletheiadb::storage::index_persistence::graph::{load_graph_index, save_graph_index};

#[test]
fn test_current_indexes_import_csr_propagates_error() {
    let indexes = CurrentIndexes::new();

    let out_node_ids = vec![10];
    let out_offsets = vec![0];
    let out_edge_ids = vec![100];
    let in_node_ids = vec![10];
    let in_offsets = vec![0, 1];
    let in_edge_ids = vec![100];

    let res = indexes.import_csr(
        out_node_ids.clone(),
        out_offsets.clone(),
        out_edge_ids.clone(),
        in_node_ids.clone(),
        in_offsets.clone(),
        in_edge_ids.clone(),
    );

    assert!(res.is_err());
    assert!(res.unwrap_err().contains("CSR offsets length mismatch"));

    let out_offsets_valid = vec![0, 1];
    let in_offsets_invalid = vec![0];

    let res2 = indexes.import_csr(
        out_node_ids,
        out_offsets_valid,
        out_edge_ids,
        in_node_ids,
        in_offsets_invalid,
        in_edge_ids,
    );

    assert!(res2.is_err());
    assert!(res2.unwrap_err().contains("CSR offsets length mismatch"));
}

#[test]
fn test_load_indexes_startup_corrupt_csr_fallback() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut config = AletheiaDBConfig::default();
    config.wal.wal_dir = temp_dir.path().join("wal");
    config.persistence.data_dir = temp_dir.path().join("data");

    // Create DB to write valid data
    let db = AletheiaDB::with_unified_config(config.clone()).unwrap();
    let source = db.create_node("Person", Default::default()).unwrap();
    let target = db.create_node("Company", Default::default()).unwrap();
    db.create_edge(source, target, "WORKS_AT", Default::default())
        .unwrap();
    db.persist_indexes().unwrap();
    drop(db);

    // Read the graph data
    let manager = IndexPersistenceManager::new(&config.persistence.data_dir);
    let graph_path = manager.graph_path().join("adjacency.idx");

    let mut data = load_graph_index(&graph_path).unwrap();

    // Corrupt the CSR offsets! (Make lengths mismatch to trigger import_csr error)
    if !data.outgoing_offsets.is_empty() {
        let last = data.outgoing_offsets.len() - 1;
        data.outgoing_offsets[last] = 9999;
    }

    // Save back to disk
    save_graph_index(&data, &graph_path).unwrap();

    // Re-initialize DB from this corrupted state
    // It should hit the fallback `compact_adjacency()` logic!
    let db2 = AletheiaDB::with_unified_config(config);

    // The DB should successfully load without panicking
    assert!(
        db2.is_ok(),
        "Database should gracefully recover from corrupted CSR offsets"
    );

    let db2 = db2.unwrap();

    // Graph should still be queryable (since compact_adjacency rebuilds it from the base nodes/edges)
    // We do a simple node count / degree check instead of SQL to avoid parsing
    assert_eq!(db2.node_count(), 2);
    assert_eq!(db2.edge_count(), 1);
}
