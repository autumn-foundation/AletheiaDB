//! Sentry tests for coverage gaps and edge cases.
//!
//! These tests target specific lines identified as uncovered by codecov
//! to ensure robust handling of error paths and edge cases.

use aletheiadb::{
    AletheiaDB, CurrentStorage,
    index::vector::temporal::TemporalVectorConfig,
};

#[test]
fn test_enable_temporal_vector_index_without_config() {
    let db = AletheiaDB::new().expect("Failed to create DB");

    // Attempt to enable temporal vector index without HNSW config
    // and without a pre-existing vector index.
    // This targets src/db/vector.rs:90
    let config = TemporalVectorConfig::default(); // hnsw_config is None by default

    let result = db.enable_temporal_vector_index("missing_prop", config);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(format!("{}", err).contains("HNSW configuration is required"));
}

#[test]
fn test_vector_search_missing_index() {
    let db = AletheiaDB::new().expect("Failed to create DB");
    let node_id = db.create_node("Person", aletheiadb::PropertyMap::new()).unwrap();

    // Attempt search on non-indexed property
    // This targets src/storage/current/mod.rs:2078-2080
    let result = db.find_similar_in("non_existent_vector_prop", node_id, 5);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(format!("{}", err).contains("Vector index not found"));
}

#[test]
fn test_current_storage_wrappers() {
    // Test wrapper methods in CurrentStorage that are simple pass-throughs
    // Targets src/storage/current/mod.rs:1145-1146, 1185-1186, 1216-1217
    let storage = CurrentStorage::new();

    #[allow(deprecated)]
    storage.rebuild_adjacency();

    let _ = storage.frozen_outgoing_view();
    let _ = storage.frozen_incoming_view();
}

#[test]
fn test_import_csr_coverage() {
    // Targets src/storage/current/mod.rs:1382-1383
    let storage = CurrentStorage::new();

    let outgoing_nodes = vec![];
    let outgoing_offsets = vec![0];
    let outgoing_edges = vec![];

    let incoming_nodes = vec![];
    let incoming_offsets = vec![0];
    let incoming_edges = vec![];

    storage.import_csr(
        outgoing_nodes,
        outgoing_offsets,
        outgoing_edges,
        incoming_nodes,
        incoming_offsets,
        incoming_edges,
    );
}
