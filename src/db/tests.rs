//! Tests for the main AletheiaDB interface.
//!
//! This module verifies the high-level API for creating, reading, and managing
//! nodes and edges, ensuring the database core functions correctly from a user's perspective.

use super::*;
use crate::api::transaction::{ReadOps, WriteOps};
use crate::core::GLOBAL_INTERNER;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::property::{PropertyMapBuilder, PropertyValue};

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
        Err(crate::core::error::Error::Storage(
            crate::core::error::StorageError::InconsistentState {
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
    // Use temp dir to avoid conflicts/corruption from default paths
    let temp_dir = tempfile::tempdir().unwrap();
    let mut config = crate::config::AletheiaDBConfig::default();
    config.wal.wal_dir = temp_dir.path().join("wal");
    config.persistence.data_dir = temp_dir.path().join("data");

    // AletheiaDB::with_unified_config() should return Result<Self>
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
    let err = result.expect_err("Expected an error");
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

/// Issue #450: `on_temporal_vector_transaction` must notify EVERY enabled
/// temporal vector index -- not just one (the removed legacy single-index
/// state only notified the most recently enabled index). With
/// `SnapshotStrategy::TransactionInterval(1)`, each notification must advance
/// the snapshot count of BOTH indexes.
#[test]
fn test_on_temporal_vector_transaction_notifies_all_indexes() {
    use crate::index::vector::temporal::{SnapshotStrategy, TemporalVectorConfig};
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = AletheiaDB::new().unwrap();

    for property in ["a_embedding", "b_embedding"] {
        let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);
        let temporal_config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            ..TemporalVectorConfig::default_with_hnsw(hnsw_config)
        };
        db.enable_temporal_vector_index(property, temporal_config)
            .unwrap();
    }

    // Add a vector to each temporal index.
    db.create_node(
        "Doc",
        PropertyMapBuilder::new()
            .insert_vector("a_embedding", &[1.0f32, 0.0, 0.0, 0.0])
            .build(),
    )
    .unwrap();
    db.create_node(
        "Doc",
        PropertyMapBuilder::new()
            .insert_vector("b_embedding", &[0.0f32, 1.0, 0.0, 0.0])
            .build(),
    )
    .unwrap();

    let a_index = db
        .current
        .get_temporal_vector_index_for("a_embedding")
        .expect("temporal index for 'a_embedding' should exist");
    let b_index = db
        .current
        .get_temporal_vector_index_for("b_embedding")
        .expect("temporal index for 'b_embedding' should exist");
    let a_before = a_index.snapshot_count();
    let b_before = b_index.snapshot_count();

    db.current.on_temporal_vector_transaction().unwrap();

    assert!(
        a_index.snapshot_count() > a_before,
        "transaction notification must reach the 'a_embedding' temporal index \
         (snapshot count stayed at {a_before})"
    );
    assert!(
        b_index.snapshot_count() > b_before,
        "transaction notification must reach the 'b_embedding' temporal index \
         (snapshot count stayed at {b_before})"
    );
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
fn test_count_connected_edges() {
    // Issue #3209: additive, non-breaking helper to learn how many edges connect
    // a node prior to deletion, so callers can decide before acting.
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

    // No edges yet
    assert_eq!(db.count_connected_edges(alice).unwrap(), 0);

    // One outgoing edge
    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    assert_eq!(db.count_connected_edges(alice).unwrap(), 1);

    // One incoming edge (counts both directions)
    db.create_edge(charlie, alice, "FOLLOWS", PropertyMapBuilder::new().build())
        .unwrap();
    assert_eq!(db.count_connected_edges(alice).unwrap(), 2);
    assert_eq!(db.count_connected_edges(bob).unwrap(), 1);
    assert_eq!(db.count_connected_edges(charlie).unwrap(), 1);

    // Missing node returns an error rather than a bogus count
    let missing = NodeId::new(999_999).unwrap();
    assert!(db.count_connected_edges(missing).is_err());
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
            crate::core::error::StorageError::InconsistentState {
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
            crate::core::error::StorageError::InconsistentState {
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
fn test_refresh_statistics_avg_delta_chain_from_historical() {
    // Issue #366: refresh_statistics must feed the planner the actual average
    // delta chain length computed from historical storage, not a hardcoded
    // estimate.
    let db = AletheiaDB::new().unwrap();

    let node = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("v", 0i64).build(),
        )
        .unwrap();
    // Three updates create delta versions behind the initial anchor.
    for i in 1..=3i64 {
        db.update_node_with_valid_time(
            node,
            PropertyMapBuilder::new().insert("v", i).build(),
            None,
        )
        .unwrap();
    }

    db.refresh_statistics();

    // Hand-compute the expected value from the historical storage counters.
    let hist = db.historical_stats().unwrap();
    let total_deltas = (hist.node_delta_count + hist.edge_delta_count) as f64;
    let total_anchors = (hist.node_anchor_count + hist.edge_anchor_count) as f64;
    assert!(total_anchors > 0.0, "expected at least one anchor version");
    assert!(
        total_deltas > 0.0,
        "expected delta versions from the updates"
    );
    let expected = total_deltas / total_anchors;

    let stats = db.statistics();
    assert!(
        (stats.average_delta_chain_length() - expected).abs() < f64::EPSILON,
        "avg delta chain {} should equal deltas/anchors {}",
        stats.average_delta_chain_length(),
        expected
    );
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
#[allow(deprecated)] // exercises deprecated wrappers for back-compat
fn test_find_similar_without_index() {
    let db = AletheiaDB::new().unwrap();
    let node_id = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let result = db.find_similar(node_id, 10);
    assert!(result.is_err());
}

#[test]
#[allow(deprecated)] // exercises deprecated wrappers for back-compat
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
#[allow(deprecated)] // exercises deprecated wrappers for back-compat
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
#[allow(deprecated)] // exercises deprecated wrappers for back-compat
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
#[allow(deprecated)] // exercises deprecated wrappers for back-compat
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

#[test]
fn test_debug_implementation() {
    let db = AletheiaDB::new().unwrap();
    let debug_output = format!("{:?}", db);

    assert!(debug_output.contains("AletheiaDB"));
    assert!(debug_output.contains("current_timestamp"));
    assert!(debug_output.contains("default_durability"));
    assert!(debug_output.contains("persistence_enabled"));
    assert!(debug_output.contains("stats"));
}

#[cfg(feature = "observability")]
fn poison_mutex<T>(mutex: &std::sync::Arc<std::sync::Mutex<T>>) {
    let mutex = std::sync::Arc::clone(mutex);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _guard = mutex.lock().expect("failed to lock mutex for poisoning");
        panic!("intentional mutex poison for metrics test");
    }));
}

#[cfg(feature = "observability")]
#[test]
#[serial_test::serial]
fn test_create_node_transaction_error_counted_once_when_lock_poisoned() {
    crate::observability::METRICS.reset();
    let db = AletheiaDB::new().unwrap();

    poison_mutex(&db.current_timestamp);

    let result = db.create_node("Person", PropertyMapBuilder::new().build());
    assert!(result.is_err());

    let snapshot = crate::observability::METRICS.snapshot();
    assert_eq!(snapshot.error_transaction_total, 1);
}

#[cfg(feature = "observability")]
#[test]
#[serial_test::serial]
fn test_vector_builder_duplicate_enable_counts_error_once() {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    crate::observability::METRICS.reset();
    let result = db
        .vector_index("embedding")
        .hnsw(HnswConfig::new(4, DistanceMetric::Cosine))
        .enable();
    assert!(result.is_err());

    let snapshot = crate::observability::METRICS.snapshot();
    assert_eq!(snapshot.error_vector_total, 1);
}

#[cfg(feature = "observability")]
#[test]
#[serial_test::serial]
fn test_read_closure_db_error_counts_once() {
    crate::observability::METRICS.reset();
    let db = AletheiaDB::new().unwrap();

    let missing_id = NodeId::new(999_999).unwrap();
    let result: Result<()> = db.read(|tx| {
        tx.get_node(missing_id)?;
        Ok(())
    });
    assert!(result.is_err());

    let snapshot = crate::observability::METRICS.snapshot();
    assert_eq!(snapshot.error_storage_total, 1);
}

#[cfg(feature = "observability")]
#[test]
#[serial_test::serial]
fn test_write_commit_error_counts_once() {
    crate::observability::METRICS.reset();
    let db = AletheiaDB::new().unwrap();

    poison_mutex(&db.commit_clock_observed_at);

    let result: Result<()> = db.write(|tx| {
        tx.create_node("Person", PropertyMapBuilder::new().build())?;
        Ok(())
    });
    assert!(result.is_err());

    let snapshot = crate::observability::METRICS.snapshot();
    assert_eq!(snapshot.error_transaction_total, 1);
}

// ==================== Schema Discovery Tests (Issue #3214) ====================

#[test]
fn test_schema_empty_database() {
    let db = AletheiaDB::new().unwrap();

    let schema = db.schema().unwrap();

    assert!(schema.node_labels.is_empty());
    assert!(schema.edge_types.is_empty());
    assert_eq!(schema.total_nodes, 0);
    assert_eq!(schema.total_edges, 0);
    assert!(!schema.sampled);
    assert!(schema.as_of.is_none());
}

#[test]
fn test_schema_populated_graph_labels_and_edge_types() {
    let db = AletheiaDB::new().unwrap();

    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30i64)
                .build(),
        )
        .unwrap();
    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert("email", "bob@example.com")
                .build(),
        )
        .unwrap();
    let acme = db
        .create_node(
            "Company",
            PropertyMapBuilder::new().insert("name", "Acme").build(),
        )
        .unwrap();

    db.create_edge(
        alice,
        bob,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2020i64).build(),
    )
    .unwrap();
    db.create_edge(
        alice,
        acme,
        "WORKS_AT",
        PropertyMapBuilder::new().insert("role", "Engineer").build(),
    )
    .unwrap();

    let schema = db.schema().unwrap();

    // Sorted by label.
    assert_eq!(schema.node_labels.len(), 2);
    assert_eq!(schema.node_labels[0].label, "Company");
    assert_eq!(schema.node_labels[0].count, 1);
    assert_eq!(schema.node_labels[1].label, "Person");
    assert_eq!(schema.node_labels[1].count, 2);

    // Property keys are the union across all nodes of that label, sorted.
    assert_eq!(
        schema.node_labels[1].property_keys,
        vec!["age".to_string(), "email".to_string(), "name".to_string()]
    );

    assert_eq!(schema.edge_types.len(), 2);
    assert_eq!(schema.edge_types[0].edge_type, "KNOWS");
    assert_eq!(schema.edge_types[0].count, 1);
    assert_eq!(
        schema.edge_types[0].property_keys,
        vec!["since".to_string()]
    );
    assert_eq!(schema.edge_types[1].edge_type, "WORKS_AT");
    assert_eq!(schema.edge_types[1].count, 1);
    assert_eq!(schema.edge_types[1].property_keys, vec!["role".to_string()]);

    assert_eq!(schema.total_nodes, 3);
    assert_eq!(schema.total_edges, 2);
    assert!(!schema.sampled);
}

