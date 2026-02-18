use super::*;
use crate::api::transaction::{ReadOps, WriteOps};
use crate::core::GLOBAL_INTERNER;
use crate::core::id::NodeId;
use crate::core::property::{PropertyMapBuilder, PropertyValue};
use crate::utils::error::{Error, Result};

#[test]
fn test_create_node() {
    let db = AletheiaDB::new().unwrap();

    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let node_id = db.create_node("Person", props).unwrap();

    assert_eq!(db.node_count(), 1);

    let node = db.get_node(node_id).unwrap();
    assert_eq!(node.id, node_id);
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
}

#[test]
fn test_create_edge() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let edge_id = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )
        .unwrap();

    assert_eq!(db.edge_count(), 1);

    let edge = db.get_edge(edge_id).unwrap();
    assert_eq!(edge.source, alice);
    assert_eq!(edge.target, bob);
}

#[test]
fn test_graph_traversal() {
    let db = AletheiaDB::new().unwrap();

    let n0 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    db.create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    let outgoing = db.get_outgoing_edges(n0);
    assert_eq!(outgoing.len(), 2);

    let knows_edges = db.get_outgoing_edges_with_label(n0, "KNOWS");
    assert_eq!(knows_edges.len(), 2);
}

#[test]
fn test_iterator_access() {
    let db = AletheiaDB::new().unwrap();

    let n0 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let e1 = db
        .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    let e2 = db
        .create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Test outgoing iterators
    let outgoing: Vec<_> = db.get_outgoing_edges_iter(n0).collect();
    assert_eq!(outgoing.len(), 2);
    assert!(outgoing.contains(&e1));
    assert!(outgoing.contains(&e2));

    // Test incoming iterators
    let incoming1: Vec<_> = db.get_incoming_edges_iter(n1).collect();
    assert_eq!(incoming1.len(), 1);
    assert_eq!(incoming1[0], e1);

    let incoming2: Vec<_> = db.get_incoming_edges_iter(n2).collect();
    assert_eq!(incoming2.len(), 1);
    assert_eq!(incoming2[0], e2);
}

#[test]
fn test_historical_stats() {
    let db = AletheiaDB::new().unwrap();

    db.create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let stats = db.historical_stats().unwrap();
    assert_eq!(stats.total_node_versions, 2);
    assert_eq!(stats.node_anchor_count, 2); // First versions are always anchors
}

// ==================== Transaction API Tests ====================

#[test]
fn test_closure_based_write_api() {
    let db = AletheiaDB::new().unwrap();

    // Use closure-based API for multiple operations
    let (node_id, edge_id) = db
        .write(|tx| {
            let n1 = tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )?;
            let n2 = tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )?;
            let e = tx.create_edge(
                n1,
                n2,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2024i64).build(),
            )?;
            Ok::<_, Error>((n1, e))
        })
        .unwrap();

    // Verify changes are visible
    assert_eq!(db.node_count(), 2);
    assert_eq!(db.edge_count(), 1);

    let node = db.get_node(node_id).unwrap();
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Alice")
    );

    let edge = db.get_edge(edge_id).unwrap();
    assert_eq!(edge.source, node_id);
}

#[test]
fn test_closure_based_read_api() {
    let db = AletheiaDB::new().unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Charlie").build(),
        )
        .unwrap();

    // Use closure-based read API
    let name = db
        .read(|tx| {
            let node = tx.get_node(node_id)?;
            Ok::<_, Error>(
                node.get_property("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            )
        })
        .unwrap();

    assert_eq!(name, Some("Charlie".to_string()));
}

#[test]
fn test_explicit_write_transaction() {
    let db = AletheiaDB::new().unwrap();

    let mut tx = db.write_transaction().unwrap();
    let n1 = tx
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "David").build(),
        )
        .unwrap();
    let n2 = tx
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Eve").build(),
        )
        .unwrap();
    tx.create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Changes not visible before commit
    assert_eq!(db.node_count(), 0);

    // Commit
    tx.commit().unwrap();

    // Now visible
    assert_eq!(db.node_count(), 2);
    assert_eq!(db.edge_count(), 1);
}

#[test]
fn test_explicit_read_transaction() {
    let db = AletheiaDB::new().unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("age", 42i64).build(),
        )
        .unwrap();

    let tx = db.read_transaction().unwrap();
    let node = tx.get_node(node_id).unwrap();
    assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(42));

    // Read transactions don't need commit
}

#[test]
fn test_transaction_atomicity() {
    let db = AletheiaDB::new().unwrap();

    // Create a valid node first
    let valid_node = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Try to create multiple operations, one of which will fail
    let result: std::result::Result<(), Error> = db.write(|tx| {
        tx.create_node("Person", PropertyMapBuilder::new().build())?;
        tx.create_node("Person", PropertyMapBuilder::new().build())?;
        // This should fail validation (non-existent target)
        tx.create_edge(
            valid_node,
            NodeId::new(9999).unwrap(),
            "KNOWS",
            PropertyMapBuilder::new().build(),
        )?;
        Ok(())
    });

    // Transaction should fail
    assert!(result.is_err());

    // No partial changes should be visible (atomicity)
    // We started with 1 node, should still have 1 node
    assert_eq!(db.node_count(), 1);
    assert_eq!(db.edge_count(), 0);
}

#[test]
fn test_transaction_rollback_on_error() {
    let db = AletheiaDB::new().unwrap();

    // Closure returns an error - should auto-rollback
    let result: Result<()> = db.write(|tx| {
        tx.create_node("Person", PropertyMapBuilder::new().build())?;
        tx.create_node("Person", PropertyMapBuilder::new().build())?;
        // Manually return an error
        Err(crate::utils::error::Error::Storage(
            crate::utils::error::StorageError::InconsistentState {
                reason: "test error".to_string(),
            },
        ))
    });

    assert!(result.is_err());

    // All changes rolled back
    assert_eq!(db.node_count(), 0);
}

#[test]
fn test_multiple_transactions() {
    let db = AletheiaDB::new().unwrap();

    // Transaction 1
    let n1 = db
        .write(|tx| tx.create_node("Person", PropertyMapBuilder::new().build()))
        .unwrap();

    // Transaction 2
    let n2 = db
        .write(|tx| tx.create_node("Person", PropertyMapBuilder::new().build()))
        .unwrap();

    // Transaction 3
    db.write(|tx| tx.create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build()))
        .unwrap();

    assert_eq!(db.node_count(), 2);
    assert_eq!(db.edge_count(), 1);
}

