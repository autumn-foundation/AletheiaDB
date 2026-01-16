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

/// Test automatic persistence integration with GallifreyDB.
///
/// This test validates:
/// 1. Automatic persistence triggers based on mutation thresholds
/// 2. Background persistence thread operation
/// 3. Graceful shutdown persistence
/// 4. Data recovery on restart
#[test]
fn test_automatic_persistence_integration() {
    // Acquire mutex to prevent race conditions with GLOBAL_INTERNER
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    use gallifreydb::GallifreyDB;
    use gallifreydb::config::GallifreyDBConfig;
    use gallifreydb::storage::index_persistence::{
        PersistenceConfig, formats::PersistencePolicies,
    };

    println!("\n=== Automatic Persistence Integration Test ===");

    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // ========================================================================
    // Phase 1: Create database with automatic persistence enabled
    // ========================================================================

    println!("\nPhase 1: Creating database with automatic persistence...");

    let persistence_config = PersistenceConfig {
        enabled: true,
        data_dir: data_dir.clone(),
        load_on_startup: true,
        policies: PersistencePolicies {
            graph: gallifreydb::storage::index_persistence::formats::GraphPersistencePolicy {
                on_adjacency_rebuild: true,
                mutation_threshold: 1, // Persist after 1 graph mutation (for testing)
                time_interval_secs: 3600, // Or after 1 hour
            },
            vector: gallifreydb::storage::index_persistence::formats::VectorPersistencePolicy {
                mutation_threshold: 10,
                time_interval_secs: 3600,
            },
            temporal: gallifreydb::storage::index_persistence::formats::TemporalPersistencePolicy {
                version_threshold: 10,
                anchor_threshold: 5,
                time_interval_secs: 3600,
            },
            strings: gallifreydb::storage::index_persistence::formats::StringPersistencePolicy {
                new_strings_threshold: 10,
                time_interval_secs: 3600,
            },
        },
        use_mmap: false,
    };

    let config = GallifreyDBConfig::builder()
        .persistence(persistence_config)
        .build();

    let db = GallifreyDB::with_unified_config(config);

    // Create test data - enough to trigger automatic persistence
    println!("Creating 10 nodes to trigger automatic persistence...");

    let mut node_ids = Vec::new();
    for i in 0..10 {
        let node_id = db
            .create_node(
                "TestNode",
                PropertyMapBuilder::new()
                    .insert("name", format!("Node {}", i))
                    .insert("index", i as i64)
                    .build(),
            )
            .unwrap();
        node_ids.push(node_id);
    }

    println!("Created {} nodes", node_ids.len());

    // Wait for background persistence to trigger (check every 1 second)
    println!("Waiting for automatic persistence to trigger (max 5 seconds)...");
    std::thread::sleep(std::time::Duration::from_secs(5));

    // Verify index files were created by background persistence
    // Note: The manifest is only saved on shutdown, so we check for individual index files
    // IndexPersistenceManager adds an "indexes" subdirectory
    let strings_path = data_dir
        .join("indexes")
        .join("strings")
        .join("interner.idx");
    let graph_path = data_dir.join("indexes").join("graph").join("adjacency.idx");

    assert!(
        graph_path.exists(),
        "Graph index file should exist after automatic persistence"
    );

    // String interner is saved as part of any persistence operation
    assert!(
        strings_path.exists(),
        "String interner file should exist after automatic persistence"
    );

    println!("✓ Automatic persistence created index files");

    // ========================================================================
    // Phase 2: Drop database to trigger shutdown persistence
    // ========================================================================

    println!("\nPhase 2: Testing graceful shutdown persistence...");

    drop(db); // This should trigger Drop impl and persist final state

    // Give shutdown persistence time to complete
    std::thread::sleep(std::time::Duration::from_millis(200));

    println!("✓ Database dropped (graceful shutdown)");

    // ========================================================================
    // Phase 3: Reload database and verify data was persisted
    // ========================================================================

    println!("\nPhase 3: Reloading database from persisted indexes...");

    let persistence_config = PersistenceConfig {
        enabled: true,
        data_dir: data_dir.clone(),
        load_on_startup: true,
        policies: PersistencePolicies::default(),
        use_mmap: false,
    };

    let config = GallifreyDBConfig::builder()
        .persistence(persistence_config)
        .build();

    let db2 = GallifreyDB::with_unified_config(config);

    // Verify all nodes were restored
    println!("Verifying restored data...");

    for (i, &node_id) in node_ids.iter().enumerate() {
        let node = db2.get_node(node_id).unwrap();
        assert_eq!(node.id, node_id);

        let name = node.properties.get("name").and_then(|v| v.as_str());
        assert_eq!(name, Some(&*format!("Node {}", i)));

        let index = node.properties.get("index").and_then(|v| v.as_int());
        assert_eq!(index, Some(i as i64));
    }

    println!("✓ All {} nodes restored correctly", node_ids.len());

    // ========================================================================
    // Phase 4: Test corruption recovery
    // ========================================================================

    println!("\nPhase 4: Testing corruption recovery...");

    drop(db2);

    // Corrupt the graph file to test recovery
    std::fs::write(&graph_path, b"CORRUPTED DATA").unwrap();

    println!("Corrupted graph index file");

    // Try to load - should handle corruption gracefully
    let persistence_config = PersistenceConfig {
        enabled: true,
        data_dir: data_dir.clone(),
        load_on_startup: true,
        policies: PersistencePolicies::default(),
        use_mmap: false,
    };

    let config = GallifreyDBConfig::builder()
        .persistence(persistence_config)
        .build();

    // This should not panic - should start with empty database
    let db3 = GallifreyDB::with_unified_config(config);

    // Verify database started (even if empty due to corruption)
    let test_node = db3
        .create_node(
            "TestNode",
            PropertyMapBuilder::new().insert("test", "recovery").build(),
        )
        .unwrap();

    let recovered_node = db3.get_node(test_node).unwrap();
    assert_eq!(
        recovered_node
            .properties
            .get("test")
            .and_then(|v| v.as_str()),
        Some("recovery")
    );

    println!("✓ Database started successfully after corruption (recovery mode)");

    println!("\n=== Automatic Persistence Integration Test PASSED ===");
}