#[test]
fn test_schema_counts_are_consistent_with_scan_and_count_nodes() {
    let db = AletheiaDB::new().unwrap();

    for _ in 0..3 {
        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
    }
    for _ in 0..2 {
        db.create_node("Company", PropertyMapBuilder::new().build())
            .unwrap();
    }

    let schema = db.schema().unwrap();

    let mut total_from_labels = 0;
    for label_schema in &schema.node_labels {
        let scanned = db.scan_nodes_by_label(&label_schema.label).count();
        assert_eq!(
            label_schema.count, scanned,
            "count for label '{}' must match scan_nodes_by_label",
            label_schema.label
        );
        total_from_labels += label_schema.count;
    }
    assert_eq!(total_from_labels, db.node_count());
    assert_eq!(schema.total_nodes, db.node_count());
}

#[test]
fn test_schema_as_of_label_absent_before_first_write() {
    use crate::core::hlc::HybridTimestamp;

    let db = AletheiaDB::new().unwrap();

    let jan_1 = HybridTimestamp::new(1_704_067_200_000_000, 0).unwrap(); // 2024-01-01
    let jan_15 = HybridTimestamp::new(1_705_276_800_000_000, 0).unwrap(); // 2024-01-15

    // Before any writes have happened, the schema is empty.
    let before_any_writes = db.schema_as_of(jan_1, jan_1).unwrap();
    assert!(before_any_writes.node_labels.is_empty());

    // Create a "Widget" node with valid_time = Jan 1, but the write is recorded
    // (transaction time) at the real current time, which is after jan_15.
    db.write(|tx| {
        tx.create_node_with_valid_time(
            "Widget",
            PropertyMapBuilder::new().insert("name", "Thing").build(),
            Some(jan_1),
        )
    })
    .unwrap();

    // As of Jan 15 transaction time (before the write was actually recorded),
    // the "Widget" label must still be absent.
    let still_absent = db.schema_as_of(jan_15, jan_1).unwrap();
    assert!(
        still_absent.node_labels.iter().all(|l| l.label != "Widget"),
        "label should be absent before its first write was committed"
    );

    // As of the current bi-temporal instant, the label is present.
    let now = crate::core::temporal::time::now();
    let present = db.schema_as_of(now, now).unwrap();
    let widget = present
        .node_labels
        .iter()
        .find(|l| l.label == "Widget")
        .expect("Widget label should be present as of now");
    assert_eq!(widget.count, 1);

    assert_eq!(
        present.as_of,
        Some(crate::db::schema::SchemaInstant {
            valid_time: now,
            transaction_time: now,
        })
    );
}

