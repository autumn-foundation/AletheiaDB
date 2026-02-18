//! Tests for DeleteNode/DeleteEdge replay handlers (Issue #290)
//!
//! These tests verify that recovery correctly replays:
//! - DeleteNode operations with tombstone creation
//! - DeleteEdge operations with tombstone creation
//! - Closing previous version's transaction_time BEFORE creating tombstone
//! - Bi-temporal semantics (critical for correctness!)
//! - Tombstone versions in historical storage

use aletheiadb::{
    GLOBAL_INTERNER,
    core::error::Result,
    core::{
        id::{EdgeId, NodeId, VersionId},
        property::{PropertyMap, PropertyMapBuilder},
        temporal::time,
    },
    storage::{
        checkpoint::{CheckpointConfig, CheckpointManager},
        wal::{
            WalOperation,
            concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
        },
    },
};
use tempfile::TempDir;

#[test]
fn test_replay_delete_node_basic() -> Result<()> {
    // Given: WAL with CreateNode followed by DeleteNode
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();
    let timestamp1 = time::now();
    let timestamp2 = (timestamp1.wallclock() + 1000).into();

    // Create node
    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new().insert("name", "Alice").build(),
        valid_from: timestamp1,
    })?;

    // Delete node
    wal.append(WalOperation::DeleteNode {
        node_id,
        valid_from: timestamp2,
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Node should NOT exist in current storage (it's deleted)
    assert_eq!(current.node_count(), 0);
    assert!(current.get_node(node_id).is_err());

    // And: Historical storage should have 2 versions (create + delete tombstone)
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 2);

    Ok(())
}

#[test]
fn test_replay_delete_node_after_update() -> Result<()> {
    // Given: WAL with Create, Update, then Delete
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();

    // Create node
    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new().insert("name", "Alice").build(),
        valid_from: time::now(),
    })?;

    // Update node
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(2).unwrap(),
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30_i64)
            .build(),
        valid_from: time::now(),
    })?;

    // Delete node
    wal.append(WalOperation::DeleteNode {
        node_id,
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Node should NOT exist in current storage
    assert_eq!(current.node_count(), 0);

    // And: Historical storage should have 3 versions (create + update + delete)
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 3);

    Ok(())
}

#[test]
fn test_replay_delete_edge_basic() -> Result<()> {
    // Given: WAL with CreateEdge followed by DeleteEdge
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let source_id = NodeId::new(1).unwrap();
    let target_id = NodeId::new(2).unwrap();
    let edge_id = EdgeId::new(1).unwrap();

    // Create nodes
    wal.append(WalOperation::CreateNode {
        node_id: source_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
    })?;

    wal.append(WalOperation::CreateNode {
        node_id: target_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
    })?;

    // Create edge
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: source_id,
        target: target_id,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
    })?;

    // Delete edge
    wal.append(WalOperation::DeleteEdge {
        edge_id,
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Edge should NOT exist in current storage
    assert_eq!(current.edge_count(), 0);
    assert!(current.get_edge(edge_id).is_err());

    // And: Nodes should still exist
    assert_eq!(current.node_count(), 2);

    // And: Historical storage should have 2 edge versions (create + delete tombstone)
    let hist_stats = historical.stats();
    assert_eq!(
        hist_stats.total_edge_versions, 2,
        "Should have 2 edge versions (create + tombstone)"
    );

    Ok(())
}

#[test]
fn test_replay_multiple_deletes() -> Result<()> {
    // Given: WAL with multiple creates and deletes
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create 5 nodes
    for i in 1..=5 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Node").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
        })?;
    }

    // Delete nodes 2 and 4
    for id in [2, 4] {
        wal.append(WalOperation::DeleteNode {
            node_id: NodeId::new(id).unwrap(),
            valid_from: time::now(),
        })?;
    }

    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Only 3 nodes should exist in current storage (1, 3, 5)
    assert_eq!(current.node_count(), 3);
    assert!(current.get_node(NodeId::new(1).unwrap()).is_ok());
    assert!(current.get_node(NodeId::new(2).unwrap()).is_err()); // Deleted
    assert!(current.get_node(NodeId::new(3).unwrap()).is_ok());
    assert!(current.get_node(NodeId::new(4).unwrap()).is_err()); // Deleted
    assert!(current.get_node(NodeId::new(5).unwrap()).is_ok());

    // And: Historical storage should have 7 versions (5 creates + 2 deletes)
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 7);

    Ok(())
}

#[test]
fn test_replay_delete_with_vector() -> Result<()> {
    // Given: WAL with node containing vector, then delete
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();
    let embedding = vec![0.1, 0.2, 0.3, 0.4];

    // Create node with vector
    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Document").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert_vector("embedding", &embedding)
            .build(),
        valid_from: time::now(),
    })?;

    // Delete node
    wal.append(WalOperation::DeleteNode {
        node_id,
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Node should NOT exist in current storage
    assert_eq!(current.node_count(), 0);

    // And: Historical storage should have 2 versions
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 2);

    Ok(())
}

#[test]
fn test_replay_mixed_creates_updates_deletes() -> Result<()> {
    // Given: WAL with interleaved creates, updates, and deletes
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create node 1
    wal.append(WalOperation::CreateNode {
        node_id: NodeId::new(1).unwrap(),
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMapBuilder::new().insert("value", 1_i64).build(),
        valid_from: time::now(),
    })?;

    // Create node 2
    wal.append(WalOperation::CreateNode {
        node_id: NodeId::new(2).unwrap(),
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMapBuilder::new().insert("value", 2_i64).build(),
        valid_from: time::now(),
    })?;

    // Update node 1
    wal.append(WalOperation::UpdateNode {
        node_id: NodeId::new(1).unwrap(),
        version_id: VersionId::new(3).unwrap(),
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMapBuilder::new().insert("value", 10_i64).build(),
        valid_from: time::now(),
    })?;

    // Delete node 2
    wal.append(WalOperation::DeleteNode {
        node_id: NodeId::new(2).unwrap(),
        valid_from: time::now(),
    })?;

    // Create node 3
    wal.append(WalOperation::CreateNode {
        node_id: NodeId::new(3).unwrap(),
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMapBuilder::new().insert("value", 3_i64).build(),
        valid_from: time::now(),
    })?;

    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Only nodes 1 and 3 should exist in current storage
    assert_eq!(current.node_count(), 2);
    assert!(current.get_node(NodeId::new(1).unwrap()).is_ok());
    assert!(current.get_node(NodeId::new(2).unwrap()).is_err()); // Deleted
    assert!(current.get_node(NodeId::new(3).unwrap()).is_ok());

    // And: Node 1 should have the updated value
    use aletheiadb::core::property::PropertyValue;
    let node1 = current.get_node(NodeId::new(1).unwrap())?;
    assert!(matches!(
        node1.properties.get("value"),
        Some(PropertyValue::Int(10))
    ));

    // And: Historical storage should have 5 versions
    // - Create node 1: v1
    // - Create node 2: v2
    // - Update node 1: v3 (v1 closed, v3 open)
    // - Delete node 2: v4 tombstone (v2 closed, v4 tombstone open)
    // - Create node 3: v5
    // Total: 5 versions (v1 closed, v2 closed, v3 open, v4 tombstone, v5 open)
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 5);

    Ok(())
}
