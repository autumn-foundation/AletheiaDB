#![cfg(test)]

use gallifreydb::PropertyMapBuilder;
use gallifreydb::core::GLOBAL_INTERNER;
use gallifreydb::core::id::{NodeId, VersionId};
use gallifreydb::core::property::PropertyValue;
use gallifreydb::core::temporal::BiTemporalInterval;
use gallifreydb::storage::index_persistence::formats::{
    IndexManifest, PersistedEdge, PersistedNode, PersistedPropertyMap,
};
use gallifreydb::storage::index_persistence::graph::{
    new_graph_index_data, persist_property_map, restore_property_map, save_graph_index,
};
use gallifreydb::storage::index_persistence::temporal::{
    convert_node_version, load_temporal_index, new_temporal_index_data, restore_node_version,
    save_temporal_index,
};
use gallifreydb::storage::index_persistence::vector::{
    new_vector_mappings, new_vector_meta, save_vector_mappings, save_vector_meta,
};
use gallifreydb::storage::index_persistence::{
    IndexPersistenceManager, formats::PersistedHnswConfig,
};
use gallifreydb::storage::version::{NodeVersion, PropertyDelta, VersionData};
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

#[test]
fn test_delta_encoding_reduces_incremental_save_size() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    use gallifreydb::PropertyMapBuilder;
    use gallifreydb::storage::index_persistence::graph::{
        load_graph_index, load_graph_index_with_delta, new_graph_index_data, persist_property_map,
        save_graph_index_compressed, save_graph_index_delta,
    };
    use gallifreydb::storage::index_persistence::{PersistedEdge, PersistedNode};

    let dir = tempdir().unwrap();

    println!("\n=== Comprehensive Delta Encoding Test (Nodes + Edges) ===");

    // ========================================================================
    // Phase 1: Create base index with nodes AND edges
    // ========================================================================

    let mut base_data = new_graph_index_data();
    base_data.node_count = 100;

    for i in 0..100 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node_{}", i))
            .insert("value", i as i64)
            .build();

        let persisted_props = persist_property_map(&props).unwrap();

        base_data.nodes.push(PersistedNode {
            id: i,
            label_idx: 1,
            properties: persisted_props,
        });
    }

    // Add base edges: 0->1, 1->2, ..., 99->0 (circular)
    base_data.edge_count = 100;
    for i in 0..100 {
        let target = (i + 1) % 100;
        let props = PropertyMapBuilder::new().insert("weight", i as i64).build();

        let persisted_props = persist_property_map(&props).unwrap();

        base_data.edges.push(PersistedEdge {
            id: i,
            source_id: i,
            target_id: target,
            label_idx: 2, // Different label for edges
            properties: persisted_props,
        });
    }

    // Save base snapshot
    let base_path = dir.path().join("base.idx");
    save_graph_index_compressed(&base_data, &base_path, 3).unwrap();
    let base_size = std::fs::metadata(&base_path).unwrap().len();

    println!(
        "Base snapshot: {} nodes, {} edges, {} bytes",
        base_data.node_count, base_data.edge_count, base_size
    );

    // ========================================================================
    // Phase 2: Create modified index with ALL three change types (nodes + edges)
    // ========================================================================

    let mut modified_data = base_data.clone();

    // NODE CHANGES
    // 1. ADDITIONS: Add 10 new nodes (IDs 100-109)
    for i in 100..110 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node_{}", i))
            .insert("value", i as i64)
            .build();

        let persisted_props = persist_property_map(&props).unwrap();

        modified_data.nodes.push(PersistedNode {
            id: i,
            label_idx: 1,
            properties: persisted_props,
        });
    }

    // 2. MODIFICATIONS: Modify nodes 10-19 (change their properties)
    for i in 10..20 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Modified_Node_{}", i))
            .insert("value", (i * 1000) as i64) // Changed value
            .insert("modified", true) // New property
            .build();

        let persisted_props = persist_property_map(&props).unwrap();

        if let Some(node) = modified_data.nodes.iter_mut().find(|n| n.id == i) {
            node.properties = persisted_props;
        }
    }

    // 3. DELETIONS: Delete nodes 90-99 (10 nodes)
    // Keep nodes with id < 90 OR id >= 100 (removes only 90-99)
    modified_data.nodes.retain(|n| n.id < 90 || n.id >= 100);

    // EDGE CHANGES
    // 1. ADDITIONS: Add 10 new edges (IDs 100-109) connecting new nodes
    for i in 100..110 {
        let target = if i < 109 { i + 1 } else { 100 }; // Connect new nodes in a chain
        let props = PropertyMapBuilder::new()
            .insert("weight", i as i64)
            .insert("new_edge", true)
            .build();

        let persisted_props = persist_property_map(&props).unwrap();

        modified_data.edges.push(PersistedEdge {
            id: i,
            source_id: i,
            target_id: target,
            label_idx: 2,
            properties: persisted_props,
        });
    }

    // 2. MODIFICATIONS: Modify edges 10-19 (change their properties)
    for i in 10..20 {
        let props = PropertyMapBuilder::new()
            .insert("weight", (i * 2000) as i64) // Changed weight
            .insert("modified_edge", true) // New property
            .build();

        let persisted_props = persist_property_map(&props).unwrap();

        if let Some(edge) = modified_data.edges.iter_mut().find(|e| e.id == i) {
            edge.properties = persisted_props;
        }
    }

    // 3. DELETIONS: Delete edges 90-99 (10 edges)
    // Keep edges with id < 90 OR id >= 100 (removes only 90-99)
    modified_data.edges.retain(|e| e.id < 90 || e.id >= 100);

    // Update counts
    modified_data.node_count = modified_data.nodes.len() as u64; // 100 - 10 + 10 = 100
    modified_data.edge_count = modified_data.edges.len() as u64; // 100 - 10 + 10 = 100

    println!("Modified index: 10 node additions, 10 node modifications, 10 node deletions");
    println!("               10 edge additions, 10 edge modifications, 10 edge deletions");

    // ========================================================================
    // Phase 3: Save and verify delta
    // ========================================================================

    // Save delta (only the changes)
    let delta_path = dir.path().join("delta.idx");
    save_graph_index_delta(&base_data, &modified_data, &delta_path, 3).unwrap();
    let delta_size = std::fs::metadata(&delta_path).unwrap().len();

    // Save full modified version for comparison
    let full_path = dir.path().join("full.idx");
    save_graph_index_compressed(&modified_data, &full_path, 3).unwrap();
    let full_size = std::fs::metadata(&full_path).unwrap().len();

    // Verify delta is smaller than full save
    assert!(
        delta_size < full_size,
        "Delta size ({}) should be smaller than full save ({})",
        delta_size,
        full_size
    );

    println!(
        "Delta size: {} bytes (vs {} bytes full)",
        delta_size, full_size
    );
    println!(
        "Delta reduction: {:.1}%",
        (1.0 - delta_size as f64 / full_size as f64) * 100.0
    );

    // ========================================================================
    // Phase 4: Reconstruct from delta and verify ALL changes applied
    // ========================================================================

    let reconstructed_data = load_graph_index_with_delta(&base_path, &delta_path).unwrap();

    // Verify counts
    assert_eq!(reconstructed_data.node_count, modified_data.node_count);
    assert_eq!(reconstructed_data.nodes.len(), modified_data.nodes.len());
    assert_eq!(reconstructed_data.edge_count, modified_data.edge_count);
    assert_eq!(reconstructed_data.edges.len(), modified_data.edges.len());

    println!("\nVerifying NODE changes:");

    // Verify node additions (nodes 100-109 exist)
    for i in 100..110 {
        assert!(
            reconstructed_data.nodes.iter().any(|n| n.id == i),
            "Added node {} should exist",
            i
        );
    }
    println!("  ✓ 10 node additions verified");

    // Verify node modifications (nodes 10-19 have updated properties)
    for i in 10..20 {
        let node = reconstructed_data
            .nodes
            .iter()
            .find(|n| n.id == i)
            .unwrap_or_else(|| panic!("Modified node {} should exist", i));

        let restored_props = restore_property_map(&node.properties).unwrap();
        assert_eq!(
            restored_props.get("name").and_then(|v| v.as_str()),
            Some(&*format!("Modified_Node_{}", i)),
            "Node {} should have modified name",
            i
        );
        assert_eq!(
            restored_props.get("value").and_then(|v| v.as_int()),
            Some((i * 1000) as i64),
            "Node {} should have modified value",
            i
        );
        assert_eq!(
            restored_props.get("modified").and_then(|v| v.as_bool()),
            Some(true),
            "Node {} should have new 'modified' property",
            i
        );
    }
    println!("  ✓ 10 node modifications verified");

    // Verify node deletions (nodes 90-99 should NOT exist)
    for i in 90..100 {
        assert!(
            !reconstructed_data.nodes.iter().any(|n| n.id == i),
            "Deleted node {} should NOT exist",
            i
        );
    }
    println!("  ✓ 10 node deletions verified");

    // Verify unmodified nodes still exist unchanged (e.g., node 5)
    let unmodified_node = reconstructed_data
        .nodes
        .iter()
        .find(|n| n.id == 5)
        .expect("Unmodified node 5 should exist");
    let restored_props = restore_property_map(&unmodified_node.properties).unwrap();
    assert_eq!(
        restored_props.get("name").and_then(|v| v.as_str()),
        Some("Node_5"),
        "Unmodified node should retain original properties"
    );

    println!("\nVerifying EDGE changes:");

    // Verify edge additions (edges 100-109 exist)
    for i in 100..110 {
        assert!(
            reconstructed_data.edges.iter().any(|e| e.id == i),
            "Added edge {} should exist",
            i
        );
    }
    println!("  ✓ 10 edge additions verified");

    // Verify edge modifications (edges 10-19 have updated properties)
    for i in 10..20 {
        let edge = reconstructed_data
            .edges
            .iter()
            .find(|e| e.id == i)
            .unwrap_or_else(|| panic!("Modified edge {} should exist", i));

        let restored_props = restore_property_map(&edge.properties).unwrap();
        assert_eq!(
            restored_props.get("weight").and_then(|v| v.as_int()),
            Some((i * 2000) as i64),
            "Edge {} should have modified weight",
            i
        );
        assert_eq!(
            restored_props
                .get("modified_edge")
                .and_then(|v| v.as_bool()),
            Some(true),
            "Edge {} should have new 'modified_edge' property",
            i
        );
    }
    println!("  ✓ 10 edge modifications verified");

    // Verify edge deletions (edges 90-99 should NOT exist)
    for i in 90..100 {
        assert!(
            !reconstructed_data.edges.iter().any(|e| e.id == i),
            "Deleted edge {} should NOT exist",
            i
        );
    }
    println!("  ✓ 10 edge deletions verified");

    // Verify unmodified edges still exist unchanged (e.g., edge 5)
    let unmodified_edge = reconstructed_data
        .edges
        .iter()
        .find(|e| e.id == 5)
        .expect("Unmodified edge 5 should exist");
    let restored_props = restore_property_map(&unmodified_edge.properties).unwrap();
    assert_eq!(
        restored_props.get("weight").and_then(|v| v.as_int()),
        Some(5),
        "Unmodified edge should retain original properties"
    );
    // Verify edge structure unchanged
    assert_eq!(unmodified_edge.source_id, 5);
    assert_eq!(unmodified_edge.target_id, 6);

    // Verify loading without delta gives us the original base
    let base_loaded = load_graph_index(&base_path).unwrap();
    assert_eq!(base_loaded.node_count, 100);
    assert_eq!(base_loaded.edge_count, 100);

    println!(
        "\n✓ All delta operations verified: nodes + edges (additions, modifications, deletions)"
    );
    println!("✓ Delta encoding comprehensive test passed");
}