#[test]
fn test_schema_as_of_entity_cap_is_configurable_and_discloses_sampling() {
    use crate::config::{AletheiaDBConfig, HistoricalConfigBuilder};
    use crate::test_utils::create_test_db_with_config;

    // A tiny cap makes the truncation path cheap to exercise directly,
    // instead of needing 50,000+ real entities to hit the default cap.
    let config = AletheiaDBConfig::builder()
        .historical(
            HistoricalConfigBuilder::new()
                .max_schema_as_of_entities(2)
                .build(),
        )
        .build();
    let (_temp_dir, db) = create_test_db_with_config(config).unwrap();

    for _ in 0..3 {
        db.create_node("Widget", PropertyMapBuilder::new().build())
            .unwrap();
    }

    let now = crate::core::temporal::time::now();
    let schema = db.schema_as_of(now, now).unwrap();
    assert!(
        schema.sampled,
        "a cap of 2 with 3 versioned nodes must disclose truncation"
    );

    // schema() (current state) is always exhaustive, regardless of the
    // schema_as_of() cap.
    let current = db.schema().unwrap();
    assert!(!current.sampled);
    assert_eq!(current.total_nodes, 3);
}

// ============================================================================
// DatabaseStats unit tests (Issue #3222)
// ============================================================================

/// `AletheiaDB::stats()` must serialize to the documented shape, with
/// disabled subsystems explicitly tagged (`enabled: false`, no count keys)
/// and enabled subsystems carrying their counters.
///
/// Serialization (`serde::Serialize` on `DatabaseStats`) only exists when
/// `config-toml` or `mcp-server` is enabled, so this test is gated the same
/// way; `test_stats_populated_matches_underlying_counters` keeps the
/// non-serde behavior covered in minimal builds.
#[cfg(feature = "serde")]
#[test]
fn test_stats_serialization_shape_empty_db() {
    let db = AletheiaDB::new().unwrap();
    let stats = db.stats();
    let value = serde_json::to_value(&stats).expect("DatabaseStats must be serializable");

    assert_eq!(value["current"]["node_count"], serde_json::json!(0));
    assert_eq!(value["current"]["edge_count"], serde_json::json!(0));
    assert_eq!(
        value["historical"]["total_node_versions"],
        serde_json::json!(0)
    );
    assert_eq!(value["historical"]["anchor_count"], serde_json::json!(0));
    assert_eq!(value["historical"]["delta_count"], serde_json::json!(0));

    let cold = value["cold_storage"].as_object().unwrap();
    assert_eq!(cold["enabled"], serde_json::json!(false));
    assert!(
        !cold.contains_key("node_versions_stored"),
        "disabled cold storage must not report counts: {value}"
    );

    let wal = value["wal"].as_object().unwrap();
    assert_eq!(wal["enabled"], serde_json::json!(true));
    assert!(wal["current_lsn"].as_u64().unwrap() >= 1);
    assert!(wal["durability_mode"].is_string());
}