#[test]
fn test_snapshot_isolation() {
    let db = AletheiaDB::new().unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("version", 1i64).build(),
        )
        .unwrap();

    // Start a read transaction - captures snapshot
    let tx1 = db.read_transaction().unwrap();
    let node_v1 = tx1.get_node(node_id).unwrap();
    assert_eq!(
        node_v1.get_property("version").and_then(|v| v.as_int()),
        Some(1)
    );

    // Another write commits a change (creates a new node)
    let new_node_id = db
        .write(|tx| {
            tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("version", 2i64).build(),
            )
        })
        .unwrap();

    // Snapshot Isolation: tx1 should NOT see the new node
    // because it was created and committed after tx1's snapshot
    assert!(tx1.get_node(new_node_id).is_err());

    // Verify tx1 still sees the original node
    let node_v1_again = tx1.get_node(node_id).unwrap();
    assert_eq!(
        node_v1_again
            .get_property("version")
            .and_then(|v| v.as_int()),
        Some(1)
    );
}

// ==================== Constructor Error Handling Tests ====================
// These tests verify that database constructors return Result and properly
// propagate WAL creation errors (Issue #343)

#[test]
fn test_new_returns_result() {
    // AletheiaDB::new() should return Result<Self> and succeed with default config
    let result = AletheiaDB::new();
    assert!(result.is_ok(), "new() should succeed with default config");
}

#[test]
fn test_with_config_returns_result() {
    // AletheiaDB::with_config() should return Result<Self>
    let result = AletheiaDB::with_config(crate::core::version::AnchorConfig::default());
    assert!(
        result.is_ok(),
        "with_config() should succeed with default config"
    );
}

#[test]
fn test_with_wal_config_returns_result() {
    // AletheiaDB::with_wal_config() should return Result<Self>
    let wal_config = crate::config::WalConfig::default();
    let result = AletheiaDB::with_wal_config(wal_config);
    assert!(
        result.is_ok(),
        "with_wal_config() should succeed with default config"
    );
}

#[test]
fn test_with_full_config_returns_result() {
    // AletheiaDB::with_full_config() should return Result<Self>
    let result = AletheiaDB::with_full_config(
        crate::core::version::AnchorConfig::default(),
        crate::config::WalConfig::default(),
    );
    assert!(
        result.is_ok(),
        "with_full_config() should succeed with default config"
    );
}

#[test]
fn test_with_unified_config_returns_result() {
    // AletheiaDB::with_unified_config() should return Result<Self>
    let config = crate::config::AletheiaDBConfig::default();
    let result = AletheiaDB::with_unified_config(config);
    assert!(
        result.is_ok(),
        "with_unified_config() should succeed with default config"
    );
}

#[test]
fn test_cold_storage_configuration() {
    use crate::config::{AletheiaDBConfig, HistoricalConfigBuilder, WalConfigBuilder};
    use std::time::Duration;

    // Create a temporary directory for test data
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let cold_storage_path = temp_dir.path().join("cold.redb");

    // Create config with cold storage enabled but index persistence disabled
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(temp_dir.path().join("wal"))
                .build(),
        )
        .persistence(crate::storage::index_persistence::PersistenceConfig {
            enabled: false, // Disable index persistence for clean test
            ..Default::default()
        })
        .historical(
            HistoricalConfigBuilder::new()
                .enable_cold_storage(true)
                .cold_storage_path(&cold_storage_path)
                .migration_age_threshold(Duration::from_secs(3600))
                .max_hot_versions(1000)
                .build(),
        )
        .build();

    // Initialize database with cold storage
    let _db = AletheiaDB::with_unified_config(config).expect("Failed to create database");

    // Verify cold storage file was created
    assert!(
        cold_storage_path.exists(),
        "Cold storage file should be created"
    );
}

#[cfg(unix)]
#[test]
fn test_wal_creation_failure_propagates_error() {
    // When WAL creation fails, the error should be propagated instead of panicking
    use std::path::PathBuf;

    // Use /dev/null/wal - /dev/null is a character device, not a directory,
    // so any attempt to create subdirectories under it will fail
    let invalid_wal_dir = PathBuf::from("/dev/null/wal");

    let wal_config = crate::config::WalConfigBuilder::new()
        .wal_dir(invalid_wal_dir)
        .build();

    let result = AletheiaDB::with_wal_config(wal_config);

    // Should return Err instead of panicking
    assert!(
        result.is_err(),
        "with_wal_config() should return Err when WAL directory cannot be created"
    );

    // Error should mention an I/O issue
    let err = result.err().expect("Expected an error");
    let err_msg = err.to_string().to_lowercase();
    assert!(
        err_msg.contains("i/o")
            || err_msg.contains("directory")
            || err_msg.contains("not a directory"),
        "Error message should indicate I/O issue, got: {}",
        err
    );
}

#[cfg(unix)]
#[test]
fn test_unified_config_wal_failure_propagates_error() {
    // When WAL creation fails in with_unified_config, the error should be propagated
    use std::path::PathBuf;

    // Use /dev/null/wal - /dev/null is a character device, not a directory,
    // so any attempt to create subdirectories under it will fail
    let invalid_wal_dir = PathBuf::from("/dev/null/wal");

    let config = crate::config::AletheiaDBConfigBuilder::new()
        .wal(
            crate::config::WalConfigBuilder::new()
                .wal_dir(invalid_wal_dir)
                .build(),
        )
        .build();

    let result = AletheiaDB::with_unified_config(config);

    // Should return Err instead of panicking
    assert!(
        result.is_err(),
        "with_unified_config() should return Err when WAL directory cannot be created"
    );
}

// ========================================================================
// Phase 3: Simple Accessor and Getter Tests
// ========================================================================

#[test]
fn test_aletheiadb_is_vector_index_enabled_for() {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = AletheiaDB::new().unwrap();

    // Initially no index should be enabled
    assert!(!db.is_vector_index_enabled());
    assert!(!db.is_vector_index_enabled_for("embedding"));
    assert!(!db.is_vector_index_enabled_for("vector"));

    // Enable index for "embedding"
    let config = HnswConfig::new(128, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config).unwrap();

    // Now should be enabled
    assert!(db.is_vector_index_enabled());
    assert!(db.is_vector_index_enabled_for("embedding"));
    assert!(!db.is_vector_index_enabled_for("vector")); // Still false for other property

    // Enable another index
    let config2 = HnswConfig::new(256, DistanceMetric::Euclidean);
    db.enable_vector_index("vector", config2).unwrap();

    assert!(db.is_vector_index_enabled_for("vector"));
}

#[test]
fn test_aletheiadb_default_durability() {
    let db = AletheiaDB::new().unwrap();

    // Default durability should exist and be valid
    let _durability = db.default_durability();
    // Just verify we can call it without error
}

#[test]
fn test_get_edge_source_and_target() {
    let db = AletheiaDB::new().unwrap();

    // Create nodes
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();

    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap();

    // Create edge from alice to bob
    let knows_edge = db
        .create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Verify get_edge_source and get_edge_target
    assert_eq!(db.get_edge_source(knows_edge).unwrap(), alice);
    assert_eq!(db.get_edge_target(knows_edge).unwrap(), bob);
}