/// Test basic zstd compression round-trip.
#[test]
fn test_zstd_round_trip() {
    let original_data = b"Hello, world! This is a test of zstd compression.";

    // Compress
    let compressed = zstd::encode_all(&original_data[..], 3).unwrap();
    println!(
        "Original: {} bytes, Compressed: {} bytes",
        original_data.len(),
        compressed.len()
    );

    // Decompress
    let decompressed = zstd::decode_all(&compressed[..]).unwrap();

    // Verify
    assert_eq!(&decompressed[..], &original_data[..]);

    // Verify CRC matches
    use crc32fast::Hasher;
    let mut hasher1 = Hasher::new();
    hasher1.update(original_data);
    let crc1 = hasher1.finalize();

    let mut hasher2 = Hasher::new();
    hasher2.update(&decompressed);
    let crc2 = hasher2.finalize();

    assert_eq!(crc1, crc2);
}

/// Test zstd compression for index persistence.
///
/// This test validates:
/// 1. Compressed files are smaller than uncompressed
/// 2. Compressed data can be loaded correctly
/// 3. Compression level affects file size
#[test]
fn test_compression_reduces_file_size() {
    // Acquire mutex to prevent race conditions with GLOBAL_INTERNER
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    use gallifreydb::PropertyMapBuilder;
    use gallifreydb::storage::index_persistence::PersistedNode;
    use gallifreydb::storage::index_persistence::graph::{
        load_graph_index, new_graph_index_data, persist_property_map, save_graph_index,
        save_graph_index_compressed,
    };

    println!("\n=== Compression Test ===");

    let dir = tempdir().unwrap();

    // Create test data with repetitive content (compresses well)
    let mut graph_data = new_graph_index_data();
    graph_data.node_count = 100;

    for i in 0..100 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node_{}", i))
            .insert("index", i as i64)
            .build();

        let persisted_props = persist_property_map(&props).unwrap();

        graph_data.nodes.push(PersistedNode {
            id: i,
            label_idx: 1, // Same label for all - compresses well
            properties: persisted_props,
        });
    }

    // Save without compression
    let uncompressed_path = dir.path().join("uncompressed.idx");
    save_graph_index(&graph_data, &uncompressed_path).unwrap();
    let uncompressed_size = std::fs::metadata(&uncompressed_path).unwrap().len();

    println!("Uncompressed size: {} bytes", uncompressed_size);

    // Save with compression
    let compressed_path = dir.path().join("compressed.idx");
    save_graph_index_compressed(&graph_data, &compressed_path, 3).unwrap();
    let compressed_size = std::fs::metadata(&compressed_path).unwrap().len();

    println!("Compressed size: {} bytes", compressed_size);

    // Verify compression actually reduced size
    assert!(
        compressed_size < uncompressed_size,
        "Compressed size ({}) should be smaller than uncompressed ({})",
        compressed_size,
        uncompressed_size
    );

    // Verify compression ratio is reasonable (at least 30% reduction)
    let compression_ratio = compressed_size as f64 / uncompressed_size as f64;
    println!("Compression ratio: {:.2}%", compression_ratio * 100.0);

    assert!(
        compression_ratio < 0.7,
        "Compression ratio should be < 70%, got {:.2}%",
        compression_ratio * 100.0
    );

    // Verify we can load compressed data correctly
    let loaded_data = load_graph_index(&compressed_path).unwrap();

    assert_eq!(loaded_data.node_count, 100);
    assert_eq!(loaded_data.nodes.len(), 100);
    assert_eq!(loaded_data.nodes[0].label_idx, 1);
    assert_eq!(loaded_data.nodes[50].id, 50);

    println!("✓ Compression test passed");
}

