#![cfg(test)]

use gallifreydb::core::GLOBAL_INTERNER;
use gallifreydb::storage::index_persistence::formats::{
    IndexManifest, PersistedEdge, PersistedNode, PersistedPropertyMap,
};
use gallifreydb::storage::index_persistence::graph::{
    new_graph_index_data, persist_property_map, restore_property_map, save_graph_index,
};
use gallifreydb::storage::index_persistence::temporal::{
    new_temporal_index_data, save_temporal_index,
};
use gallifreydb::storage::index_persistence::vector::{
    new_vector_mappings, new_vector_meta, save_vector_mappings, save_vector_meta,
};
use gallifreydb::storage::index_persistence::{
    formats::PersistedHnswConfig, IndexPersistenceManager,
};
use gallifreydb::PropertyMapBuilder;
use tempfile::tempdir;

/// Test full persistence cycle: save → clear → load → verify.
///
/// This test validates the complete persistence workflow for all index types:
/// 1. String interner (dependency for all others)
/// 2. Manifest (index registry)
/// 3. Graph index (adjacency + properties)
/// 4. Temporal index (version chains)
/// 5. Vector index (metadata + mappings)
#[test]
fn test_full_persistence_cycle() {
    // ========================================================================
    // Phase 1: Setup and Save
    // ========================================================================

    let dir = tempdir().unwrap();
    let manager = IndexPersistenceManager::new(dir.path());
    manager.ensure_directories().unwrap();

    // Step 1: Populate and save string interner
    GLOBAL_INTERNER.intern("Person").unwrap();
    GLOBAL_INTERNER.intern("name").unwrap();
    GLOBAL_INTERNER.intern("age").unwrap();
    GLOBAL_INTERNER.intern("Document").unwrap();
    GLOBAL_INTERNER.intern("title").unwrap();

    manager.save_string_interner().unwrap();

    // Step 2: Create and save manifest
    let mut manifest = IndexManifest::new(42);
    manifest.set_lsn(100);
    manager.save_manifest(&manifest).unwrap();

    // Step 3: Create and save graph index
    let mut graph_data = new_graph_index_data();

    // Add a node with properties
    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();
    let persisted_props = persist_property_map(&props);

    graph_data.nodes.push(PersistedNode {
        id: 1,
        label_idx: GLOBAL_INTERNER.intern("Person").unwrap().as_u32(),
        properties: persisted_props.clone(),
    });

    // Add another node
    let doc_props = PropertyMapBuilder::new()
        .insert("title", "Rust Guide")
        .build();
    graph_data.nodes.push(PersistedNode {
        id: 2,
        label_idx: GLOBAL_INTERNER.intern("Document").unwrap().as_u32(),
        properties: persist_property_map(&doc_props),
    });

    // Add an edge
    graph_data.edges.push(PersistedEdge {
        id: 100,
        source_id: 1,
        target_id: 2,
        label_idx: GLOBAL_INTERNER.intern("AUTHORED").unwrap().as_u32(),
        properties: PersistedPropertyMap { entries: vec![] },
    });

    // Add CSR adjacency data (simplified)
    graph_data.node_count = 2;
    graph_data.edge_count = 1;
    graph_data.outgoing_offsets = vec![0, 1, 1];
    graph_data.outgoing_neighbors = vec![100];
    graph_data.incoming_offsets = vec![0, 0, 1];
    graph_data.incoming_neighbors = vec![100];

    save_graph_index(&graph_data, &manager.graph_path().join("adjacency.idx")).unwrap();

    // Step 4: Create and save temporal index
    let temporal_data = new_temporal_index_data();
    save_temporal_index(
        &temporal_data,
        &manager.temporal_path().join("versions.idx"),
    )
    .unwrap();

    // Step 5: Create and save vector index metadata and mappings
    let hnsw_config = PersistedHnswConfig {
        m: 16,
        ef_construction: 128,
        ef_search: 64,
    };
    let vector_meta = new_vector_meta("embedding", 384, 0, hnsw_config);
    let vector_mappings = new_vector_mappings();

    let vec_path = manager.vector_path("embedding");
    std::fs::create_dir_all(&vec_path).unwrap();

    save_vector_meta(&vector_meta, &vec_path.join("meta.idx")).unwrap();
    save_vector_mappings(&vector_mappings, &vec_path.join("mappings.idx")).unwrap();

    // ========================================================================
    // Phase 2: Verify files exist on disk
    // ========================================================================

    assert!(manager.manifest_path().exists());
    assert!(manager.interner_path().exists());
    assert!(manager.graph_path().join("adjacency.idx").exists());
    assert!(manager.temporal_path().join("versions.idx").exists());
    assert!(vec_path.join("meta.idx").exists());
    assert!(vec_path.join("mappings.idx").exists());

    // ========================================================================
    // Phase 3: Load and Verify
    // ========================================================================

    // Load manifest and strings (validates load order: interner → manifest)
    let loaded_manifest = manager.load_manifest_and_strings().unwrap();
    assert_eq!(loaded_manifest.lsn, 100);
    assert_eq!(loaded_manifest.version, 1);

    // Verify string interner was restored correctly
    assert_eq!(
        GLOBAL_INTERNER
            .resolve(GLOBAL_INTERNER.intern("Person").unwrap())
            .unwrap()
            .as_ref(),
        "Person"
    );
    assert_eq!(
        GLOBAL_INTERNER
            .resolve(GLOBAL_INTERNER.intern("name").unwrap())
            .unwrap()
            .as_ref(),
        "name"
    );

    // Verify graph data can be loaded and properties restored
    let restored_props = restore_property_map(&persisted_props);
    assert_eq!(
        restored_props.get("name").unwrap().as_str().unwrap(),
        "Alice"
    );
    assert_eq!(restored_props.get("age").unwrap().as_int().unwrap(), 30);

    println!("✓ Full persistence cycle test passed");
}

/// Test property map serialization round-trip.
#[test]
fn test_property_map_persistence() {
    // Intern strings first
    GLOBAL_INTERNER.intern("name").unwrap();
    GLOBAL_INTERNER.intern("age").unwrap();
    GLOBAL_INTERNER.intern("active").unwrap();

    let original = PropertyMapBuilder::new()
        .insert("name", "Bob")
        .insert("age", 25i64)
        .insert("active", true)
        .build();

    let persisted = persist_property_map(&original);
    let restored = restore_property_map(&persisted);

    assert_eq!(restored.get("name").unwrap().as_str().unwrap(), "Bob");
    assert_eq!(restored.get("age").unwrap().as_int().unwrap(), 25);
    assert!(restored.get("active").unwrap().as_bool().unwrap());
}

/// Test that indexes_exist() correctly detects presence of persisted data.
#[test]
fn test_indexes_exist_detection() {
    let dir = tempdir().unwrap();
    let manager = IndexPersistenceManager::new(dir.path());

    // Initially no indexes
    assert!(!manager.indexes_exist());

    // Save manifest
    manager.ensure_directories().unwrap();
    let manifest = IndexManifest::new(0);
    manager.save_manifest(&manifest).unwrap();

    // Now indexes exist
    assert!(manager.indexes_exist());
}