// ==================== Phase 9: History/Version API Tests ====================

#[test]
fn test_get_node_at_valid_time() {
    let db = AletheiaDB::new().unwrap();

    // Create backdated node
    let mut tx = db.write_transaction().unwrap();
    let jan_1 = crate::core::hlc::HybridTimestamp::new(1_704_067_200_000_000, 0).unwrap();
    let props = PropertyMapBuilder::new().insert("name", "Alice").build();
    let node_id = tx
        .create_node_with_valid_time("Person", props, Some(jan_1))
        .unwrap();
    tx.commit().unwrap();

    // Query at Jan 15 (after valid_time start)
    let jan_15 = crate::core::hlc::HybridTimestamp::new(1_705_276_800_000_000, 0).unwrap();
    let node = db.get_node_at_valid_time(node_id, jan_15).unwrap();
    assert_eq!(node.id, node_id);
    assert_eq!(
        node.properties.get("name").unwrap(),
        &PropertyValue::String("Alice".into())
    );
}

#[test]
fn test_get_node_at_transaction_time() {
    let db = AletheiaDB::new().unwrap();

    // Create node
    let props = PropertyMapBuilder::new().insert("name", "Alice").build();
    let node_id = db.create_node("Person", props).unwrap();

    // Query at current transaction time should find it
    let tx_time = crate::core::temporal::time::now();
    let node = db.get_node_at_transaction_time(node_id, tx_time).unwrap();
    assert_eq!(node.id, node_id);
}

#[test]
fn test_get_node_history_returns_all_versions() {
    let db = AletheiaDB::new().unwrap();

    // Create and update a node
    let props1 = PropertyMapBuilder::new().insert("name", "Alice").build();
    let node_id = db.create_node("Person", props1).unwrap();

    // Update node through transaction
    db.write(|tx| {
        let props2 = PropertyMapBuilder::new()
            .insert("name", "Alice Smith")
            .build();
        tx.update_node(node_id, props2)
    })
    .unwrap();

    let history = db.get_node_history(node_id).unwrap();
    assert_eq!(history.version_count(), 2);
    assert_eq!(history.first_version().unwrap().version_number, 1);
    assert_eq!(history.current_version().unwrap().version_number, 2);
}

#[test]
fn test_get_node_at_version() {
    let db = AletheiaDB::new().unwrap();

    // Create and update a node
    let props1 = PropertyMapBuilder::new().insert("name", "Alice").build();
    let node_id = db.create_node("Person", props1).unwrap();

    db.write(|tx| {
        let props2 = PropertyMapBuilder::new().insert("name", "Bob").build();
        tx.update_node(node_id, props2)
    })
    .unwrap();

    // Query version 1
    let v1 = db.get_node_at_version(node_id, 1).unwrap();
    assert_eq!(
        v1.properties.get("name").unwrap(),
        &PropertyValue::String("Alice".into())
    );

    // Query version 2
    let v2 = db.get_node_at_version(node_id, 2).unwrap();
    assert_eq!(
        v2.properties.get("name").unwrap(),
        &PropertyValue::String("Bob".into())
    );
}

#[test]
fn test_diff_node_versions() {
    let db = AletheiaDB::new().unwrap();

    // Create node
    let props1 = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();
    let node_id = db.create_node("Person", props1).unwrap();

    // Update it
    db.write(|tx| {
        let props2 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 31i64)
            .insert("city", "NYC")
            .build();
        tx.update_node(node_id, props2)
    })
    .unwrap();

    // Get history to find version IDs
    let history = db.get_node_history(node_id).unwrap();
    let v1_id = history.first_version().unwrap().version_id;
    let v2_id = history.current_version().unwrap().version_id;

    // Compute diff
    let diff = db.diff_node_versions(node_id, v1_id, v2_id).unwrap();

    assert!(diff.has_changes());
    assert_eq!(diff.added.len(), 1); // city added
    assert!(diff.added.contains_key("city"));
    assert_eq!(diff.modified.len(), 1); // age modified
    assert!(diff.removed.is_empty());
}

#[test]
fn test_get_edge_history_returns_all_versions() {
    let db = AletheiaDB::new().unwrap();

    // Create nodes
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create and update edge
    let props1 = PropertyMapBuilder::new().insert("since", 2020i64).build();
    let edge_id = db.create_edge(alice, bob, "KNOWS", props1).unwrap();

    let props2 = PropertyMapBuilder::new().insert("since", 2021i64).build();
    db.write(|tx| tx.update_edge(edge_id, props2)).unwrap();

    let history = db.get_edge_history(edge_id).unwrap();
    assert_eq!(history.version_count(), 2);
}

#[test]
fn test_diff_edge_versions() {
    let db = AletheiaDB::new().unwrap();

    // Create nodes
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create and update edge
    let props1 = PropertyMapBuilder::new().insert("weight", 1.0f64).build();
    let edge_id = db.create_edge(alice, bob, "KNOWS", props1).unwrap();

    let props2 = PropertyMapBuilder::new().insert("weight", 2.0f64).build();
    db.write(|tx| tx.update_edge(edge_id, props2)).unwrap();

    // Get history to find version IDs
    let history = db.get_edge_history(edge_id).unwrap();
    let v1_id = history.first_version().unwrap().version_id;
    let v2_id = history.current_version().unwrap().version_id;

    // Compute diff
    let diff = db.diff_edge_versions(edge_id, v1_id, v2_id).unwrap();

    assert!(diff.has_changes());
    assert_eq!(diff.modified.len(), 1); // weight modified
}

