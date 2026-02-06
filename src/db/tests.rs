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