/// Populated DB: `stats()` mirrors the underlying O(1) counters exactly.
#[test]
fn test_stats_populated_matches_underlying_counters() {
    let db = AletheiaDB::new().unwrap();
    let n1 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let _n2 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    db.update_node_with_valid_time(
        n1,
        PropertyMapBuilder::new().insert("name", "Alice").build(),
        None,
    )
    .unwrap();

    let stats = db.stats();
    assert_eq!(stats.current.node_count, 2);
    assert_eq!(stats.current.edge_count, 0);

    let hist = db.historical_stats().unwrap();
    assert_eq!(
        stats.historical.total_node_versions,
        hist.total_node_versions
    );
    // 2 creates + 1 update = exactly 3 node versions.
    assert_eq!(stats.historical.total_node_versions, 3);
    assert_eq!(stats.historical.unique_nodes, hist.unique_nodes);
    assert_eq!(
        stats.historical.anchor_count,
        hist.node_anchor_count + hist.edge_anchor_count
    );
    assert_eq!(
        stats.historical.delta_count,
        hist.node_delta_count + hist.edge_delta_count
    );
    // With the default anchor interval (10), the update after a create is
    // stored as a delta — the compression machinery must actually engage.
    assert!(
        stats.historical.delta_count >= 1,
        "an update following a create must produce at least one delta, got: {stats:?}"
    );
    assert!(
        stats.historical.compression_ratio < 1.0,
        "with >= 1 delta the anchor share must drop below 1.0, got: {}",
        stats.historical.compression_ratio
    );
}