/// End-to-end integration test for true bi-temporal support.
///
/// This test verifies the complete workflow:
/// 1. Backdated writes with valid_time
/// 2. Independent dimension queries (valid_time vs transaction_time)
/// 3. Version history tracking
/// 4. Version diffing
/// 5. Logical version queries
#[test]
fn test_full_bitemporal_workflow() {
    use crate::core::hlc::HybridTimestamp;
    use crate::core::temporal::time;

    let db = AletheiaDB::new().unwrap();

    // === PART 1: Backdated Write ===
    let jan_1 = HybridTimestamp::new(1_704_067_200_000_000, 0).unwrap(); // 2024-01-01
    let jan_15 = HybridTimestamp::new(1_705_276_800_000_000, 0).unwrap(); // 2024-01-15
    let feb_1 = HybridTimestamp::new(1_706_745_600_000_000, 0).unwrap(); // 2024-02-01

    // Create Alice with valid_time = Jan 1, but recording happens now
    let alice = db
        .write(|tx| {
            tx.create_node_with_valid_time(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
                Some(jan_1),
            )
        })
        .unwrap();

    // === PART 2: Query by Valid Time ===
    // "Was Alice in the system on Jan 15?" - YES (valid_time covers it)
    let result = db.get_node_at_valid_time(alice, jan_15);
    assert!(result.is_ok(), "Should find Alice at Jan 15 valid time");

    // === PART 3: Query by Transaction Time ===
    // "Did we know about Alice on Jan 15?" - NO (recorded after)
    let result = db.get_node_at_transaction_time(alice, jan_15);
    assert!(
        result.is_err(),
        "Should NOT find Alice at Jan 15 transaction time (recorded later)"
    );

    // "Did we know about Alice now?" - YES (recorded at current time)
    let result = db.get_node_at_transaction_time(alice, time::now());
    assert!(
        result.is_ok(),
        "Should find Alice at current transaction time"
    );

    // === PART 4: Update and Check History ===
    db.write(|tx| {
        tx.update_node_with_valid_time(
            alice,
            PropertyMapBuilder::new()
                .insert("name", "Alice Smith")
                .build(),
            Some(feb_1), // Name changed on Feb 1
        )
    })
    .unwrap();

    let history = db.get_node_history(alice).unwrap();
    assert_eq!(
        history.versions.len(),
        2,
        "Should have 2 versions after update"
    );

    // Version 1: name = "Alice", valid from Jan 1
    // Version 2: name = "Alice Smith", valid from Feb 1

    // === PART 5: Version Diff ===
    let diff = db
        .diff_node_versions(
            alice,
            history.versions[0].version_id,
            history.versions[1].version_id,
        )
        .unwrap();

    assert_eq!(diff.modified.len(), 1, "Should have 1 modified property");

    // Check that "name" was modified
    let name_key = GLOBAL_INTERNER.intern("name").unwrap();
    let (modified_key, _, _) = &diff.modified[0];
    assert_eq!(
        *modified_key, name_key,
        "Modified property should be 'name'"
    );

    // === PART 6: Query by Logical Version ===
    let v1 = db.get_node_at_version(alice, 1).unwrap();
    assert_eq!(
        v1.properties.get("name").unwrap(),
        &PropertyValue::String("Alice".into()),
        "Version 1 should have name='Alice'"
    );

    let v2 = db.get_node_at_version(alice, 2).unwrap();
    assert_eq!(
        v2.properties.get("name").unwrap(),
        &PropertyValue::String("Alice Smith".into()),
        "Version 2 should have name='Alice Smith'"
    );
}

#[test]
fn test_find_similar_as_of_in() {
    use crate::index::vector::temporal::{SnapshotStrategy, TemporalVectorConfig};
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = AletheiaDB::new().unwrap();

    // Enable temporal vector index with immediate snapshot strategy
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);
    let temporal_config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        ..TemporalVectorConfig::default_with_hnsw(hnsw_config)
    };
    db.enable_temporal_vector_index("embedding", temporal_config)
        .unwrap();

    // Create a node with a vector
    let vector = vec![1.0, 0.0, 0.0, 0.0];
    let props = PropertyMapBuilder::new()
        .insert("name", "Test")
        .insert_vector("embedding", &vector)
        .build();

    let (node_id, commit_ts) = db
        .write_with_timestamp(|tx| tx.create_node("TestNode", props))
        .unwrap();

    // Search using the specific property
    let results = db
        .find_similar_as_of_in("embedding", &vector, 10, commit_ts)
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node_id);
}

#[test]
fn test_find_nodes_by_property_facade() {
    let db = AletheiaDB::new().unwrap();

    let alice_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30i64)
                .build(),
        )
        .unwrap();
    let bob_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert("age", 30i64)
                .build(),
        )
        .unwrap();
    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Charlie")
            .insert("age", 25i64)
            .build(),
    )
    .unwrap();

    // Find by name
    let results =
        db.find_nodes_by_property("Person", "name", &PropertyValue::String("Alice".into()));
    assert_eq!(results, vec![alice_id]);

    // Find by age (multiple matches)
    let mut results = db.find_nodes_by_property("Person", "age", &PropertyValue::Int(30));
    results.sort();
    let mut expected = vec![alice_id, bob_id];
    expected.sort();
    assert_eq!(results, expected);

    // No matches
    let results =
        db.find_nodes_by_property("Person", "name", &PropertyValue::String("Nobody".into()));
    assert!(results.is_empty());
}

#[test]
fn test_find_nodes_by_property_facade_cross_label() {
    let db = AletheiaDB::new().unwrap();

    let person_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    db.create_node(
        "Company",
        PropertyMapBuilder::new().insert("name", "Alice").build(),
    )
    .unwrap();

    // Should only match Person label
    let results =
        db.find_nodes_by_property("Person", "name", &PropertyValue::String("Alice".into()));
    assert_eq!(results, vec![person_id]);
}

// ========================================================================
// Ops Edge Cases & Error Paths
// ========================================================================

#[test]
fn test_get_node_nonexistent() {
    let db = AletheiaDB::new().unwrap();
    let fake_id = NodeId::new(9999).unwrap();
    let result = db.get_node(fake_id);
    assert!(result.is_err());
}

#[test]
fn test_get_edge_nonexistent() {
    let db = AletheiaDB::new().unwrap();
    let fake_id = crate::core::id::EdgeId::new(9999).unwrap();
    let result = db.get_edge(fake_id);
    assert!(result.is_err());
}

#[test]
fn test_get_edge_source_nonexistent() {
    let db = AletheiaDB::new().unwrap();
    let fake_id = crate::core::id::EdgeId::new(9999).unwrap();
    let result = db.get_edge_source(fake_id);
    assert!(result.is_err());
}

#[test]
fn test_get_edge_target_nonexistent() {
    let db = AletheiaDB::new().unwrap();
    let fake_id = crate::core::id::EdgeId::new(9999).unwrap();
    let result = db.get_edge_target(fake_id);
    assert!(result.is_err());
}

#[test]
fn test_create_node_empty_properties() {
    let db = AletheiaDB::new().unwrap();
    let node_id = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let node = db.get_node(node_id).unwrap();
    assert!(node.properties.is_empty());
}

#[test]
fn test_create_node_empty_label() {
    let db = AletheiaDB::new().unwrap();
    // Empty label should still work - it's a valid string
    let node_id = db
        .create_node("", PropertyMapBuilder::new().build())
        .unwrap();
    let node = db.get_node(node_id).unwrap();
    assert_eq!(node.id, node_id);
}

#[test]
fn test_create_edge_invalid_source() {
    let db = AletheiaDB::new().unwrap();
    let target = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let fake_source = NodeId::new(9999).unwrap();
    let result = db.create_edge(
        fake_source,
        target,
        "KNOWS",
        PropertyMapBuilder::new().build(),
    );
    assert!(result.is_err());
}