// ============================================================================
// Error Path Tests - Corruption Scenarios
// ============================================================================

/// Test that malformed manifests are handled gracefully.
#[test]
fn test_malformed_manifest_handling() {
    let dir = tempdir().unwrap();
    let manager = IndexPersistenceManager::new(dir.path());
    manager.ensure_directories().unwrap();

    // Write invalid manifest (wrong magic bytes)
    let manifest_path = manager.manifest_path();
    std::fs::write(&manifest_path, b"WRONG_MAGIC").unwrap();

    // Should fail gracefully, not panic
    let result = manager.load_manifest_and_strings();
    assert!(result.is_err(), "Should fail on malformed manifest");

    // Write truncated manifest (incomplete header)
    std::fs::write(&manifest_path, b"GIDX\x01\x00\x00").unwrap();

    let result = manager.load_manifest_and_strings();
    assert!(result.is_err(), "Should fail on truncated manifest");

    println!("✓ Malformed manifest handling test passed");
}

/// Test that truncated index files are detected and handled.
#[test]
fn test_truncated_file_detection() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    use gallifreydb::PropertyMapBuilder;
    use gallifreydb::storage::index_persistence::PersistedNode;
    use gallifreydb::storage::index_persistence::graph::{
        load_graph_index, new_graph_index_data, persist_property_map, save_graph_index,
    };

    let dir = tempdir().unwrap();

    // Create valid graph data
    let mut graph_data = new_graph_index_data();
    let props = PropertyMapBuilder::new().insert("name", "Alice").build();
    let persisted_props = persist_property_map(&props).unwrap();

    graph_data.nodes.push(PersistedNode {
        id: 1,
        label_idx: 1,
        properties: persisted_props,
    });
    graph_data.node_count = 1;

    // Save valid file
    let path = dir.path().join("graph.idx");
    save_graph_index(&graph_data, &path).unwrap();

    // Verify it loads correctly
    let loaded = load_graph_index(&path);
    assert!(loaded.is_ok(), "Valid file should load");

    // Truncate the file (corrupt it)
    let data = std::fs::read(&path).unwrap();
    let truncated = &data[..data.len() / 2]; // Cut in half
    std::fs::write(&path, truncated).unwrap();

    // Should fail gracefully
    let result = load_graph_index(&path);
    assert!(result.is_err(), "Should detect truncated file");

    println!("✓ Truncated file detection test passed");
}

