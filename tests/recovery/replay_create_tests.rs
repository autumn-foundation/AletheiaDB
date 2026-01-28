//! Tests for CreateNode/CreateEdge replay handlers (Issue #288)
//!
//! These tests verify that recovery correctly replays:
//! - CreateNode operations with proper ID tracking and storage
//! - CreateEdge operations with proper ID tracking and storage
//! - Property handling (including vectors)
//! - Bi-temporal interval preservation
//! - Version metadata creation
//! - Temporal index updates

use gallifreydb::{
    GLOBAL_INTERNER,
    core::{
        id::{EdgeId, NodeId},
        property::{PropertyMap, PropertyMapBuilder},
        temporal::{BiTemporalInterval, time},
    },
    storage::{
        persistence::{CheckpointConfig, PersistenceManager},
        version::TemporalVersion,
        wal::{
            WalOperation,
            concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
        },
    },
    utils::error::Result,
};
use tempfile::TempDir;

#[test]
fn test_replay_create_node_basic() -> Result<()> {
    // Given: WAL with single CreateNode operation
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(42).unwrap();
    let timestamp = time::now();

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(timestamp),
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Node exists in current storage
    assert_eq!(current.node_count(), 1);
    let node = current.get_node(node_id)?;
    assert!(node.has_label_str("Person"));
    assert_eq!(node.id, node_id);

    // And: Version exists in historical storage
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 1);

    Ok(())
}

#[test]
fn test_replay_create_node_with_properties() -> Result<()> {
    // Given: WAL with CreateNode containing properties
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();
    let properties = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30_i64)
        .insert("active", true)
        .build();

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("User").unwrap(),
        properties: properties.clone(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: Node has correct properties
    let node = current.get_node(node_id)?;
    use gallifreydb::core::property::PropertyValue;
    assert!(
        matches!(node.properties.get("name"), Some(PropertyValue::String(s)) if s.as_ref() == "Alice")
    );
    assert!(matches!(
        node.properties.get("age"),
        Some(PropertyValue::Int(30))
    ));
    assert!(matches!(
        node.properties.get("active"),
        Some(PropertyValue::Bool(true))
    ));

    Ok(())
}

#[test]
fn test_replay_create_node_with_vector() -> Result<()> {
    // Given: WAL with CreateNode containing vector property
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();
    let embedding = vec![0.1, 0.2, 0.3, 0.4];
    let properties = PropertyMapBuilder::new()
        .insert("title", "Test Document")
        .insert_vector("embedding", &embedding)
        .build();

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Document").unwrap(),
        properties: properties.clone(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: Node has correct vector property
    let node = current.get_node(node_id)?;
    use gallifreydb::core::property::PropertyValue;
    if let Some(PropertyValue::Vector(vec)) = node.properties.get("embedding") {
        assert_eq!(vec.len(), 4);
        assert_eq!(&vec[..], &embedding[..]);
    } else {
        panic!("Expected vector property");
    }

    Ok(())
}

#[test]
fn test_replay_create_edge_basic() -> Result<()> {
    // Given: WAL with CreateNode operations followed by CreateEdge
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let source_id = NodeId::new(1).unwrap();
    let target_id = NodeId::new(2).unwrap();
    let edge_id = EdgeId::new(10).unwrap();

    // Create source and target nodes
    wal.append(WalOperation::CreateNode {
        node_id: source_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    wal.append(WalOperation::CreateNode {
        node_id: target_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Create edge
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: source_id,
        target: target_id,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Edge exists in current storage
    assert_eq!(current.edge_count(), 1);
    let edge = current.get_edge(edge_id)?;
    assert!(edge.has_label_str("KNOWS"));
    assert_eq!(edge.source, source_id);
    assert_eq!(edge.target, target_id);

    // And: Edge version exists in historical storage
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_edge_versions, 1);

    Ok(())
}

#[test]
fn test_replay_create_edge_with_properties() -> Result<()> {
    // Given: WAL with CreateEdge containing properties
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let source_id = NodeId::new(1).unwrap();
    let target_id = NodeId::new(2).unwrap();
    let edge_id = EdgeId::new(1).unwrap();

    // Create nodes first
    wal.append(WalOperation::CreateNode {
        node_id: source_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    wal.append(WalOperation::CreateNode {
        node_id: target_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Create edge with properties
    let edge_properties = PropertyMapBuilder::new()
        .insert("since", 2020_i64)
        .insert("strength", 0.8)
        .build();

    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: source_id,
        target: target_id,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: edge_properties.clone(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: Edge has correct properties
    let edge = current.get_edge(edge_id)?;
    use gallifreydb::core::property::PropertyValue;
    assert!(matches!(
        edge.properties.get("since"),
        Some(PropertyValue::Int(2020))
    ));
    assert!(
        matches!(edge.properties.get("strength"), Some(PropertyValue::Float(s)) if (*s - 0.8).abs() < f64::EPSILON)
    );

    Ok(())
}

#[test]
fn test_replay_multiple_creates() -> Result<()> {
    // Given: WAL with multiple CreateNode and CreateEdge operations
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create 10 nodes
    for i in 1..=10 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern(format!("Node{}", i)).unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    // Create 5 edges
    for i in 1..=5 {
        wal.append(WalOperation::CreateEdge {
            edge_id: EdgeId::new(i).unwrap(),
            source: NodeId::new(i).unwrap(),
            target: NodeId::new(i + 1).unwrap(),
            label: GLOBAL_INTERNER.intern("LINKS_TO").unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: All nodes and edges recovered
    assert_eq!(current.node_count(), 10);
    assert_eq!(current.edge_count(), 5);

    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 10);
    assert_eq!(hist_stats.total_edge_versions, 5);

    Ok(())
}

#[test]
fn test_replay_create_node_tracks_max_id() -> Result<()> {
    // Given: WAL with nodes having non-sequential IDs
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create nodes with IDs: 5, 100, 42 (not sequential)
    for id in [5, 100, 42] {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(id).unwrap(),
            label: GLOBAL_INTERNER.intern("Test").unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: All nodes recovered
    assert_eq!(current.node_count(), 3);

    // And: Node ID generator should be initialized to max_id + 1 (100 + 1 = 101)
    let next_node_id = current.create_node("NewNode", PropertyMap::new())?;
    assert_eq!(
        next_node_id.as_u64(),
        101,
        "Node ID generator should start from max_id + 1"
    );

    Ok(())
}

#[test]
fn test_replay_preserves_temporal_interval() -> Result<()> {
    // Given: WAL with CreateNode using specific temporal interval
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();
    let timestamp = time::now();
    let temporal = BiTemporalInterval::current(timestamp);

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Test").unwrap(),
        properties: PropertyMap::new(),
        temporal,
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (_current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Historical version has correct temporal interval
    let all_versions = historical.get_all_node_versions();
    let node_versions = all_versions
        .get(&node_id)
        .expect("Node should exist in historical storage");

    if let [version] = node_versions.as_slice() {
        assert_eq!(
            version.temporal(),
            &temporal,
            "Temporal interval should match"
        );
    } else {
        panic!(
            "Expected exactly one version for node {}, but found {}",
            node_id,
            node_versions.len()
        );
    }

    Ok(())
}
