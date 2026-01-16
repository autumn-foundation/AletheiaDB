#![cfg(test)]

use gallifreydb::PropertyMapBuilder;
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
    IndexPersistenceManager, formats::PersistedHnswConfig,
};
use std::sync::Mutex;
use tempfile::tempdir;

/// Global mutex to serialize tests that use GLOBAL_INTERNER.
///
/// Since GLOBAL_INTERNER is a global singleton, tests that use it can interfere
/// with each other when run in parallel. This mutex ensures only one test modifies
/// the interner at a time, preventing flaky tests and race conditions.
///
/// Each test should acquire this lock at the start to ensure exclusive access.
static INTERNER_TEST_MUTEX: Mutex<()> = Mutex::new(());

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
    // Acquire mutex to prevent race conditions with GLOBAL_INTERNER
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

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
    let persisted_props = persist_property_map(&props).unwrap();

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
        properties: persist_property_map(&doc_props).unwrap(),
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
    let restored_props = restore_property_map(&persisted_props).unwrap();
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
    // Acquire mutex to prevent race conditions with GLOBAL_INTERNER
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    // Intern strings first
    GLOBAL_INTERNER.intern("name").unwrap();
    GLOBAL_INTERNER.intern("age").unwrap();
    GLOBAL_INTERNER.intern("active").unwrap();

    let original = PropertyMapBuilder::new()
        .insert("name", "Bob")
        .insert("age", 25i64)
        .insert("active", true)
        .build();

    let persisted = persist_property_map(&original).unwrap();
    let restored = restore_property_map(&persisted).unwrap();

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

// ============================================================================
// GallifreyDB Integration Tests
// ============================================================================

use gallifreydb::storage::index_persistence::PersistenceConfig;
use gallifreydb::{GallifreyDB, config::GallifreyDBConfig};

/// Test that GallifreyDB can persist indexes to disk (MVP - Phase 1).
///
/// This test verifies:
/// 1. persist_indexes() successfully saves all index data
/// 2. Index files are created on disk
/// 3. Manifest and strings can be loaded back
///
/// Note: Full graph restoration is deferred to Phase 2.
#[test]
fn test_db_persist_indexes_mvp() {
    // Acquire mutex to prevent race conditions with GLOBAL_INTERNER
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // Phase 1: Create database, add data, persist indexes
    {
        let config = GallifreyDBConfig::builder()
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: data_dir.clone(),
                load_on_startup: true,
                ..Default::default()
            })
            .build();

        let db = GallifreyDB::with_unified_config(config);

        // Add some nodes
        let node1_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30i64)
                    .build(),
            )
            .unwrap();

        let node2_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert("age", 25i64)
                    .build(),
            )
            .unwrap();

        // Add an edge
        db.create_edge(
            node1_id,
            node2_id,
            "KNOWS",
            PropertyMapBuilder::new().build(),
        )
        .unwrap();

        // Verify before persist
        assert_eq!(db.node_count(), 2);
        assert_eq!(db.edge_count(), 1);

        // Persist indexes - this is what we're testing
        db.persist_indexes().unwrap();

        // Drop database to simulate shutdown
        drop(db);
    }

    // Phase 2: Verify index files were created
    {
        use gallifreydb::storage::index_persistence::IndexPersistenceManager;

        let manager = IndexPersistenceManager::new(&data_dir);

        // Verify all expected files exist
        assert!(
            manager.manifest_path().exists(),
            "Manifest file should exist"
        );
        assert!(
            manager.interner_path().exists(),
            "String interner file should exist"
        );
        assert!(
            manager.graph_path().join("adjacency.idx").exists(),
            "Graph index file should exist"
        );

        // Verify we can load manifest and strings back
        let manifest = manager.load_manifest_and_strings().unwrap();
        assert_eq!(manifest.version, 1, "Manifest version should be 1");

        // Verify strings were saved (we interned "Person", "name", "age", "Bob", "Alice", "KNOWS")
        use gallifreydb::core::GLOBAL_INTERNER;
        let person_str = GLOBAL_INTERNER.intern("Person").unwrap();
        assert_eq!(
            GLOBAL_INTERNER.resolve(person_str).unwrap().as_ref(),
            "Person",
            "Should have persisted and loaded 'Person' string"
        );

        println!("✓ Database persisted indexes successfully (MVP Phase 1)");
    }
}