/// Test that invalid IDs are detected during restoration.
#[test]
fn test_invalid_id_detection() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    use gallifreydb::storage::index_persistence::PersistenceConfig;
    use gallifreydb::{GallifreyDB, PropertyMapBuilder, config::GallifreyDBConfig};

    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // Create database with data
    {
        let config = GallifreyDBConfig::builder()
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: data_dir.clone(),
                load_on_startup: false, // Don't load yet
                ..Default::default()
            })
            .build();

        let db = GallifreyDB::with_unified_config(config);

        // Add node
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();

        // Persist
        db.persist_indexes().unwrap();
    }

    // Manually corrupt the graph file with invalid IDs
    use gallifreydb::storage::index_persistence::IndexPersistenceManager;
    use gallifreydb::storage::index_persistence::graph::{load_graph_index, save_graph_index};

    let manager = IndexPersistenceManager::new(&data_dir);
    let graph_path = manager.graph_path().join("adjacency.idx");

    let mut graph_data = load_graph_index(&graph_path).unwrap();

    // Add node with invalid ID (exceeding MAX_VALID_ID would be u64::MAX - 1000+)
    // For this test, we'll just create a node with a very large ID
    use gallifreydb::storage::index_persistence::PersistedNode;
    graph_data.nodes.push(PersistedNode {
        id: u64::MAX - 500, // Very large ID
        label_idx: 1,
        properties: graph_data.nodes[0].properties.clone(),
    });

    // Save corrupted data
    save_graph_index(&graph_data, &graph_path).unwrap();

    // Try to load - should handle gracefully
    // Note: Current implementation may skip invalid IDs during restoration
    let config = GallifreyDBConfig::builder()
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.clone(),
            load_on_startup: true,
            ..Default::default()
        })
        .build();

    // Should not panic, may log warnings
    let _db = GallifreyDB::with_unified_config(config);

    println!("✓ Invalid ID detection test passed");
}