#[test]
fn test_create_edge_invalid_target() {
    let db = AletheiaDB::new().unwrap();
    let source = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let fake_target = NodeId::new(9999).unwrap();
    let result = db.create_edge(
        source,
        fake_target,
        "KNOWS",
        PropertyMapBuilder::new().build(),
    );
    assert!(result.is_err());
}

#[test]
fn test_create_edge_both_invalid() {
    let db = AletheiaDB::new().unwrap();
    let result = db.create_edge(
        NodeId::new(9998).unwrap(),
        NodeId::new(9999).unwrap(),
        "KNOWS",
        PropertyMapBuilder::new().build(),
    );
    assert!(result.is_err());
}

#[test]
fn test_delete_node_via_transaction() {
    let db = AletheiaDB::new().unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    assert_eq!(db.node_count(), 1);

    db.write(|tx| tx.delete_node(node_id)).unwrap();
    assert_eq!(db.node_count(), 0);

    // get_node on deleted node should fail
    assert!(db.get_node(node_id).is_err());
}

#[test]
fn test_delete_node_with_edges_creates_orphans() {
    // Current behavior: deleting a node with edges succeeds but leaves orphaned edges.
    // This documents the current system behavior (see issue comments about orphaned edges).
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let edge_id = db
        .create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Deleting a node with edges succeeds (creates orphaned edges)
    db.write(|tx| tx.delete_node(alice)).unwrap();

    // Node is deleted
    assert_eq!(db.node_count(), 1);
    assert!(db.get_node(alice).is_err());

    // Edge still exists as an orphan (documents current behavior)
    let edge = db.get_edge(edge_id).unwrap();
    assert_eq!(edge.source, alice);
}

#[test]
fn test_delete_node_cascade() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let charlie = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(charlie, alice, "FOLLOWS", PropertyMapBuilder::new().build())
        .unwrap();

    assert_eq!(db.edge_count(), 2);

    // Cascade delete should remove alice and all connected edges
    db.write(|tx| tx.delete_node_cascade(alice)).unwrap();

    assert_eq!(db.node_count(), 2); // bob and charlie remain
    assert_eq!(db.edge_count(), 0); // both edges removed
    assert!(db.get_node(alice).is_err());
}

#[test]
fn test_delete_edge_via_transaction() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let edge_id = db
        .create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    assert_eq!(db.edge_count(), 1);

    db.write(|tx| tx.delete_edge(edge_id)).unwrap();
    assert_eq!(db.edge_count(), 0);
    assert!(db.get_edge(edge_id).is_err());
}

#[test]
fn test_delete_nonexistent_node() {
    let db = AletheiaDB::new().unwrap();
    let result: Result<()> = db.write(|tx| tx.delete_node(NodeId::new(9999).unwrap()));
    assert!(result.is_err());
}

#[test]
fn test_delete_nonexistent_edge() {
    let db = AletheiaDB::new().unwrap();
    let result: Result<()> =
        db.write(|tx| tx.delete_edge(crate::core::id::EdgeId::new(9999).unwrap()));
    assert!(result.is_err());
}

#[test]
fn test_scan_nodes_by_label() {
    let db = AletheiaDB::new().unwrap();

    let p1 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let p2 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_node("Company", PropertyMapBuilder::new().build())
        .unwrap();

    let mut persons: Vec<NodeId> = db.scan_nodes_by_label("Person").collect();
    persons.sort();
    let mut expected = vec![p1, p2];
    expected.sort();
    assert_eq!(persons, expected);

    let companies: Vec<NodeId> = db.scan_nodes_by_label("Company").collect();
    assert_eq!(companies.len(), 1);

    // Non-existent label returns empty
    let empty: Vec<NodeId> = db.scan_nodes_by_label("NonExistent").collect();
    assert!(empty.is_empty());
}

#[test]
fn test_outgoing_edges_for_node_with_no_edges() {
    let db = AletheiaDB::new().unwrap();
    let node = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let outgoing = db.get_outgoing_edges(node);
    assert!(outgoing.is_empty());

    let outgoing_iter: Vec<_> = db.get_outgoing_edges_iter(node).collect();
    assert!(outgoing_iter.is_empty());
}

#[test]
fn test_incoming_edges_for_node_with_no_edges() {
    let db = AletheiaDB::new().unwrap();
    let node = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let incoming = db.get_incoming_edges(node);
    assert!(incoming.is_empty());

    let incoming_iter: Vec<_> = db.get_incoming_edges_iter(node).collect();
    assert!(incoming_iter.is_empty());
}

#[test]
fn test_outgoing_edges_with_label_no_matches() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Different label should return empty
    let edges = db.get_outgoing_edges_with_label(alice, "FOLLOWS");
    assert!(edges.is_empty());
}

#[test]
fn test_node_and_edge_counts_empty_db() {
    let db = AletheiaDB::new().unwrap();
    assert_eq!(db.node_count(), 0);
    assert_eq!(db.edge_count(), 0);
}

#[test]
fn test_out_degree_and_in_degree() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let charlie = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(alice, charlie, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    assert_eq!(db.out_degree(alice), 2);
    assert_eq!(db.in_degree(alice), 0);
    assert_eq!(db.out_degree(bob), 0);
    assert_eq!(db.in_degree(bob), 1);
}

#[test]
fn test_iterator_vs_vec_consistency() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let charlie = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(alice, charlie, "FOLLOWS", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(charlie, alice, "FOLLOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Outgoing: Vec vs Iterator should match
    let mut vec_result = db.get_outgoing_edges(alice);
    let mut iter_result: Vec<_> = db.get_outgoing_edges_iter(alice).collect();
    vec_result.sort();
    iter_result.sort();
    assert_eq!(vec_result, iter_result);

    // Incoming: Vec vs Iterator should match
    let mut vec_result = db.get_incoming_edges(alice);
    let mut iter_result: Vec<_> = db.get_incoming_edges_iter(alice).collect();
    vec_result.sort();
    iter_result.sort();
    assert_eq!(vec_result, iter_result);
}

// ========================================================================
// Transaction Edge Cases
// ========================================================================

#[test]
fn test_write_with_timestamp() {
    let db = AletheiaDB::new().unwrap();

    let (node_id, commit_ts) = db
        .write_with_timestamp(|tx| {
            tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
        })
        .unwrap();

    assert_eq!(db.node_count(), 1);

    // The commit timestamp should be a valid non-zero timestamp
    assert!(commit_ts.wallclock() > 0);

    // Should be able to query at the commit timestamp
    let node = db.get_node_at_time(node_id, commit_ts, commit_ts).unwrap();
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
}

#[test]
fn test_write_with_options_default() {
    use crate::storage::wal::WriteOptions;

    let db = AletheiaDB::new().unwrap();
    let options = WriteOptions::new();

    let node_id = db
        .write_with_options(options, |tx| {
            tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
        })
        .unwrap();

    assert_eq!(db.node_count(), 1);
    let node = db.get_node(node_id).unwrap();
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Bob")
    );
}