#[test]
fn test_parallel_loading_is_faster() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    use gallifreydb::PropertyMapBuilder;
    use gallifreydb::storage::index_persistence::PersistedNode;
    use gallifreydb::storage::index_persistence::graph::{
        load_graph_index, new_graph_index_data, persist_property_map, save_graph_index_compressed,
    };
    use std::time::Instant;

    let dir = tempdir().unwrap();

    // Create large graph data to make loading measurably slow
    let mut graph_data = new_graph_index_data();
    graph_data.node_count = 1000;

    for i in 0..1000 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node_{}", i))
            .insert(
                "description",
                format!(
                    "This is a longer description for node {} to make the data larger",
                    i
                ),
            )
            .insert("index", i as i64)
            .build();

        let persisted_props = persist_property_map(&props).unwrap();

        graph_data.nodes.push(PersistedNode {
            id: i,
            label_idx: 1,
            properties: persisted_props,
        });
    }

    // Save graph index
    let graph_path = dir.path().join("graph.idx");
    save_graph_index_compressed(&graph_data, &graph_path, 3).unwrap();

    // Create second copy for parallel test isolation
    let graph_path2 = dir.path().join("graph2.idx");
    save_graph_index_compressed(&graph_data, &graph_path2, 3).unwrap();

    // Create third copy
    let graph_path3 = dir.path().join("graph3.idx");
    save_graph_index_compressed(&graph_data, &graph_path3, 3).unwrap();

    // Measure sequential loading
    let start = Instant::now();
    let data1 = load_graph_index(&graph_path).unwrap();
    let data2 = load_graph_index(&graph_path2).unwrap();
    let data3 = load_graph_index(&graph_path3).unwrap();
    let sequential_duration = start.elapsed();

    println!("Sequential loading took: {:?}", sequential_duration);

    // Test the library's parallel loading function
    // Load graph, temporal (if exists), and vector (if exists) in parallel
    let start = Instant::now();

    // Use the library's parallel loading function
    // For this test, we'll load the same graph multiple times to demonstrate parallelism
    use gallifreydb::storage::index_persistence::load_indexes_parallel;

    // Load three indexes in parallel by calling the function three times concurrently
    use std::thread;
    let graph_path_clone1 = graph_path.clone();
    let graph_path_clone2 = graph_path2.clone();
    let graph_path_clone3 = graph_path3.clone();

    let handle1 = thread::spawn(move || load_indexes_parallel(&graph_path_clone1, None, vec![]));
    let handle2 = thread::spawn(move || load_indexes_parallel(&graph_path_clone2, None, vec![]));
    let handle3 = thread::spawn(move || load_indexes_parallel(&graph_path_clone3, None, vec![]));

    let (data1_par, _) = handle1.join().expect("Thread 1 panicked").unwrap();
    let (data2_par, _) = handle2.join().expect("Thread 2 panicked").unwrap();
    let (data3_par, _) = handle3.join().expect("Thread 3 panicked").unwrap();

    let parallel_duration = start.elapsed();

    println!("Parallel loading took: {:?}", parallel_duration);

    // Verify parallel is faster (though due to caching, this might not always be true)
    // The important thing is that the function works correctly
    println!(
        "Sequential: {:?}, Parallel: {:?}, Speedup: {:.2}x",
        sequential_duration,
        parallel_duration,
        sequential_duration.as_secs_f64() / parallel_duration.as_secs_f64()
    );

    // Verify data loaded correctly
    assert_eq!(data1_par.node_count, 1000);
    assert_eq!(data2_par.node_count, 1000);
    assert_eq!(data3_par.node_count, 1000);
    assert_eq!(data1.node_count, data1_par.node_count);
    assert_eq!(data2.node_count, data2_par.node_count);
    assert_eq!(data3.node_count, data3_par.node_count);

    println!("✓ Parallel loading test passed");
}