/// With cold storage configured, `stats()` reports the cold tier as enabled
/// with counters and the hot/warm/cold access distribution.
///
/// Gated like `test_stats_serialization_shape_empty_db`: the serde derive
/// this test exercises only exists under `config-toml`/`mcp-server`.
#[cfg(feature = "serde")]
#[test]
fn test_stats_cold_storage_enabled() {
    use crate::config::{AletheiaDBConfig, HistoricalConfigBuilder, WalConfigBuilder};

    let temp_dir = tempfile::tempdir().unwrap();
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(temp_dir.path().join("wal"))
                .build(),
        )
        .persistence(crate::storage::index_persistence::PersistenceConfig {
            enabled: false,
            ..Default::default()
        })
        .historical(
            HistoricalConfigBuilder::new()
                .enable_cold_storage(true)
                .cold_storage_path(temp_dir.path().join("cold.redb"))
                .build(),
        )
        .build();
    let db = AletheiaDB::with_unified_config(config).unwrap();

    let value = serde_json::to_value(db.stats()).unwrap();
    let cold = value["cold_storage"].as_object().unwrap();
    assert_eq!(cold["enabled"], serde_json::json!(true));
    assert_eq!(cold["node_versions_stored"], serde_json::json!(0));
    assert!(cold["tier_access"].is_object());
}

/// After versions actually migrate to the cold tier, `stats()` must report
/// them under `cold_storage` (and the hot historical counters must shrink
/// accordingly) — the enabled-tier path with nonzero counters.
#[test]
fn test_stats_cold_storage_reports_migrated_versions() {
    use crate::storage::migration::{MigrationPolicyBuilder, MigrationService};
    use crate::storage::redb_cold_storage::RedbColdStorage;
    use crate::storage::tiered_storage::TieredStorage;
    use std::sync::Arc;
    use std::time::Duration;

    let db = AletheiaDB::new().unwrap();
    let n1 = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    db.update_node_with_valid_time(
        n1,
        PropertyMapBuilder::new().insert("name", "Alice").build(),
        None,
    )
    .unwrap();

    // Attach a cold tier and migrate everything older than the head version.
    let temp_dir = tempfile::tempdir().unwrap();
    let cold =
        Arc::new(RedbColdStorage::with_default_config(temp_dir.path().join("cold.redb")).unwrap());
    let tiered = Arc::new(TieredStorage::with_default_config(Arc::clone(&cold)));
    db.__test_historical_storage()
        .write()
        .set_tiered_storage(tiered);

    let policy = MigrationPolicyBuilder::new()
        .age_threshold(Duration::ZERO)
        .build();
    let service = MigrationService::new(Arc::clone(&cold), policy);
    let hot_versions_before = db.stats().historical.total_node_versions;
    let migrated = db
        .__test_historical_storage()
        .write()
        .migrate_to_cold(&service)
        .unwrap();
    assert!(migrated >= 1, "at least the non-head version must migrate");

    let stats = db.stats();
    assert!(stats.cold_storage.enabled);
    let details = stats
        .cold_storage
        .details
        .expect("enabled cold storage must carry details");
    assert_eq!(
        details.node_versions_stored, migrated as u64,
        "cold tier must report exactly the migrated versions"
    );
    assert_eq!(
        stats.historical.total_node_versions,
        hot_versions_before - migrated,
        "migrated versions must leave the hot historical counters"
    );
}