#[test]
fn test_concurrent_read_transactions() {
    let db = AletheiaDB::new().unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();

    // Multiple read transactions can coexist
    let tx1 = db.read_transaction().unwrap();
    let tx2 = db.read_transaction().unwrap();

    let node1 = tx1.get_node(node_id).unwrap();
    let node2 = tx2.get_node(node_id).unwrap();

    assert_eq!(node1.id, node2.id);
    assert_eq!(node1.get_property("name"), node2.get_property("name"));
}

#[test]
fn test_write_transaction_commit_then_rollback_path() {
    let db = AletheiaDB::new().unwrap();

    // Successful commit
    let mut tx1 = db.write_transaction().unwrap();
    tx1.create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    tx1.commit().unwrap();
    assert_eq!(db.node_count(), 1);

    // Failed transaction (drop without commit = implicit rollback)
    let mut tx2 = db.write_transaction().unwrap();
    tx2.create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    drop(tx2); // Implicit rollback
    assert_eq!(db.node_count(), 1); // Still 1
}

#[test]
fn test_write_closure_error_propagation() {
    let db = AletheiaDB::new().unwrap();

    // Custom error from closure should propagate
    let result: Result<()> = db.write(|_tx| {
        Err(Error::Storage(
            crate::utils::error::StorageError::InconsistentState {
                reason: "custom test error".to_string(),
            },
        ))
    });

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("custom test error"),
        "Error should contain our message: {}",
        err_msg
    );
}

#[test]
fn test_read_closure_error_propagation() {
    let db = AletheiaDB::new().unwrap();

    let result: Result<()> = db.read(|_tx| {
        Err(Error::Storage(
            crate::utils::error::StorageError::InconsistentState {
                reason: "read error".to_string(),
            },
        ))
    });

    assert!(result.is_err());
}

// ========================================================================
// Admin / Statistics / Compression Tests
// ========================================================================

#[test]
fn test_refresh_statistics() {
    let db = AletheiaDB::new().unwrap();

    db.create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_node("Company", PropertyMapBuilder::new().build())
        .unwrap();

    db.refresh_statistics();

    let stats = db.statistics();
    assert!(stats.node_count() >= 3);
}

#[test]
fn test_invalidate_statistics() {
    let db = AletheiaDB::new().unwrap();

    db.create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    db.refresh_statistics();
    db.invalidate_statistics();

    // After invalidation, statistics should still be accessible (lazy refresh)
    let _stats = db.statistics();
}

#[test]
fn test_compress_commit_log() {
    let db = AletheiaDB::new().unwrap();

    // Create some transactions to have something to compress
    for _ in 0..10 {
        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
    }

    // Should not panic
    db.compress_commit_log();
}

#[test]
fn test_commit_log_memory_usage() {
    let db = AletheiaDB::new().unwrap();

    let initial_mem = db.commit_log_memory_usage();

    for _ in 0..10 {
        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
    }

    let after_mem = db.commit_log_memory_usage();
    // After 10 commits, memory should be >= initial
    assert!(after_mem >= initial_mem);
}

#[test]
fn test_get_compression_stats() {
    let db = AletheiaDB::new().unwrap();

    for _ in 0..5 {
        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
    }

    let stats = db.get_compression_stats();
    assert!(stats.total_transactions >= 5);
}

#[test]
fn test_should_compress_commit_log() {
    let db = AletheiaDB::new().unwrap();

    // With a very large threshold, should return false for empty db
    assert!(!db.should_compress_commit_log(usize::MAX));

    // With threshold of 0, should always return true (if any data)
    db.create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    assert!(db.should_compress_commit_log(0));
}

#[test]
fn test_should_compress_by_exception_count() {
    let db = AletheiaDB::new().unwrap();

    // Should not compress with very high threshold
    assert!(!db.should_compress_by_exception_count(usize::MAX));
}

#[test]
fn test_persist_indexes_without_persistence_enabled() {
    let db = AletheiaDB::new().unwrap();
    // Without persistence enabled, should return an error
    let result = db.persist_indexes();
    assert!(result.is_err());
}

#[test]
fn test_historical_stats_empty_db() {
    let db = AletheiaDB::new().unwrap();
    let stats = db.historical_stats().unwrap();
    assert_eq!(stats.total_node_versions, 0);
    assert_eq!(stats.node_anchor_count, 0);
}

#[test]
fn test_test_current_wal_lsn() {
    let db = AletheiaDB::new().unwrap();
    let lsn_before = db.__test_current_wal_lsn();

    db.create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let lsn_after = db.__test_current_wal_lsn();
    assert!(
        lsn_after > lsn_before,
        "LSN should advance after operations"
    );
}

#[test]
fn test_test_current_timestamp() {
    let db = AletheiaDB::new().unwrap();
    let ts = db.__test_current_timestamp();
    assert!(ts.wallclock() > 0, "Timestamp should be non-zero");
}

// ========================================================================
// Temporal Edge Cases
// ========================================================================

#[test]
fn test_get_node_at_time_nonexistent_node() {
    let db = AletheiaDB::new().unwrap();
    let now = crate::core::temporal::time::now();
    let result = db.get_node_at_time(NodeId::new(9999).unwrap(), now, now);
    assert!(result.is_err());
}

#[test]
fn test_get_edge_at_time_nonexistent_edge() {
    let db = AletheiaDB::new().unwrap();
    let now = crate::core::temporal::time::now();
    let result = db.get_edge_at_time(crate::core::id::EdgeId::new(9999).unwrap(), now, now);
    assert!(result.is_err());
}

#[test]
fn test_get_nodes_at_time_batch() {
    let db = AletheiaDB::new().unwrap();

    let (n1, ts1) = db
        .write_with_timestamp(|tx| {
            tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
        })
        .unwrap();

    let (n2, ts2) = db
        .write_with_timestamp(|tx| {
            tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
        })
        .unwrap();

    let query_ts = std::cmp::max(ts1, ts2);
    let results = db.get_nodes_at_time(&[n1, n2], query_ts, query_ts).unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, n1);
    assert!(results[0].1.is_some());
    assert_eq!(results[1].0, n2);
    assert!(results[1].1.is_some());
}

#[test]
fn test_get_nodes_at_time_batch_with_nonexistent() {
    let db = AletheiaDB::new().unwrap();

    let (n1, ts) = db
        .write_with_timestamp(|tx| tx.create_node("Person", PropertyMapBuilder::new().build()))
        .unwrap();

    let fake_id = NodeId::new(9999).unwrap();
    let results = db.get_nodes_at_time(&[n1, fake_id], ts, ts).unwrap();

    assert_eq!(results.len(), 2);
    assert!(results[0].1.is_some()); // Real node found
    assert!(results[1].1.is_none()); // Fake node not found
}