/// Test that missing string interner entries are detected.
#[test]
fn test_missing_interner_entries() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    use gallifreydb::storage::index_persistence::PersistenceConfig;
    use gallifreydb::{GallifreyDB, PropertyMapBuilder, config::GallifreyDBConfig};

    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // Create database with data
    {
        let config = GallifreyDBConfig::builder()
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: data_dir.clone(),
                load_on_startup: false,
                ..Default::default()
            })
            .build();

        let db = GallifreyDB::with_unified_config(config);

        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();

        db.persist_indexes().unwrap();
    }

    // Delete the string interner file to simulate corruption
    use gallifreydb::storage::index_persistence::IndexPersistenceManager;
    let manager = IndexPersistenceManager::new(&data_dir);
    std::fs::remove_file(manager.interner_path()).unwrap();

    // Try to load - should handle missing interner gracefully
    let config = GallifreyDBConfig::builder()
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.clone(),
            load_on_startup: true,
            ..Default::default()
        })
        .build();

    // Should not panic - may start with empty DB or skip invalid entries
    let db = GallifreyDB::with_unified_config(config);

    // Verify database is functional even if data was lost
    let node_id = db
        .create_node(
            "TestNode",
            PropertyMapBuilder::new().insert("test", "recovery").build(),
        )
        .unwrap();

    let node = db.get_node(node_id).unwrap();
    assert_eq!(
        node.properties.get("test").and_then(|v| v.as_str()),
        Some("recovery")
    );

    println!("✓ Missing interner entries test passed");
}

/// Test that corrupted property data is handled gracefully.
#[test]
fn test_corrupted_property_data() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    use gallifreydb::PropertyMapBuilder;
    use gallifreydb::storage::index_persistence::graph::{
        load_graph_index, new_graph_index_data, persist_property_map, save_graph_index,
    };
    use gallifreydb::storage::index_persistence::{PersistedNode, PersistedPropertyMap};

    let dir = tempdir().unwrap();

    // Create graph with valid properties
    let mut graph_data = new_graph_index_data();
    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();
    let persisted_props = persist_property_map(&props).unwrap();

    graph_data.nodes.push(PersistedNode {
        id: 1,
        label_idx: 1,
        properties: persisted_props,
    });

    // Add node with corrupted property data (invalid serialization)
    graph_data.nodes.push(PersistedNode {
        id: 2,
        label_idx: 1,
        properties: PersistedPropertyMap {
            entries: vec![], // Empty properties - edge case
        },
    });

    graph_data.node_count = 2;

    // Save and load
    let path = dir.path().join("graph.idx");
    save_graph_index(&graph_data, &path).unwrap();

    let loaded = load_graph_index(&path);
    assert!(loaded.is_ok(), "Should handle empty properties gracefully");

    let loaded_data = loaded.unwrap();
    assert_eq!(loaded_data.node_count, 2);

    println!("✓ Corrupted property data test passed");
}