/// Regression test for Issue #3425: a checkpoint/backup snapshot must never
/// capture a *committed* node whose `commit_timestamp` is still `None`.
///
/// The latent bug: the commit path finalized `commit_timestamp`
/// (`None -> Some(T)`) *after* dropping the `historical.write()` guard and took
/// no snapshot lock, leaving a narrow window in which a snapshot holding
/// `historical.read()` could clone a committed node while its timestamp was
/// still `None` (which downstream visibility logic treats as uncommitted).
///
/// The hardening (Option A in the issue) holds the `historical.write()` guard
/// across finalization, making the current-write + finalize atomic w.r.t. any
/// `historical.read()`-holding snapshot.
///
/// This test uses the `#[cfg(test)]` pre-finalize hook to deterministically park
/// a committing writer in that exact window while a checker thread mimics
/// `create_checkpoint`'s snapshot block -- holding `historical.read()` (outer)
/// then `snapshot_lock.write()` (inner), exactly as the issue describes -- and
/// inspects every captured node. Before the fix the checker observes
/// `commit_timestamp: None` (RED). After the fix the checker's `historical.read()`
/// blocks until finalize completes under the writer's held guard, so it only ever
/// sees resolved timestamps (GREEN).
/// Serializes the finalize-window tests (#3425). Both install the *global*
/// `commit_test_hooks` pre-finalize hook, so they must never run concurrently or
/// they would clobber each other's hook and interleave unpredictably.
static FINALIZE_WINDOW_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn test_checkpoint_never_captures_uncommitted_finalize_window_3425() {
    use crate::api::transaction::write::commit_test_hooks;
    use crate::storage::wal::LSN;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    let _serial = FINALIZE_WINDOW_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());

    let db = Arc::new(AletheiaDB::new().unwrap());

    // Writer sets this when it enters the finalize window (pre-finalize hook).
    let in_window = Arc::new(AtomicBool::new(false));
    // Both threads rendezvous before the writer begins its commit.
    let barrier = Arc::new(Barrier::new(2));

    // Install a pre-finalize hook that (1) announces the window and (2) parks the
    // writer long enough for the checker to attempt its snapshot. In the buggy
    // code the `historical.write()` guard is already dropped here; in the fixed
    // code it is still held across this sleep.
    {
        let in_window = Arc::clone(&in_window);
        commit_test_hooks::set_pre_finalize_hook(Arc::new(move || {
            in_window.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
        }));
    }

    // Writer thread: commit a single node. Its commit fires the hook.
    let writer = {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            let props = PropertyMapBuilder::new().insert("name", "Alice").build();
            db.create_node("Person", props).expect("create_node failed");
        })
    };

    // Checker thread: once the writer is in the window, replicate the checkpoint
    // snapshot block and assert no captured node is finalize-pending.
    let checker = {
        let db = Arc::clone(&db);
        let in_window = Arc::clone(&in_window);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            // Spin until the writer signals it has entered the finalize window.
            let deadline = Instant::now() + Duration::from_secs(5);
            while !in_window.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "writer never entered window");
                std::thread::yield_now();
            }

            // Mimic `create_checkpoint`'s consistent-snapshot block: hold the
            // historical read guard (outer) then the snapshot lock (inner).
            let hist_guard = db.historical.read();
            let snap_lock = db.current.snapshot_lock.write();
            let snapshot = db.current.create_snapshot(LSN(0));
            let captured_none = snapshot
                .nodes()
                .iter()
                .any(|n| n.metadata.commit_timestamp.is_none());
            drop(snap_lock);
            drop(hist_guard);
            captured_none
        })
    };

    writer.join().expect("writer thread panicked");
    let captured_none = checker.join().expect("checker thread panicked");

    commit_test_hooks::clear_pre_finalize_hook();

    assert!(
        !captured_none,
        "Issue #3425: checkpoint snapshot captured a committed node with \
         commit_timestamp: None inside the finalize window"
    );
}