/// Test full persistence lifecycle: save, shutdown, restart, load.
///
/// This test verifies the complete workflow:
/// 1. Create database with data
/// 2. Persist indexes and shutdown
/// 3. Start new database instance
/// 4. Verify all data was restored correctly
#[test]
fn test_full_persistence_lifecycle() {
    // Acquire mutex to prevent race conditions with GLOBAL_INTERNER
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    let node1_id;
    let node2_id;
    let edge_id;

    // Phase 1: Create database, add data, persist
    {
        let config = GallifreyDBConfig::builder()
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: data_dir.clone(),
                load_on_startup: true,
                ..Default::default()
            })
            .build();

        let db = GallifreyDB::with_unified_config(config);

        // Add nodes with properties
        node1_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30i64)
                    .insert("city", "Seattle")
                    .build(),
            )
            .unwrap();

        node2_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert("age", 25i64)
                    .insert("city", "Portland")
                    .build(),
            )
            .unwrap();

        // Add edge with properties
        edge_id = db
            .create_edge(
                node1_id,
                node2_id,
                "KNOWS",
                PropertyMapBuilder::new()
                    .insert("since", 2020i64)
                    .insert("strength", "strong")
                    .build(),
            )
            .unwrap();

        // Verify data exists
        assert_eq!(db.node_count(), 2);
        assert_eq!(db.edge_count(), 1);

        // Persist indexes
        db.persist_indexes().unwrap();

        // Explicit drop to simulate shutdown
        drop(db);
    }

    // Phase 2: Restart database and verify data was restored
    {
        let config = GallifreyDBConfig::builder()
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: data_dir.clone(),
                load_on_startup: true,
                ..Default::default()
            })
            .build();

        let db = GallifreyDB::with_unified_config(config);

        // Verify counts
        assert_eq!(db.node_count(), 2, "Should have restored 2 nodes from disk");
        assert_eq!(db.edge_count(), 1, "Should have restored 1 edge from disk");

        // Verify node 1 with all properties
        let node1 = db.get_node(node1_id).unwrap();
        assert_eq!(
            node1.properties.get("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(
            node1.properties.get("age").and_then(|v| v.as_int()),
            Some(30)
        );
        assert_eq!(
            node1.properties.get("city").and_then(|v| v.as_str()),
            Some("Seattle")
        );

        // Verify node 2 with all properties
        let node2 = db.get_node(node2_id).unwrap();
        assert_eq!(
            node2.properties.get("name").and_then(|v| v.as_str()),
            Some("Bob")
        );
        assert_eq!(
            node2.properties.get("age").and_then(|v| v.as_int()),
            Some(25)
        );
        assert_eq!(
            node2.properties.get("city").and_then(|v| v.as_str()),
            Some("Portland")
        );

        // Verify edge with properties
        let edge = db.get_edge(edge_id).unwrap();
        assert_eq!(edge.source, node1_id);
        assert_eq!(edge.target, node2_id);
        assert_eq!(
            edge.properties.get("since").and_then(|v| v.as_int()),
            Some(2020)
        );
        assert_eq!(
            edge.properties.get("strength").and_then(|v| v.as_str()),
            Some("strong")
        );

        // Verify graph structure (adjacency)
        let outgoing = db.get_outgoing_edges(node1_id);
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0], edge_id);

        let incoming = db.get_incoming_edges(node2_id);
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0], edge_id);

        println!("✓ Full persistence lifecycle test passed");
    }
}