#[test]
fn test_get_nodes_at_time_empty_batch() {
    let db = AletheiaDB::new().unwrap();
    let now = crate::core::temporal::time::now();
    let results = db.get_nodes_at_time(&[], now, now).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_get_edges_at_time_batch() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let (e1, ts) = db
        .write_with_timestamp(|tx| {
            tx.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        })
        .unwrap();

    let results = db.get_edges_at_time(&[e1], ts, ts).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].1.is_some());
}

#[test]
fn test_get_edges_at_time_empty_batch() {
    let db = AletheiaDB::new().unwrap();
    let now = crate::core::temporal::time::now();
    let results = db.get_edges_at_time(&[], now, now).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_get_outgoing_edges_at_time() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let (_, ts) = db
        .write_with_timestamp(|tx| {
            tx.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        })
        .unwrap();

    let edges = db.get_outgoing_edges_at_time(alice, ts, ts);
    assert_eq!(edges.len(), 1);
}

#[test]
fn test_get_incoming_edges_at_time() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let (_, ts) = db
        .write_with_timestamp(|tx| {
            tx.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        })
        .unwrap();

    let edges = db.get_incoming_edges_at_time(bob, ts, ts);
    assert_eq!(edges.len(), 1);
}

#[test]
fn test_get_node_history_nonexistent() {
    let db = AletheiaDB::new().unwrap();
    let result = db.get_node_history(NodeId::new(9999).unwrap());
    assert!(result.is_err());
}

#[test]
fn test_get_node_at_version_nonexistent() {
    let db = AletheiaDB::new().unwrap();
    let result = db.get_node_at_version(NodeId::new(9999).unwrap(), 1);
    assert!(result.is_err());
}

#[test]
fn test_get_node_at_version_invalid_version() {
    let db = AletheiaDB::new().unwrap();
    let node_id = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    // Version 0 is invalid (versions are 1-indexed)
    let result = db.get_node_at_version(node_id, 0);
    assert!(result.is_err());
}

#[test]
fn test_get_node_at_version_beyond_latest() {
    let db = AletheiaDB::new().unwrap();
    let node_id = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    // Only version 1 exists, version 999 should fail
    let result = db.get_node_at_version(node_id, 999);
    assert!(result.is_err());
}

#[test]
fn test_get_edge_at_valid_time() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let (edge_id, ts) = db
        .write_with_timestamp(|tx| {
            tx.create_edge(
                alice,
                bob,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
            )
        })
        .unwrap();

    let edge = db.get_edge_at_valid_time(edge_id, ts).unwrap();
    assert_eq!(edge.source, alice);
    assert_eq!(edge.target, bob);
}

#[test]
fn test_get_edge_at_transaction_time() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let edge_id = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )
        .unwrap();

    let tx_time = crate::core::temporal::time::now();
    let edge = db.get_edge_at_transaction_time(edge_id, tx_time).unwrap();
    assert_eq!(edge.source, alice);
}

#[test]
fn test_get_edge_history_nonexistent() {
    let db = AletheiaDB::new().unwrap();
    let result = db.get_edge_history(crate::core::id::EdgeId::new(9999).unwrap());
    assert!(result.is_err());
}

#[test]
fn test_diff_node_versions_same_version() {
    let db = AletheiaDB::new().unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();

    let history = db.get_node_history(node_id).unwrap();
    let v1_id = history.first_version().unwrap().version_id;

    // Diff same version against itself
    let diff = db.diff_node_versions(node_id, v1_id, v1_id).unwrap();
    assert!(!diff.has_changes());
}

// ========================================================================
// Vector Edge Cases
// ========================================================================

#[test]
fn test_find_similar_without_index() {
    let db = AletheiaDB::new().unwrap();
    let node_id = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let result = db.find_similar(node_id, 10);
    assert!(result.is_err());
}

#[test]
fn test_find_similar_by_embedding_without_index() {
    let db = AletheiaDB::new().unwrap();
    let embedding = vec![0.1, 0.2, 0.3];
    let result = db.find_similar_by_embedding(&embedding, 10);
    assert!(result.is_err());
}

#[test]
fn test_search_vectors_in_without_index() {
    let db = AletheiaDB::new().unwrap();
    let embedding = vec![0.1, 0.2, 0.3];
    let result = db.search_vectors_in("embedding", &embedding, 10);
    assert!(result.is_err());
}

#[test]
fn test_find_similar_in_without_index() {
    let db = AletheiaDB::new().unwrap();
    let node_id = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let result = db.find_similar_in("embedding", node_id, 10);
    assert!(result.is_err());
}

#[test]
fn test_vector_index_builder_basic() {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = AletheiaDB::new().unwrap();
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(128, DistanceMetric::Cosine))
        .enable()
        .unwrap();

    assert!(db.has_vector_index("embedding"));
    assert!(!db.has_vector_index("other"));
}

#[test]
fn test_list_vector_indexes() {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = AletheiaDB::new().unwrap();

    assert!(db.list_vector_indexes().is_empty());

    db.enable_vector_index("embedding", HnswConfig::new(128, DistanceMetric::Cosine))
        .unwrap();

    let indexes = db.list_vector_indexes();
    assert_eq!(indexes.len(), 1);
    assert_eq!(indexes[0].property_name, "embedding");
}

#[test]
fn test_enable_vector_index_duplicate() {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(128, DistanceMetric::Cosine))
        .unwrap();

    // Enabling the same index again should error
    let result = db.enable_vector_index("embedding", HnswConfig::new(128, DistanceMetric::Cosine));
    assert!(result.is_err());
}

#[test]
fn test_find_similar_with_label_without_matches() {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    // Search with a label that doesn't match
    let results = db
        .find_similar_with_label(node_id, "NonExistentLabel", 10)
        .unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_find_similar_by_embedding_with_label() {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert_vector("embedding", &[1.0, 0.0, 0.0, 0.0])
            .build(),
    )
    .unwrap();

    db.create_node(
        "Company",
        PropertyMapBuilder::new()
            .insert("name", "Acme")
            .insert_vector("embedding", &[0.9, 0.1, 0.0, 0.0])
            .build(),
    )
    .unwrap();

    let query = vec![1.0, 0.0, 0.0, 0.0];

    // Only Person nodes
    let results = db
        .find_similar_by_embedding_with_label(&query, "Person", 10)
        .unwrap();
    assert_eq!(results.len(), 1);

    // Only Company nodes
    let results = db
        .find_similar_by_embedding_with_label(&query, "Company", 10)
        .unwrap();
    assert_eq!(results.len(), 1);

    // Nonexistent label
    let results = db
        .find_similar_by_embedding_with_label(&query, "NoLabel", 10)
        .unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_is_temporal_vector_index_enabled() {
    let db = AletheiaDB::new().unwrap();
    assert!(!db.is_temporal_vector_index_enabled());
}

#[test]
fn test_list_temporal_vector_indexes_empty() {
    let db = AletheiaDB::new().unwrap();
    assert!(db.list_temporal_vector_indexes().is_empty());
}

#[test]
fn test_find_similar_as_of_without_temporal_index() {
    let db = AletheiaDB::new().unwrap();
    let now = crate::core::temporal::time::now();
    let result = db.find_similar_as_of(&[0.1, 0.2, 0.3], 10, now);
    assert!(result.is_err());
}

// ========================================================================
// Update Operations via Transactions
// ========================================================================

#[test]
fn test_update_node_via_transaction() {
    let db = AletheiaDB::new().unwrap();

    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30i64)
                .build(),
        )
        .unwrap();

    db.write(|tx| {
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert("name", "Alice Updated")
                .insert("age", 31i64)
                .build(),
        )
    })
    .unwrap();

    let node = db.get_node(node_id).unwrap();
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Alice Updated")
    );
    assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(31));
}