/// Regression test for Issue #3425 on the **`backup()`** path specifically.
///
/// The checkpoint test above only replicates the checkpoint lock ordering. This
/// test drives the *real* `AletheiaDB::backup()` code concurrently with a writer
/// parked in the finalize window, and — via a `#[cfg(test)]` seam inside
/// `backup()` — inspects the exact current-storage snapshot `backup()` captured.
///
/// Before the Finding-1 fix, `backup()` cloned the current snapshot BEFORE (and
/// outside) the `historical.read()` guard, so it could observe a committed node
/// whose `commit_timestamp` is still `None` (RED). After the fix the snapshot is
/// taken INSIDE `historical.read()`, which blocks on the writer's held
/// `historical.write()` guard until finalize completes, so it only ever sees a
/// resolved timestamp (GREEN).
#[test]
fn test_backup_never_captures_uncommitted_finalize_window_3425() {
    use crate::api::transaction::write::commit_test_hooks;
    use crate::db::backup::backup_test_hooks;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    let _serial = FINALIZE_WINDOW_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());

    let db = Arc::new(AletheiaDB::new().unwrap());

    // Writer sets this when it enters the finalize window (pre-finalize hook).
    let in_window = Arc::new(AtomicBool::new(false));
    // Records whether the snapshot that real `backup()` captured held a committed
    // node with `commit_timestamp: None` (the bug).
    let captured_none = Arc::new(AtomicBool::new(false));
    // Set once the backup hook has fired (so we know backup reached its snapshot).
    let backup_snapshotted = Arc::new(AtomicBool::new(false));
    // Both threads rendezvous before the writer begins its commit.
    let barrier = Arc::new(Barrier::new(2));

    // Pre-finalize hook: announce the window and park the writer so the backup
    // thread can run. In the buggy code the `historical.write()` guard is already
    // dropped here; in the fixed code it is still held across this sleep.
    {
        let in_window = Arc::clone(&in_window);
        commit_test_hooks::set_pre_finalize_hook(Arc::new(move || {
            in_window.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
        }));
    }

    // Backup post-snapshot hook: inspect the real snapshot `backup()` took.
    {
        let captured_none = Arc::clone(&captured_none);
        let backup_snapshotted = Arc::clone(&backup_snapshotted);
        backup_test_hooks::set_post_current_snapshot_hook(Arc::new(move |snapshot| {
            let has_none = snapshot
                .nodes()
                .iter()
                .any(|n| n.metadata.commit_timestamp.is_none());
            captured_none.store(has_none, Ordering::SeqCst);
            backup_snapshotted.store(true, Ordering::SeqCst);
        }));
    }

    // Writer thread: commit a single node. Its commit fires the pre-finalize hook.
    let writer = {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            let props = PropertyMapBuilder::new().insert("name", "Alice").build();
            db.create_node("Person", props).expect("create_node failed");
        })
    };

    // Backup thread: once the writer is in the window, run the real backup path.
    let backup_thread = {
        let db = Arc::clone(&db);
        let in_window = Arc::clone(&in_window);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            // Spin until the writer signals it has entered the finalize window.
            let deadline = Instant::now() + Duration::from_secs(5);
            while !in_window.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "writer never entered window");
                std::thread::yield_now();
            }

            let tmp = tempfile::TempDir::new().expect("tempdir");
            let path = tmp.path().join("finalize_window.albk");
            db.backup(&path).expect("backup failed");
        })
    };

    writer.join().expect("writer thread panicked");
    backup_thread.join().expect("backup thread panicked");

    commit_test_hooks::clear_pre_finalize_hook();
    backup_test_hooks::clear_post_current_snapshot_hook();

    assert!(
        backup_snapshotted.load(Ordering::SeqCst),
        "backup never captured its current snapshot (hook did not fire)"
    );
    assert!(
        !captured_none.load(Ordering::SeqCst),
        "Issue #3425: backup() captured a committed node with \
         commit_timestamp: None inside the finalize window"
    );
}