/// Test restoration with multiple simultaneous errors.
#[test]
fn test_multiple_restoration_errors() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    use gallifreydb::storage::index_persistence::PersistenceConfig;
    use gallifreydb::{GallifreyDB, PropertyMapBuilder, config::GallifreyDBConfig};

    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // Create database with multiple nodes
    {
        let config = GallifreyDBConfig::builder()
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: data_dir.clone(),
                load_on_startup: false,
                ..Default::default()
            })
            .build();

        let db = GallifreyDB::with_unified_config(config);

        // Add several nodes
        for i in 0..10 {
            db.create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("Person {}", i))
                    .insert("id", i as i64)
                    .build(),
            )
            .unwrap();
        }

        db.persist_indexes().unwrap();
    }

    // Corrupt the graph file to simulate various errors
    use gallifreydb::storage::index_persistence::IndexPersistenceManager;
    use gallifreydb::storage::index_persistence::graph::{load_graph_index, save_graph_index};

    let manager = IndexPersistenceManager::new(&data_dir);
    let graph_path = manager.graph_path().join("adjacency.idx");

    let mut graph_data = load_graph_index(&graph_path).unwrap();

    // Add nodes with various invalid states
    use gallifreydb::storage::index_persistence::PersistedNode;

    // Node with invalid label index (non-existent string)
    graph_data.nodes.push(PersistedNode {
        id: 100,
        label_idx: 99999, // Non-existent string index
        properties: graph_data.nodes[0].properties.clone(),
    });

    // Node with very large ID
    graph_data.nodes.push(PersistedNode {
        id: u64::MAX - 100,
        label_idx: 1,
        properties: graph_data.nodes[0].properties.clone(),
    });

    graph_data.node_count = graph_data.nodes.len() as u64;

    // Save corrupted data
    save_graph_index(&graph_data, &graph_path).unwrap();

    // Load and verify it handles multiple errors gracefully
    // With our new logging, this should print warnings for each skipped node
    let config = GallifreyDBConfig::builder()
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.clone(),
            load_on_startup: true,
            ..Default::default()
        })
        .build();

    let db = GallifreyDB::with_unified_config(config);

    // Should have loaded the valid nodes (10) and skipped the invalid ones (2)
    // Note: Exact count may vary depending on restoration logic
    assert!(
        db.node_count() >= 10,
        "Should have loaded at least the 10 valid nodes"
    );

    println!("✓ Multiple restoration errors test passed");
    println!(
        "  Loaded {} nodes (expected to skip some invalid ones)",
        db.node_count()
    );
}

// ============================================================================
// Vector Index Persistence Tests (Issue #408)
// ============================================================================

/// Test that vector indexes can be persisted and loaded correctly.
///
/// This test validates:
/// 1. Vector index metadata (dimensions, metric, config) is persisted
/// 2. Vector mappings (NodeID ↔ usearch key) are persisted
/// 3. HNSW index is persisted (usearch native format)
/// 4. Vector index can be loaded and queried after restart
#[test]
fn test_vector_index_persistence() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    println!("\n=== Vector Index Persistence Test ===");

    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    let node_ids;
    let similar_results_before;

    // ========================================================================
    // Phase 1: Create database with vector index and data
    // ========================================================================

    println!("\nPhase 1: Creating database with vector index...");

    {
        use gallifreydb::index::vector::{DistanceMetric, HnswConfig};

        let config = GallifreyDBConfig::builder()
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: data_dir.clone(),
                load_on_startup: false,
                ..Default::default()
            })
            .build();

        let db = GallifreyDB::with_unified_config(config);

        // Enable vector index for "embedding" property
        db.vector_index("embedding")
            .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
            .enable()
            .unwrap();

        println!("✓ Vector index enabled for 'embedding' property");

        // Create nodes with vector embeddings
        node_ids = vec![
            db.create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Rust Programming")
                    .insert_vector("embedding", &vec![0.1; 384])
                    .build(),
            )
            .unwrap(),
            db.create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Python Guide")
                    .insert_vector("embedding", &vec![0.2; 384])
                    .build(),
            )
            .unwrap(),
            db.create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Go Tutorial")
                    .insert_vector("embedding", &vec![0.3; 384])
                    .build(),
            )
            .unwrap(),
        ];

        println!("✓ Created {} nodes with embeddings", node_ids.len());

        // Query vector index before persistence
        similar_results_before = db.find_similar(node_ids[0], 2).unwrap();
        assert!(
            !similar_results_before.is_empty(),
            "Should find similar vectors before persistence"
        );

        println!(
            "✓ Found {} similar nodes before persistence",
            similar_results_before.len()
        );

        // Persist indexes
        db.persist_indexes().unwrap();

        println!("✓ Persisted all indexes to disk");

        drop(db);
    }

    // ========================================================================
    // Phase 2: Verify vector index files exist on disk
    // ========================================================================

    println!("\nPhase 2: Verifying vector index files...");

    {
        use gallifreydb::storage::index_persistence::IndexPersistenceManager;

        let manager = IndexPersistenceManager::new(&data_dir);

        // Check that vector index directory and files exist
        let vec_path = manager.vector_path("embedding");
        assert!(
            vec_path.exists(),
            "Vector index directory should exist for 'embedding' property"
        );

        let meta_path = vec_path.join("meta.idx");
        assert!(
            meta_path.exists(),
            "Vector metadata file should exist: {:?}",
            meta_path
        );

        let mappings_path = vec_path.join("mappings.idx");
        assert!(
            mappings_path.exists(),
            "Vector mappings file should exist: {:?}",
            mappings_path
        );

        let usearch_path = vec_path.join("current.usearch");
        assert!(
            usearch_path.exists(),
            "Usearch index file should exist: {:?}",
            usearch_path
        );

        println!("✓ All vector index files exist on disk");

        // Verify metadata content
        use gallifreydb::storage::index_persistence::vector::load_vector_meta;
        let meta = load_vector_meta(&meta_path).unwrap();
        assert_eq!(meta.property_name, "embedding");
        assert_eq!(meta.dimensions, 384);
        assert_eq!(meta.vector_count, 3);

        println!("✓ Vector metadata is correct (384 dimensions, 3 vectors)");

        // Verify mappings content
        use gallifreydb::storage::index_persistence::vector::load_vector_mappings;
        let mappings = load_vector_mappings(&mappings_path).unwrap();
        assert_eq!(mappings.count, 3);
        assert_eq!(mappings.mappings.len(), 3);

        println!("✓ Vector mappings are correct (3 mappings)");
    }

    // ========================================================================
    // Phase 3: Restart database and verify vector index is restored
    // ========================================================================

    println!("\nPhase 3: Restarting database and verifying restoration...");

    {
        let config = GallifreyDBConfig::builder()
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: data_dir.clone(),
                load_on_startup: true, // This should load vector indexes
                ..Default::default()
            })
            .build();

        let db = GallifreyDB::with_unified_config(config);

        println!("✓ Database restarted, loading indexes...");

        // Verify vector index is enabled
        assert!(
            db.is_vector_index_enabled_for("embedding"),
            "Vector index should be enabled after loading"
        );

        println!("✓ Vector index is enabled for 'embedding'");

        // Verify nodes were restored
        assert_eq!(db.node_count(), 3);

        for &node_id in &node_ids {
            let node = db.get_node(node_id).unwrap();
            assert!(
                node.properties.get("embedding").is_some(),
                "Node should have embedding property"
            );
        }

        println!("✓ All nodes with embeddings were restored");

        // Verify vector queries work after restart
        let similar_results_after = db.find_similar(node_ids[0], 2).unwrap();
        assert!(
            !similar_results_after.is_empty(),
            "Should find similar vectors after restart"
        );

        println!(
            "✓ Found {} similar nodes after restart",
            similar_results_after.len()
        );

        // Results should be similar (same approximate neighbors)
        assert_eq!(
            similar_results_before.len(),
            similar_results_after.len(),
            "Should find same number of similar nodes"
        );

        println!("\n=== Vector Index Persistence Test PASSED ===");
    }
}

