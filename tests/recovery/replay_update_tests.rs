//! Tests for UpdateNode/UpdateEdge replay handlers (Issue #289)
//!
//! These tests verify that recovery correctly replays:
//! - UpdateNode operations with property and label changes
//! - UpdateEdge operations with property and label changes
//! - Multiple updates to the same entity (version chain)
//! - Closing previous version's transaction_time (bi-temporal semantics)
//! - Version metadata creation for updates

use aletheiadb::{
    GLOBAL_INTERNER,
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
    core::error::Result,
};
use tempfile::TempDir;

#[test]
fn test_replay_update_node_basic() -> Result<()> {
    // Given: WAL with CreateNode followed by UpdateNode
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

    // Update node with new properties
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(2).unwrap(),
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30_i64)
            .build(),
        valid_from: timestamp2,
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Current storage has updated node
    assert_eq!(current.node_count(), 1);
    let node = current.get_node(node_id)?;
    assert!(node.has_label_str("Person"));
    use aletheiadb::core::property::PropertyValue;
    assert!(
        matches!(node.properties.get("name"), Some(PropertyValue::String(s)) if s.as_ref() == "Alice")
    );
    assert!(matches!(
        node.properties.get("age"),
        Some(PropertyValue::Int(30))
    ));

    // And: Historical storage has 2 versions
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 2);

    Ok(())
}

#[test]
fn test_replay_update_node_label_change() -> Result<()> {
    // Given: WAL with CreateNode followed by UpdateNode with label change
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();

    // Create node with "Person" label
    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
    })?;

    // Update node to "User" label
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(2).unwrap(),
        label: GLOBAL_INTERNER.intern("User").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: Current storage has updated label
    let node = current.get_node(node_id)?;
    assert!(node.has_label_str("User"));

    Ok(())
}

#[test]
fn test_replay_update_node_with_vector() -> Result<()> {
    // Given: WAL with UpdateNode containing vector property
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();
    let embedding_v1 = vec![0.1, 0.2, 0.3, 0.4];
    let embedding_v2 = vec![0.5, 0.6, 0.7, 0.8];

    // Create node with initial embedding
    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Document").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert_vector("embedding", &embedding_v1)
            .build(),
        valid_from: time::now(),
    })?;

    // Update node with new embedding
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(2).unwrap(),
        label: GLOBAL_INTERNER.intern("Document").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert_vector("embedding", &embedding_v2)
            .build(),
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: Node has updated vector
    let node = current.get_node(node_id)?;
    use aletheiadb::core::property::PropertyValue;
    if let Some(PropertyValue::Vector(vec)) = node.properties.get("embedding") {
        assert_eq!(vec.len(), 4);
        assert_eq!(&vec[..], &embedding_v2[..]);
    } else {
        panic!("Expected vector property");
    }

    Ok(())
}

#[test]
fn test_replay_multiple_updates_same_node() -> Result<()> {
    // Given: WAL with node updated multiple times
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();

    // Create node
    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Counter").unwrap(),
        properties: PropertyMapBuilder::new().insert("count", 0_i64).build(),
        valid_from: time::now(),
    })?;

    // Update 5 times
    for i in 1..=5 {
        wal.append(WalOperation::UpdateNode {
            node_id,
            version_id: VersionId::new((i + 1) as u64).unwrap(),
            label: GLOBAL_INTERNER.intern("Counter").unwrap(),
            properties: PropertyMapBuilder::new().insert("count", i).build(),
            valid_from: time::now(),
        })?;
    }
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Current storage has final value
    let node = current.get_node(node_id)?;
    use aletheiadb::core::property::PropertyValue;
    assert!(matches!(
        node.properties.get("count"),
        Some(PropertyValue::Int(5))
    ));

    // And: Historical storage has 6 versions (1 create + 5 updates)
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 6);

    Ok(())
}

#[test]
fn test_replay_update_edge_basic() -> Result<()> {
    // Given: WAL with CreateEdge followed by UpdateEdge
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
        properties: PropertyMapBuilder::new().insert("since", 2020_i64).build(),
        valid_from: time::now(),
    })?;

    // Update edge
    wal.append(WalOperation::UpdateEdge {
        edge_id,
        version_id: VersionId::new(4).unwrap(),
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("since", 2020_i64)
            .insert("strength", 0.8)
            .build(),
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Current storage has updated edge
    let edge = current.get_edge(edge_id)?;
    use aletheiadb::core::property::PropertyValue;
    assert!(matches!(
        edge.properties.get("since"),
        Some(PropertyValue::Int(2020))
    ));
    assert!(
        matches!(edge.properties.get("strength"), Some(PropertyValue::Float(s)) if (*s - 0.8).abs() < f64::EPSILON)
    );

    // And: Historical storage has 2 edge versions
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_edge_versions, 2);

    Ok(())
}

#[test]
fn test_replay_update_edge_label_change() -> Result<()> {
    // Given: WAL with CreateEdge followed by UpdateEdge with label change
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

    // Create edge with "KNOWS" label
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: source_id,
        target: target_id,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
    })?;

    // Update edge to "FRIENDS_WITH" label
    wal.append(WalOperation::UpdateEdge {
        edge_id,
        version_id: VersionId::new(4).unwrap(),
        label: GLOBAL_INTERNER.intern("FRIENDS_WITH").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: Current storage has updated label
    let edge = current.get_edge(edge_id)?;
    assert!(edge.has_label_str("FRIENDS_WITH"));

    Ok(())
}

#[test]
fn test_replay_mixed_creates_and_updates() -> Result<()> {
    // Given: WAL with interleaved creates and updates
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create 3 nodes
    for i in 1..=3 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Node").unwrap(),
            properties: PropertyMapBuilder::new().insert("value", i as i64).build(),
            valid_from: time::now(),
        })?;
    }

    // Update nodes 1 and 2
    wal.append(WalOperation::UpdateNode {
        node_id: NodeId::new(1).unwrap(),
        version_id: VersionId::new(4).unwrap(),
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMapBuilder::new().insert("value", 10_i64).build(),
        valid_from: time::now(),
    })?;

    wal.append(WalOperation::UpdateNode {
        node_id: NodeId::new(2).unwrap(),
        version_id: VersionId::new(5).unwrap(),
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMapBuilder::new().insert("value", 20_i64).build(),
        valid_from: time::now(),
    })?;

    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Current storage has 3 nodes with correct values
    assert_eq!(current.node_count(), 3);

    use aletheiadb::core::property::PropertyValue;
    let node1 = current.get_node(NodeId::new(1).unwrap())?;
    assert!(matches!(
        node1.properties.get("value"),
        Some(PropertyValue::Int(10))
    ));

    let node2 = current.get_node(NodeId::new(2).unwrap())?;
    assert!(matches!(
        node2.properties.get("value"),
        Some(PropertyValue::Int(20))
    ));

    let node3 = current.get_node(NodeId::new(3).unwrap())?;
    assert!(matches!(
        node3.properties.get("value"),
        Some(PropertyValue::Int(3))
    ));

    // And: Historical storage has 5 versions (3 creates + 2 updates)
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 5);

    Ok(())
}