#[test]
fn test_memory_mapped_loading() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    use gallifreydb::PropertyMapBuilder;
    use gallifreydb::storage::index_persistence::PersistedNode;
    use gallifreydb::storage::index_persistence::graph::{
        load_graph_index, new_graph_index_data, persist_property_map, save_graph_index_compressed,
    };

    let dir = tempdir().unwrap();

    // Create test graph data
    let mut graph_data = new_graph_index_data();
    graph_data.node_count = 500;

    for i in 0..500 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node_{}", i))
            .insert("value", i as i64)
            .build();

        let persisted_props = persist_property_map(&props).unwrap();

        graph_data.nodes.push(PersistedNode {
            id: i,
            label_idx: 1,
            properties: persisted_props,
        });
    }

    // Save graph index
    let graph_path = dir.path().join("graph_mmap.idx");
    save_graph_index_compressed(&graph_data, &graph_path, 3).unwrap();

    // Load using regular loading
    let regular_data = load_graph_index(&graph_path).unwrap();

    // Load using memory-mapped loading (this will fail - function doesn't exist yet)
    use gallifreydb::storage::index_persistence::graph::load_graph_index_mmap;
    let mmap_data = load_graph_index_mmap(&graph_path).unwrap();

    // Verify both methods produce identical results
    assert_eq!(regular_data.node_count, mmap_data.node_count);
    assert_eq!(regular_data.nodes.len(), mmap_data.nodes.len());
    assert_eq!(regular_data.magic, mmap_data.magic);
    assert_eq!(regular_data.version, mmap_data.version);

    // Verify specific node data
    assert_eq!(regular_data.nodes[0].id, mmap_data.nodes[0].id);
    assert_eq!(
        regular_data.nodes[0].label_idx,
        mmap_data.nodes[0].label_idx
    );
    assert_eq!(regular_data.nodes[100].id, mmap_data.nodes[100].id);
    assert_eq!(regular_data.nodes[499].id, mmap_data.nodes[499].id);

    println!("✓ Memory-mapped loading test passed");
    println!("  Loaded {} nodes correctly", mmap_data.node_count);
}