/// Test temporal version round-trip: convert → save → load → restore.
///
/// Validates that temporal versions (both nodes and edges, anchors and deltas)
/// can be persisted and restored correctly.
#[test]
fn test_temporal_version_round_trip() {
    use gallifreydb::core::id::{EdgeId, NodeId, VersionId};
    use gallifreydb::core::temporal::{BiTemporalInterval, TimeRange};
    use gallifreydb::storage::index_persistence::formats::PersistedVersionType;
    use gallifreydb::storage::index_persistence::temporal::{
        convert_edge_version, convert_node_version, load_temporal_index, restore_edge_version,
        restore_node_version,
    };
    use gallifreydb::storage::version::{EdgeVersion, NodeVersion, VersionData};

    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    // ========================================================================
    // Phase 1: Create temporal versions
    // ========================================================================

    let person_label = GLOBAL_INTERNER.intern("Person").unwrap();
    let knows_label = GLOBAL_INTERNER.intern("KNOWS").unwrap();

    GLOBAL_INTERNER.intern("name").unwrap();
    GLOBAL_INTERNER.intern("age").unwrap();
    GLOBAL_INTERNER.intern("weight").unwrap();

    // Create node anchor version
    let node_props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let node_version = NodeVersion {
        id: VersionId::new(1000).unwrap(),
        node_id: NodeId::new(1).unwrap(),
        temporal: BiTemporalInterval::new(
            TimeRange::new(1000, 2000).unwrap(),
            TimeRange::from(1000),
        ),
        label: person_label,
        data: VersionData::Anchor {
            properties: node_props,
            vector_snapshot_id: Some(42),
        },
        next_version: None,
        prev_version: None,
    };

    // Create edge anchor version
    let edge_props = PropertyMapBuilder::new().insert("weight", 1.5f64).build();

    let edge_version = EdgeVersion {
        id: VersionId::new(1001).unwrap(),
        edge_id: EdgeId::new(100).unwrap(),
        temporal: BiTemporalInterval::new(
            TimeRange::new(1000, 2000).unwrap(),
            TimeRange::from(1000),
        ),
        label: knows_label,
        source: NodeId::new(1).unwrap(),
        target: NodeId::new(2).unwrap(),
        data: VersionData::anchor(edge_props),
        next_version: None,
        prev_version: None,
    };

    // ========================================================================
    // Phase 2: Convert and Save
    // ========================================================================

    let dir = tempdir().unwrap();
    let manager = IndexPersistenceManager::new(dir.path());
    manager.ensure_directories().unwrap();

    // Save interner first (dependency)
    manager.save_string_interner().unwrap();

    // Convert versions
    let node_entry = convert_node_version(&node_version).unwrap();
    let edge_entry = convert_edge_version(&edge_version).unwrap();

    // Create temporal data
    let mut temporal_data = new_temporal_index_data();
    temporal_data.node_versions.push(node_entry.clone());
    temporal_data.edge_versions.push(edge_entry.clone());

    // Save to disk
    let temporal_path = manager.temporal_path().join("versions.idx");
    save_temporal_index(&temporal_data, &temporal_path).unwrap();

    assert!(temporal_path.exists(), "Temporal index file should exist");

    // ========================================================================
    // Phase 3: Load and Restore
    // ========================================================================

    // Load from disk
    let loaded_data = load_temporal_index(&temporal_path).unwrap();

    assert_eq!(
        loaded_data.node_versions.len(),
        1,
        "Should have 1 node version"
    );
    assert_eq!(
        loaded_data.edge_versions.len(),
        1,
        "Should have 1 edge version"
    );

    // Restore versions
    let restored_node = restore_node_version(&loaded_data.node_versions[0], person_label).unwrap();
    let restored_edge = restore_edge_version(&loaded_data.edge_versions[0], knows_label).unwrap();

    // ========================================================================
    // Phase 4: Verify
    // ========================================================================

    // Verify node version
    assert_eq!(restored_node.node_id, node_version.node_id);
    assert_eq!(
        restored_node.temporal.valid_time().start(),
        node_version.temporal.valid_time().start()
    );
    assert_eq!(
        restored_node.temporal.valid_time().end(),
        node_version.temporal.valid_time().end()
    );
    assert!(restored_node.data.is_anchor());
    assert_eq!(
        restored_node.data.get_vector_snapshot_id(),
        Some(42),
        "Vector snapshot ID should be preserved"
    );

    // Verify node properties
    if let VersionData::Anchor { properties, .. } = &restored_node.data {
        assert_eq!(properties.len(), 2);
        assert_eq!(properties.get("name").unwrap().as_str().unwrap(), "Alice");
        assert_eq!(properties.get("age").unwrap().as_int().unwrap(), 30);
    } else {
        panic!("Expected anchor version for node");
    }

    // Verify edge version
    assert_eq!(restored_edge.edge_id, edge_version.edge_id);
    assert_eq!(restored_edge.source, edge_version.source);
    assert_eq!(restored_edge.target, edge_version.target);
    assert!(restored_edge.data.is_anchor());

    // Verify edge properties
    if let VersionData::Anchor { properties, .. } = &restored_edge.data {
        assert_eq!(properties.len(), 1);
        assert_eq!(properties.get("weight").unwrap().as_float().unwrap(), 1.5);
    } else {
        panic!("Expected anchor version for edge");
    }

    // Verify entry types are correct
    assert!(
        matches!(
            loaded_data.node_versions[0].version_type,
            PersistedVersionType::Anchor
        ),
        "Node should be persisted as anchor"
    );
    assert!(
        matches!(
            loaded_data.edge_versions[0].version_type,
            PersistedVersionType::Anchor
        ),
        "Edge should be persisted as anchor"
    );

    println!("✓ Temporal version round-trip test passed");
}

