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

    // Instead of `db.compact_adjacency()` which doesn't exist, we will use the test-only
    // internal api or auto-trigger it. Since I cannot access pub(crate) from `tests/`,
    // I can make a fake `GraphIndexData` entirely manually and write it to `adjacency.idx`!
    // But since `AletheiaDB` was created, I can just let it persist nodes and edges,
    // and I'll modify the `graph_data` manually after loading it.
    db.persist_indexes().unwrap();
    drop(db);

    // Read the graph data
    let manager = IndexPersistenceManager::new(&config.persistence.data_dir);
    let graph_path = manager.graph_path().join("adjacency.idx");

    let mut data = load_graph_index(&graph_path).unwrap();

    // Because the DB didn't compact, outgoing_offsets is likely empty!
    // We can manually fake a CSR structure that fails validation to force it down the error path.
    data.outgoing_node_ids = vec![0];
    data.outgoing_offsets = vec![0, 1]; // Length 2
    data.outgoing_neighbors = vec![1]; // Length 1 (valid so far)

    // Also need incoming to bypass the `!is_empty()` checks
    data.incoming_node_ids = vec![1];
    data.incoming_offsets = vec![0, 1];
    data.incoming_neighbors = vec![0];

    // Corrupt the CSR offsets! (Make length of offsets wrong compared to neighbors)
    data.outgoing_offsets.push(2); // Now length 3, neighbors length 1 -> Mismatch!

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
    assert_eq!(db2.node_count(), 2);
    assert_eq!(db2.edge_count(), 1);
}
// I noticed db.persist_indexes() is saving `outgoing_neighbors` as empty,
// which causes `import_csr` to hit the `if offsets.is_empty() || edge_ids.is_empty() { return Ok(...) }` early return.
// Why is `outgoing_neighbors` empty? `current.export_outgoing_csr()` must be returning empty?
// Wait, `db.persist_indexes()` persists `current.all_edges()`, but CSR is exported from `current.export_outgoing_csr()`.
// `CurrentIndexes` only moves items to CSR via `compact_adjacency()`.
// `db.persist_indexes()` does not automatically compact before exporting!
// Ah, `current.export_outgoing_csr()` exports the frozen CSR. But my new edge is still in the delta buffer!
// Let's add `db.compact_adjacency()` before `db.persist_indexes()`!
// I see that db.persist_indexes() does NOT compact the adjacency!
// So it just saves the frozen CSR (which is empty) and the delta buffer is saved somewhere else?
// Wait, `persist_graph_index` calls `current.export_outgoing_csr()`.
// Let's check what `persist_graph_index` does.
// I'll call `db.current_storage().compact_adjacency();` directly, if I can.
// But AletheiaDB struct has no current_storage() method publicly available.