#[test]
fn test_update_nonexistent_node() {
    let db = AletheiaDB::new().unwrap();
    let result: Result<()> = db.write(|tx| {
        tx.update_node(
            NodeId::new(9999).unwrap(),
            PropertyMapBuilder::new().build(),
        )
    });
    assert!(result.is_err());
}

#[test]
fn test_update_edge_via_transaction() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let edge_id = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("weight", 1.0f64).build(),
        )
        .unwrap();

    db.write(|tx| {
        tx.update_edge(
            edge_id,
            PropertyMapBuilder::new().insert("weight", 2.0f64).build(),
        )
    })
    .unwrap();

    let edge = db.get_edge(edge_id).unwrap();
    assert_eq!(
        edge.properties.get("weight"),
        Some(&PropertyValue::Float(2.0))
    );
}

#[test]
fn test_update_nonexistent_edge() {
    let db = AletheiaDB::new().unwrap();
    let result: Result<()> = db.write(|tx| {
        tx.update_edge(
            crate::core::id::EdgeId::new(9999).unwrap(),
            PropertyMapBuilder::new().build(),
        )
    });
    assert!(result.is_err());
}

// ========================================================================
// Multiple Operations in Single Transaction
// ========================================================================

#[test]
fn test_multiple_creates_in_single_transaction() {
    let db = AletheiaDB::new().unwrap();

    let node_ids = db
        .write(|tx| {
            let mut ids = Vec::new();
            for i in 0..10 {
                let id = tx.create_node(
                    "Item",
                    PropertyMapBuilder::new().insert("index", i as i64).build(),
                )?;
                ids.push(id);
            }
            Ok::<_, Error>(ids)
        })
        .unwrap();

    assert_eq!(node_ids.len(), 10);
    assert_eq!(db.node_count(), 10);
}

#[test]
fn test_create_then_delete_edge_across_transactions() {
    let db = AletheiaDB::new().unwrap();

    // Create nodes and edge in one transaction
    let (n1, n2, edge_id) = db
        .write(|tx| {
            let n1 = tx.create_node("Person", PropertyMapBuilder::new().build())?;
            let n2 = tx.create_node("Person", PropertyMapBuilder::new().build())?;
            let e = tx.create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())?;
            Ok::<_, Error>((n1, n2, e))
        })
        .unwrap();

    assert_eq!(db.node_count(), 2);
    assert_eq!(db.edge_count(), 1);

    // Delete edge in a second transaction
    db.write(|tx| tx.delete_edge(edge_id)).unwrap();
    assert_eq!(db.edge_count(), 0);

    // Nodes still exist
    assert!(db.get_node(n1).is_ok());
    assert!(db.get_node(n2).is_ok());
}

// ========================================================================
// Self-edges and Multi-edges
// ========================================================================

#[test]
fn test_self_edge() {
    let db = AletheiaDB::new().unwrap();

    let node = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let edge_id = db
        .create_edge(node, node, "SELF_REF", PropertyMapBuilder::new().build())
        .unwrap();

    let edge = db.get_edge(edge_id).unwrap();
    assert_eq!(edge.source, node);
    assert_eq!(edge.target, node);
    assert_eq!(db.out_degree(node), 1);
    assert_eq!(db.in_degree(node), 1);
}

#[test]
fn test_multiple_edges_same_nodes() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(alice, bob, "WORKS_WITH", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(bob, alice, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    assert_eq!(db.edge_count(), 3);
    assert_eq!(db.out_degree(alice), 2);
    assert_eq!(db.in_degree(alice), 1);
    assert_eq!(db.out_degree(bob), 1);
    assert_eq!(db.in_degree(bob), 2);
}

// ========================================================================
// Convenience Methods Tests
// ========================================================================

#[test]
fn test_convenience_update_node() {
    let db = AletheiaDB::new().unwrap();
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("age", 30i64).build(),
        )
        .unwrap();

    db.update_node(
        node_id,
        PropertyMapBuilder::new().insert("age", 31i64).build(),
    )
    .unwrap();

    let node = db.get_node(node_id).unwrap();
    assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(31));
}

#[test]
fn test_convenience_update_edge() {
    let db = AletheiaDB::new().unwrap();
    let n1 = db
        .create_node("P", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = db
        .create_node("P", PropertyMapBuilder::new().build())
        .unwrap();
    let edge_id = db
        .create_edge(
            n1,
            n2,
            "KNOWS",
            PropertyMapBuilder::new().insert("w", 1i64).build(),
        )
        .unwrap();

    db.update_edge(edge_id, PropertyMapBuilder::new().insert("w", 2i64).build())
        .unwrap();

    let edge = db.get_edge(edge_id).unwrap();
    assert_eq!(edge.properties.get("w").and_then(|v| v.as_int()), Some(2));
}

#[test]
fn test_convenience_delete_node() {
    let db = AletheiaDB::new().unwrap();
    let node_id = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    db.delete_node(node_id).unwrap();

    assert!(db.get_node(node_id).is_err());
}

#[test]
fn test_convenience_delete_node_cascade() {
    let db = AletheiaDB::new().unwrap();
    let n1 = db
        .create_node("P", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = db
        .create_node("P", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(n1, n2, "K", PropertyMapBuilder::new().build())
        .unwrap();

    db.delete_node_cascade(n1).unwrap();

    assert!(db.get_node(n1).is_err());
    assert_eq!(db.edge_count(), 0);
}

#[test]
fn test_convenience_delete_edge() {
    let db = AletheiaDB::new().unwrap();
    let n1 = db
        .create_node("P", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = db
        .create_node("P", PropertyMapBuilder::new().build())
        .unwrap();
    let edge_id = db
        .create_edge(n1, n2, "K", PropertyMapBuilder::new().build())
        .unwrap();

    db.delete_edge(edge_id).unwrap();

    assert!(db.get_edge(edge_id).is_err());
}