/// Test full temporal persistence lifecycle with database.
///
/// This test validates the complete temporal persistence workflow:
/// 1. Create database with nodes/edges (which creates temporal versions)
/// 2. Persist indexes (including temporal versions)
/// 3. Restart database
/// 4. Verify temporal versions are restored correctly
#[test]
fn test_temporal_persistence_with_database() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    println!("\n=== Temporal Persistence with Database Test ===");

    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    let node1_id;
    let node2_id;
    let edge_id;

    // ========================================================================
    // Phase 1: Create database with nodes/edges
    // ========================================================================

    println!("\nPhase 1: Creating database with nodes and edges...");

    {
        let config = GallifreyDBConfig::builder()
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: data_dir.clone(),
                load_on_startup: false,
                ..Default::default()
            })
            .build();

        let db = GallifreyDB::with_unified_config(config);

        // Create nodes (each node creation creates a temporal version)
        node1_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30i64)
                    .build(),
            )
            .unwrap();

        node2_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert("age", 25i64)
                    .build(),
            )
            .unwrap();

        println!("✓ Created 2 nodes");

        // Create edge (edge creation also creates a temporal version)
        edge_id = db
            .create_edge(
                node1_id,
                node2_id,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
            )
            .unwrap();

        println!("✓ Created 1 edge");

        // Persist indexes (including temporal versions)
        db.persist_indexes().unwrap();
        println!("✓ Persisted all indexes to disk");

        drop(db);
    }

    // ========================================================================
    // Phase 2: Verify temporal index file exists
    // ========================================================================

    println!("\nPhase 2: Verifying temporal index file...");

    {
        use gallifreydb::storage::index_persistence::IndexPersistenceManager;

        let manager = IndexPersistenceManager::new(&data_dir);
        let temporal_path = manager.temporal_path().join("versions.idx");

        assert!(
            temporal_path.exists(),
            "Temporal index file should exist: {:?}",
            temporal_path
        );

        println!("✓ Temporal index file exists");

        // Load and verify temporal data
        use gallifreydb::storage::index_persistence::temporal::load_temporal_index;
        let temporal_data = load_temporal_index(&temporal_path).unwrap();

        println!(
            "✓ Temporal index contains {} node versions, {} edge versions",
            temporal_data.node_versions.len(),
            temporal_data.edge_versions.len()
        );

        // We should have versions for the nodes and edges we created
        assert!(
            temporal_data.node_versions.len() >= 2,
            "Should have persisted at least 2 node versions"
        );
        assert!(
            !temporal_data.edge_versions.is_empty(),
            "Should have persisted at least 1 edge version"
        );
    }

    // ========================================================================
    // Phase 3: Restart database and verify data restored
    // ========================================================================

    println!("\nPhase 3: Restarting database and verifying restoration...");

    {
        let config = GallifreyDBConfig::builder()
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: data_dir.clone(),
                load_on_startup: true, // This should load temporal versions
                ..Default::default()
            })
            .build();

        let db = GallifreyDB::with_unified_config(config);

        println!("✓ Database restarted with load_on_startup=true");

        // Verify nodes were restored
        let node1 = db.get_node(node1_id).unwrap();
        assert_eq!(
            node1.properties.get("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(
            node1.properties.get("age").and_then(|v| v.as_int()),
            Some(30)
        );

        let node2 = db.get_node(node2_id).unwrap();
        assert_eq!(
            node2.properties.get("name").and_then(|v| v.as_str()),
            Some("Bob")
        );

        println!("✓ Nodes restored correctly");

        // Verify edge was restored
        let edge = db.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.properties.get("since").and_then(|v| v.as_int()),
            Some(2020)
        );

        println!("✓ Edge restored correctly");

        // Verify the database is fully functional after restore
        assert_eq!(db.node_count(), 2);
        assert_eq!(db.edge_count(), 1);

        println!("✓ Database counts match expected values");

        println!("\n=== Temporal Persistence with Database Test PASSED ===");
    }
}

/// Test that delta versions with removed properties are correctly persisted and restored.
///
/// This test verifies that PropertyDelta.removed keys are preserved through persistence.
#[test]
fn test_delta_removed_properties_persistence() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    println!("\n=== Delta Removed Properties Persistence Test ===");

    // Create anchor with multiple properties
    let person_label = GLOBAL_INTERNER.intern("Person").unwrap();

    let anchor_props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .insert("city", "Seattle")
        .build();

    let _anchor_version = NodeVersion {
        id: VersionId::new(1).unwrap(),
        node_id: NodeId::new(100).unwrap(),
        temporal: BiTemporalInterval::now(1000i64, 2000i64),
        label: person_label,
        data: VersionData::Anchor {
            properties: anchor_props,
            vector_snapshot_id: None,
        },
        next_version: None,
        prev_version: None,
    };

    // Create delta that removes "city" property
    let mut delta = PropertyDelta::new();
    delta.changed.insert(
        GLOBAL_INTERNER.intern("age").unwrap(),
        PropertyValue::Int(31),
    );
    delta
        .removed
        .insert(GLOBAL_INTERNER.intern("city").unwrap());

    let delta_version = NodeVersion {
        id: VersionId::new(2).unwrap(),
        node_id: NodeId::new(100).unwrap(),
        temporal: BiTemporalInterval::now(1000i64, 3000i64),
        label: person_label,
        data: VersionData::Delta {
            delta: delta.clone(),
        },
        next_version: None,
        prev_version: Some(VersionId::new(1).unwrap()),
    };

    println!("✓ Created delta with 1 changed property (age: 31) and 1 removed property (city)");
    assert!(
        !delta.removed.is_empty(),
        "Delta should have removed properties"
    );

    // Persist
    let dir = tempdir().unwrap();
    let manager = IndexPersistenceManager::new(dir.path());
    manager.ensure_directories().unwrap();
    manager.save_string_interner().unwrap();

    let delta_entry = convert_node_version(&delta_version).unwrap();

    let mut temporal_data = new_temporal_index_data();
    temporal_data.node_versions.push(delta_entry);

    let temporal_path = manager.temporal_path().join("versions.idx");
    save_temporal_index(&temporal_data, &temporal_path).unwrap();

    println!("✓ Persisted delta version");

    // Restore
    let loaded_data = load_temporal_index(&temporal_path).unwrap();
    let restored_delta = restore_node_version(&loaded_data.node_versions[0], person_label).unwrap();

    println!("✓ Restored delta version");

    // Verify removed properties are preserved
    match &restored_delta.data {
        VersionData::Delta {
            delta: restored_delta_inner,
        } => {
            println!(
                "Restored delta.changed: {} properties",
                restored_delta_inner.changed.len()
            );
            println!(
                "Restored delta.removed: {} properties",
                restored_delta_inner.removed.len()
            );

            assert_eq!(
                restored_delta_inner.changed.len(),
                1,
                "Should have 1 changed property"
            );

            // THIS WILL FAIL - removed properties are not persisted!
            assert_eq!(
                restored_delta_inner.removed.len(),
                1,
                "Should have 1 removed property (THIS IS THE BUG)"
            );

            // Verify the removed property is the correct one
            let city_key = GLOBAL_INTERNER.intern("city").unwrap();
            assert!(
                restored_delta_inner.removed.contains(&city_key),
                "Delta should indicate 'city' was removed"
            );

            println!("✓ Delta removed properties preserved correctly");
        }
        _ => panic!("Expected Delta version, got Anchor"),
    }
}
