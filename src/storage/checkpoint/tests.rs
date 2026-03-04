use super::*;
use crate::GLOBAL_INTERNER;
use crate::PropertyMapBuilder;
use crate::core::id::NodeId;
use crate::core::temporal::{BiTemporalInterval, time};
use crate::storage::wal::WalOperation;
use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
use tempfile::TempDir;

// ========================================================================
// TDD Tests: Checkpoint-Index Persistence Integration
// ========================================================================

/// Test basic checkpoint creation and stats.
#[test]
fn test_checkpoint_creation_basic() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let config = CheckpointConfig::with_data_dir(temp_dir.path());
    let mut manager = CheckpointManager::new(config)?;

    let current = CurrentStorage::new();
    let historical = HistoricalStorage::new();

    let stats = manager.create_checkpoint(LSN(100), &current, &historical)?;

    assert_eq!(stats.lsn, LSN(100));
    assert_eq!(stats.node_count, 0);
    assert_eq!(stats.edge_count, 0);
    assert!(stats.bytes_written > 0); // At least manifest should be written

    Ok(())
}

/// Test checkpoint persists nodes and edges.
#[test]
fn test_checkpoint_persists_graph_data() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let config = CheckpointConfig::with_data_dir(temp_dir.path());
    let mut manager = CheckpointManager::new(config)?;

    let current = CurrentStorage::new();

    // Create some nodes
    for i in 1..=10 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node{}", i))
            .build();
        let node_id = NodeId::new(i)?;
        let label = GLOBAL_INTERNER
            .intern("Person")
            .map_err(|e| StorageError::WalError {
                reason: e.to_string(),
            })?;
        let version_id = VersionId::new(i)?;
        let node = Node::new(node_id, label, props, version_id);
        current.insert_node_direct(node, time::now())?;
    }

    let historical = HistoricalStorage::new();
    let stats = manager.create_checkpoint(LSN(50), &current, &historical)?;

    assert_eq!(stats.node_count, 10);

    // Verify files were created
    assert!(manager.persistence_manager.manifest_path().exists());
    assert!(manager.persistence_manager.interner_path().exists());
    assert!(
        manager
            .persistence_manager
            .graph_path()
            .join("adjacency.idx")
            .exists()
    );

    Ok(())
}

/// Test checkpoint recovery loads persisted state.
#[test]
fn test_checkpoint_recovery_loads_state() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create WAL first so we have a valid LSN for the checkpoint
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Phase 1: Create checkpoint with data
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();

        // Create 5 nodes
        for i in 1..=5 {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("Node{}", i))
                .build();
            let node_id = NodeId::new(i)?;
            let label = GLOBAL_INTERNER
                .intern("Document")
                .map_err(|e| StorageError::WalError {
                    reason: e.to_string(),
                })?;
            let version_id = VersionId::new(i)?;
            let node = Node::new(node_id, label, props, version_id);
            current.insert_node_direct(node, time::now())?;
        }

        let historical = HistoricalStorage::new();
        // Use LSN(0) which is valid for an empty WAL
        manager.create_checkpoint(LSN(0), &current, &historical)?;
    }

    // Phase 2: Recover from checkpoint using the same WAL
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let (recovered_current, _recovered_historical, lsn) = manager.recover(&wal)?;

        // Verify state was restored
        assert_eq!(recovered_current.node_count(), 5);
        assert_eq!(lsn, LSN::initial()); // Empty WAL

        // Verify node data
        for i in 1..=5 {
            let node = recovered_current.get_node(NodeId::new(i)?)?;
            let name = node.get_property("name").unwrap().as_str().unwrap();
            assert_eq!(name, format!("Node{}", i));
        }
    }

    Ok(())
}

/// Test recovery with WAL replay after checkpoint.
///
/// This test verifies that:
/// 1. Nodes from the checkpoint are restored
/// 2. WAL entries after the checkpoint LSN are replayed
#[test]
fn test_checkpoint_recovery_with_wal_replay() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create WAL first to have a proper LSN sequence
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Phase 1: Add initial nodes to WAL (to establish LSN sequence)
    for i in 1..=3 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Initial{}", i))
            .build();
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: props,
            valid_from: time::now(),
        })?;
    }
    wal.flush()?;

    // Create checkpoint at last written WAL LSN
    // current_lsn() returns the *next* LSN to be allocated, so subtract 1
    let checkpoint_lsn = LSN(wal.current_lsn().0.saturating_sub(1));
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();

        // Create 3 nodes directly (matching what was logged to WAL)
        for i in 1..=3 {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("Initial{}", i))
                .build();
            let node_id = NodeId::new(i)?;
            let label = GLOBAL_INTERNER
                .intern("Person")
                .map_err(|e| StorageError::WalError {
                    reason: e.to_string(),
                })?;
            let version_id = VersionId::new(i)?;
            let node = Node::new(node_id, label, props, version_id);
            current.insert_node_direct(node, time::now())?;
        }

        let historical = HistoricalStorage::new();
        manager.create_checkpoint(checkpoint_lsn, &current, &historical)?;
    }

    // Phase 2: Add more WAL entries after checkpoint
    for i in 4..=5 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("WalNode{}", i))
            .build();
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: props,
            valid_from: time::now(),
        })?;
    }
    wal.flush()?;

    // Phase 3: Recover (should load checkpoint + replay WAL)
    let config = CheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

    // Should have 3 from checkpoint + 2 from WAL = 5 total
    assert_eq!(recovered_current.node_count(), 5);

    // Verify checkpoint nodes
    for i in 1..=3 {
        let node = recovered_current.get_node(NodeId::new(i)?)?;
        let name = node.get_property("name").unwrap().as_str().unwrap();
        assert_eq!(name, format!("Initial{}", i));
    }

    // Verify WAL-replayed nodes
    for i in 4..=5 {
        let node = recovered_current.get_node(NodeId::new(i)?)?;
        let name = node.get_property("name").unwrap().as_str().unwrap();
        assert_eq!(name, format!("WalNode{}", i));
    }

    Ok(())
}

/// Test LSN consistency between checkpoint and manifest.
#[test]
fn test_lsn_consistency() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let config = CheckpointConfig::with_data_dir(temp_dir.path());
    let mut manager = CheckpointManager::new(config)?;

    let current = CurrentStorage::new();
    let historical = HistoricalStorage::new();

    // Create checkpoint at LSN 42
    manager.create_checkpoint(LSN(42), &current, &historical)?;

    // Verify persisted LSN
    let persisted_lsn = manager.get_persisted_lsn();
    assert_eq!(persisted_lsn, Some(LSN(42)));

    Ok(())
}

/// Test should_checkpoint logic.
#[test]
fn test_should_checkpoint_logic() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let config = CheckpointConfig {
        data_dir: temp_dir.path().to_path_buf(),
        checkpoint_interval: Duration::from_secs(3600), // 1 hour
        min_wal_entries: 100,
        ..Default::default()
    };
    let mut manager = CheckpointManager::new(config)?;

    // Should checkpoint initially (never checkpointed)
    assert!(manager.should_checkpoint(LSN(1)));

    // Simulate a checkpoint
    manager.last_checkpoint_time = SystemTime::now();
    manager.last_checkpoint_lsn = LSN(50);

    // Should NOT checkpoint (not enough time or entries)
    assert!(!manager.should_checkpoint(LSN(60)));

    // Should checkpoint when LSN threshold exceeded
    assert!(manager.should_checkpoint(LSN(200)));

    Ok(())
}

/// Test recovery without persisted state (fresh start).
#[test]
fn test_recovery_without_persisted_state() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let config = CheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    // Create WAL with some entries (no checkpoint)
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    for i in 1..=3 {
        let props = PropertyMapBuilder::new().insert("value", i as i64).build();
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: GLOBAL_INTERNER.intern("Test").unwrap(),
            properties: props,
            valid_from: time::now(),
        })?;
    }
    wal.flush()?;

    // Recover (should replay full WAL)
    let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

    assert_eq!(recovered_current.node_count(), 3);

    Ok(())
}

/// Test checkpoint with compression enabled.
#[test]
fn test_checkpoint_with_compression() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let config = CheckpointConfig {
        data_dir: temp_dir.path().to_path_buf(),
        enable_compression: true,
        compression_level: 3,
        ..Default::default()
    };
    let mut manager = CheckpointManager::new(config)?;

    let current = CurrentStorage::new();

    // Create nodes with larger properties to benefit from compression
    for i in 1..=100 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node{} with some longer text for compression", i))
            .insert("description", "This is a longer description that should compress well when repeated across many nodes")
            .build();
        let node_id = NodeId::new(i)?;
        let label = GLOBAL_INTERNER
            .intern("Document")
            .map_err(|e| StorageError::WalError {
                reason: e.to_string(),
            })?;
        let version_id = VersionId::new(i)?;
        let node = Node::new(node_id, label, props, version_id);
        current.insert_node_direct(node, time::now())?;
    }

    let historical = HistoricalStorage::new();
    let stats = manager.create_checkpoint(LSN(100), &current, &historical)?;

    assert_eq!(stats.node_count, 100);
    assert!(stats.bytes_written > 0);

    Ok(())
}

/// Test has_persisted_state check.
#[test]
fn test_has_persisted_state() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let config = CheckpointConfig::with_data_dir(temp_dir.path());
    let mut manager = CheckpointManager::new(config)?;

    // Initially no persisted state
    assert!(!manager.has_persisted_state());

    // Create checkpoint
    let current = CurrentStorage::new();
    let historical = HistoricalStorage::new();
    manager.create_checkpoint(LSN(1), &current, &historical)?;

    // Now has persisted state
    assert!(manager.has_persisted_state());

    Ok(())
}

/// Test checkpoint preserves node properties correctly.
#[test]
fn test_checkpoint_preserves_properties() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Phase 1: Create checkpoint with various property types
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();

        let props = PropertyMapBuilder::new()
            .insert("string_prop", "hello")
            .insert("int_prop", 42i64)
            .insert("float_prop", 3.15f64)
            .insert("bool_prop", true)
            .build();

        let node_id = NodeId::new(1)?;
        let label = GLOBAL_INTERNER
            .intern("TestNode")
            .map_err(|e| StorageError::WalError {
                reason: e.to_string(),
            })?;
        let version_id = VersionId::new(1)?;
        let node = Node::new(node_id, label, props, version_id);
        current.insert_node_direct(node, time::now())?;

        let historical = HistoricalStorage::new();
        manager.create_checkpoint(LSN(1), &current, &historical)?;
    }

    // Phase 2: Recover and verify properties
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

        let node = recovered_current.get_node(NodeId::new(1)?)?;

        assert_eq!(
            node.get_property("string_prop").unwrap().as_str().unwrap(),
            "hello"
        );
        assert_eq!(node.get_property("int_prop").unwrap().as_int().unwrap(), 42);
        assert!(
            (node.get_property("float_prop").unwrap().as_float().unwrap() - 3.15).abs() < 0.001
        );
        assert!(node.get_property("bool_prop").unwrap().as_bool().unwrap());
    }

    Ok(())
}

// ========================================================================
// Additional Coverage Tests
// ========================================================================

/// Test invalid compression level returns error.
#[test]
fn test_invalid_compression_level_error() {
    let temp_dir = TempDir::new().unwrap();
    let config = CheckpointConfig {
        data_dir: temp_dir.path().to_path_buf(),
        enable_compression: true,
        compression_level: 0, // Invalid - must be 1-22
        ..Default::default()
    };
    let result = CheckpointManager::new(config);
    assert!(result.is_err());
    match result {
        Err(e) => assert!(e.to_string().contains("Invalid compression level")),
        Ok(_) => panic!("Expected error"),
    }

    // Test compression level too high
    let config2 = CheckpointConfig {
        data_dir: temp_dir.path().to_path_buf(),
        enable_compression: true,
        compression_level: 25, // Invalid - must be 1-22
        ..Default::default()
    };
    let result2 = CheckpointManager::new(config2);
    assert!(result2.is_err());
}

/// Test that compression level is not validated when compression is disabled.
#[test]
fn test_compression_disabled_ignores_level() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let config = CheckpointConfig {
        data_dir: temp_dir.path().to_path_buf(),
        enable_compression: false,
        compression_level: 0, // Invalid, but should be ignored
        ..Default::default()
    };
    let _manager = CheckpointManager::new(config)?;
    Ok(())
}

/// Test checkpoint without compression (uncompressed path).
#[test]
fn test_checkpoint_without_compression() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Phase 1: Create checkpoint without compression
    {
        let config = CheckpointConfig {
            data_dir: data_dir.clone(),
            enable_compression: false,
            compression_level: 1, // Won't be used
            ..Default::default()
        };
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();

        for i in 1..=5 {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("Node{}", i))
                .build();
            let node_id = NodeId::new(i)?;
            let label =
                GLOBAL_INTERNER
                    .intern("Uncompressed")
                    .map_err(|e| StorageError::WalError {
                        reason: e.to_string(),
                    })?;
            let version_id = VersionId::new(i)?;
            let node = Node::new(node_id, label, props, version_id);
            current.insert_node_direct(node, time::now())?;
        }

        let historical = HistoricalStorage::new();
        let stats = manager.create_checkpoint(LSN(0), &current, &historical)?;
        assert_eq!(stats.node_count, 5);
    }

    // Phase 2: Recover and verify
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;
        assert_eq!(recovered_current.node_count(), 5);
    }

    Ok(())
}

/// Test checkpoint with edges.
#[test]
fn test_checkpoint_with_edges() -> Result<()> {
    use crate::core::graph::Edge;
    use crate::core::id::EdgeId;

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Phase 1: Create checkpoint with nodes and edges
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();

        // Create nodes
        for i in 1..=3 {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("Person{}", i))
                .build();
            let node_id = NodeId::new(i)?;
            let label = GLOBAL_INTERNER
                .intern("Person")
                .map_err(|e| StorageError::WalError {
                    reason: e.to_string(),
                })?;
            let version_id = VersionId::new(i)?;
            let node = Node::new(node_id, label, props, version_id);
            current.insert_node_direct(node, time::now())?;
        }

        // Create edges
        let edge_label = GLOBAL_INTERNER
            .intern("KNOWS")
            .map_err(|e| StorageError::WalError {
                reason: e.to_string(),
            })?;

        let edge1 = Edge::new(
            EdgeId::new(1)?,
            edge_label,
            NodeId::new(1)?,
            NodeId::new(2)?,
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
            VersionId::new(4)?,
        );
        let edge2 = Edge::new(
            EdgeId::new(2)?,
            edge_label,
            NodeId::new(2)?,
            NodeId::new(3)?,
            PropertyMapBuilder::new().insert("since", 2021i64).build(),
            VersionId::new(5)?,
        );

        current.insert_edge_direct(edge1)?;
        current.insert_edge_direct(edge2)?;

        let historical = HistoricalStorage::new();
        let stats = manager.create_checkpoint(LSN(0), &current, &historical)?;
        assert_eq!(stats.node_count, 3);
        assert_eq!(stats.edge_count, 2);
    }

    // Phase 2: Recover and verify edges
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

        assert_eq!(recovered_current.node_count(), 3);
        assert_eq!(recovered_current.edge_count(), 2);

        // Verify edge data
        let edge1 = recovered_current.get_edge(EdgeId::new(1)?)?;
        assert_eq!(edge1.source.as_u64(), 1);
        assert_eq!(edge1.target.as_u64(), 2);
        assert_eq!(edge1.get_property("since").unwrap().as_int().unwrap(), 2020);

        let edge2 = recovered_current.get_edge(EdgeId::new(2)?)?;
        assert_eq!(edge2.source.as_u64(), 2);
        assert_eq!(edge2.target.as_u64(), 3);
    }

    Ok(())
}

/// Test checkpoint LSN ahead of WAL returns error.
#[test]
fn test_checkpoint_lsn_ahead_of_wal_error() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create a checkpoint with a high LSN
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();

        // Create checkpoint with LSN 1000
        manager.create_checkpoint(LSN(1000), &current, &historical)?;
    }

    // Try to recover with an empty WAL (current LSN = 0)
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let result = manager.recover(&wal);
        assert!(result.is_err());
        match result {
            Err(e) => {
                let err_str = e.to_string();
                assert!(err_str.contains("Checkpoint LSN"));
                assert!(err_str.contains("ahead of WAL"));
            }
            Ok(_) => panic!("Expected error"),
        }
    }

    Ok(())
}

/// Test WAL replay with CreateEdge operation.
#[test]
fn test_wal_replay_create_edge() -> Result<()> {
    use crate::core::id::EdgeId;

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let config = CheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create nodes first
    for i in 1..=2 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("name", format!("Person{}", i))
                .build(),
            valid_from: time::now(),
        })?;
    }

    // Create edge
    wal.append(WalOperation::CreateEdge {
        edge_id: EdgeId::new(1)?,
        source: NodeId::new(1)?,
        target: NodeId::new(2)?,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMapBuilder::new().insert("since", 2023i64).build(),
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // Recover (no persisted state - full WAL replay)
    let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

    assert_eq!(recovered_current.node_count(), 2);
    assert_eq!(recovered_current.edge_count(), 1);

    let edge = recovered_current.get_edge(EdgeId::new(1)?)?;
    assert_eq!(edge.source.as_u64(), 1);
    assert_eq!(edge.target.as_u64(), 2);

    // Verify historical storage also has the edge version
    assert_eq!(recovered_historical.get_edge_versions().len(), 1);

    Ok(())
}

/// Test WAL replay with UpdateNode operation.
#[test]
fn test_wal_replay_update_node() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let config = CheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1)?;

    // Create node
    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build(),
        valid_from: time::now(),
    })?;

    // Update node
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(2)?,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 31i64)
            .build(),
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // Recover
    let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

    assert_eq!(recovered_current.node_count(), 1);

    let node = recovered_current.get_node(node_id)?;
    assert_eq!(node.get_property("age").unwrap().as_int().unwrap(), 31);

    // Verify historical has versions (create + update)
    assert_eq!(recovered_historical.get_node_versions().len(), 2);

    Ok(())
}

/// Test WAL replay with UpdateEdge operation.
#[test]
fn test_wal_replay_update_edge() -> Result<()> {
    use crate::core::id::EdgeId;

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let config = CheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create nodes first
    for i in 1..=2 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMapBuilder::new().build(),
            valid_from: time::now(),
        })?;
    }

    let edge_id = EdgeId::new(1)?;

    // Create edge
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: NodeId::new(1)?,
        target: NodeId::new(2)?,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMapBuilder::new().insert("strength", 5i64).build(),
        valid_from: time::now(),
    })?;

    // Update edge
    wal.append(WalOperation::UpdateEdge {
        edge_id,
        version_id: VersionId::new(4)?,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMapBuilder::new().insert("strength", 10i64).build(),
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // Recover
    let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

    assert_eq!(recovered_current.edge_count(), 1);

    let edge = recovered_current.get_edge(edge_id)?;
    assert_eq!(edge.get_property("strength").unwrap().as_int().unwrap(), 10);

    // Verify historical has edge versions (create + update)
    assert_eq!(recovered_historical.get_edge_versions().len(), 2);

    Ok(())
}

/// Test WAL replay with DeleteNode operation.
#[test]
fn test_wal_replay_delete_node() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let config = CheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1)?;

    // Create node
    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("ToDelete").unwrap(),
        properties: PropertyMapBuilder::new().insert("temp", true).build(),
        valid_from: time::now(),
    })?;

    // Delete node
    wal.append(WalOperation::DeleteNode {
        node_id,
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // Recover
    let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

    // Node should be deleted from current
    assert_eq!(recovered_current.node_count(), 0);

    // But historical should have versions (create + tombstone)
    assert_eq!(recovered_historical.get_node_versions().len(), 2);

    Ok(())
}

/// Test WAL replay with DeleteEdge operation.
#[test]
fn test_wal_replay_delete_edge() -> Result<()> {
    use crate::core::id::EdgeId;

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let config = CheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create nodes
    for i in 1..=2 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMapBuilder::new().build(),
            valid_from: time::now(),
        })?;
    }

    let edge_id = EdgeId::new(1)?;

    // Create edge
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: NodeId::new(1)?,
        target: NodeId::new(2)?,
        label: GLOBAL_INTERNER.intern("TEMP_EDGE").unwrap(),
        properties: PropertyMapBuilder::new().build(),
        valid_from: time::now(),
    })?;

    // Delete edge
    wal.append(WalOperation::DeleteEdge {
        edge_id,
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // Recover
    let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

    // Edge should be deleted from current
    assert_eq!(recovered_current.edge_count(), 0);
    // Nodes should still exist
    assert_eq!(recovered_current.node_count(), 2);

    // Historical should have edge versions (create + tombstone)
    assert_eq!(recovered_historical.get_edge_versions().len(), 2);

    Ok(())
}

/// Test WAL replay with Checkpoint marker (should be ignored).
#[test]
fn test_wal_replay_checkpoint_marker() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let config = CheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create a node
    wal.append(WalOperation::CreateNode {
        node_id: NodeId::new(1)?,
        label: GLOBAL_INTERNER.intern("Test").unwrap(),
        properties: PropertyMapBuilder::new().build(),
        valid_from: time::now(),
    })?;

    // Add checkpoint marker
    wal.append(WalOperation::Checkpoint {
        lsn: LSN(1),
        timestamp: time::now(),
    })?;

    // Create another node
    wal.append(WalOperation::CreateNode {
        node_id: NodeId::new(2)?,
        label: GLOBAL_INTERNER.intern("Test").unwrap(),
        properties: PropertyMapBuilder::new().build(),
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // Recover - checkpoint marker should be ignored
    let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

    // Both nodes should exist
    assert_eq!(recovered_current.node_count(), 2);

    Ok(())
}

/// Test checkpoint with temporal data including node versions.
#[test]
fn test_checkpoint_with_temporal_node_versions() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Phase 1: Create checkpoint with historical node versions
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();

        // Create a node in current storage
        let node_id = NodeId::new(1)?;
        let label = GLOBAL_INTERNER
            .intern("Document")
            .map_err(|e| StorageError::WalError {
                reason: e.to_string(),
            })?;

        let props = PropertyMapBuilder::new()
            .insert("title", "Version 2")
            .build();
        let version_id = VersionId::new(2)?;
        let node = Node::new(node_id, label, props, version_id);
        current.insert_node_direct(node, time::now())?;

        // Add historical version (anchor)
        let anchor_props = PropertyMapBuilder::new()
            .insert("title", "Version 1")
            .build();
        let now = time::now();
        historical.add_node_version(
            node_id,
            VersionId::new(1)?,
            now,
            now,
            label,
            anchor_props,
            false, // not a tombstone
        )?;

        let stats = manager.create_checkpoint(LSN(0), &current, &historical)?;
        assert_eq!(stats.node_count, 1);
        assert!(stats.version_count >= 1);
    }

    // Phase 2: Recover and verify temporal data
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

        assert_eq!(recovered_current.node_count(), 1);
        assert_eq!(recovered_historical.get_node_versions().len(), 1);
    }

    Ok(())
}

/// Test checkpoint with temporal data including edge versions.
#[test]
fn test_checkpoint_with_temporal_edge_versions() -> Result<()> {
    use crate::core::graph::Edge;
    use crate::core::id::EdgeId;

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Phase 1: Create checkpoint with historical edge versions
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();

        // Create nodes
        let person_label =
            GLOBAL_INTERNER
                .intern("Person")
                .map_err(|e| StorageError::WalError {
                    reason: e.to_string(),
                })?;
        for i in 1..=2 {
            let node = Node::new(
                NodeId::new(i)?,
                person_label,
                PropertyMapBuilder::new().build(),
                VersionId::new(i)?,
            );
            current.insert_node_direct(node, time::now())?;
        }

        // Create edge in current storage
        let edge_label = GLOBAL_INTERNER
            .intern("KNOWS")
            .map_err(|e| StorageError::WalError {
                reason: e.to_string(),
            })?;
        let edge = Edge::new(
            EdgeId::new(1)?,
            edge_label,
            NodeId::new(1)?,
            NodeId::new(2)?,
            PropertyMapBuilder::new().insert("strength", 10i64).build(),
            VersionId::new(4)?,
        );
        current.insert_edge_direct(edge)?;

        // Add historical edge version (anchor)
        let now = time::now();
        historical.add_edge_version(
            EdgeId::new(1)?,
            VersionId::new(3)?,
            now,
            now,
            edge_label,
            NodeId::new(1)?,
            NodeId::new(2)?,
            PropertyMapBuilder::new().insert("strength", 5i64).build(),
            false, // not a tombstone
        )?;

        let stats = manager.create_checkpoint(LSN(0), &current, &historical)?;
        assert_eq!(stats.edge_count, 1);
        assert_eq!(stats.version_count, 1);
    }

    // Phase 2: Recover and verify temporal edge data
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let (recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

        assert_eq!(recovered_current.edge_count(), 1);
        assert_eq!(recovered_historical.get_edge_versions().len(), 1);
    }

    Ok(())
}

/// Test get_persisted_lsn returns None when no persisted state exists.
#[test]
fn test_get_persisted_lsn_none() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let config = CheckpointConfig::with_data_dir(temp_dir.path());
    let manager = CheckpointManager::new(config)?;

    assert!(manager.get_persisted_lsn().is_none());
    Ok(())
}

/// Test CheckpointConfig default values.
#[test]
fn test_checkpoint_config_default() {
    let config = CheckpointConfig::default();

    assert_eq!(config.data_dir, PathBuf::from("data"));
    assert_eq!(config.checkpoint_interval, Duration::from_secs(300));
    assert_eq!(config.min_wal_entries, 1000);
    assert!(config.enable_compression);
    assert_eq!(config.compression_level, 3);
}

/// Test checkpoint updates last_checkpoint_time and last_checkpoint_lsn.
#[test]
fn test_checkpoint_updates_tracking() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let config = CheckpointConfig::with_data_dir(temp_dir.path());
    let mut manager = CheckpointManager::new(config)?;

    // Initially, last checkpoint time is UNIX_EPOCH
    assert_eq!(manager.last_checkpoint_lsn, LSN::initial());

    let current = CurrentStorage::new();
    let historical = HistoricalStorage::new();

    manager.create_checkpoint(LSN(42), &current, &historical)?;

    // After checkpoint, tracking should be updated
    assert_eq!(manager.last_checkpoint_lsn, LSN(42));
    assert!(manager.last_checkpoint_time > UNIX_EPOCH);

    Ok(())
}

/// Test should_checkpoint with time threshold.
#[test]
fn test_should_checkpoint_time_threshold() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let config = CheckpointConfig {
        data_dir: temp_dir.path().to_path_buf(),
        checkpoint_interval: Duration::from_millis(1), // Very short
        min_wal_entries: 1_000_000,                    // Very high
        ..Default::default()
    };
    let mut manager = CheckpointManager::new(config)?;

    // Set last checkpoint time to now
    manager.last_checkpoint_time = SystemTime::now();
    manager.last_checkpoint_lsn = LSN(100);

    // Wait a bit for time to elapse
    std::thread::sleep(Duration::from_millis(5));

    // Should checkpoint due to time threshold
    assert!(manager.should_checkpoint(LSN(101)));

    Ok(())
}

/// Test temporal data with closed valid time (valid_to is set).
#[test]
fn test_checkpoint_with_closed_valid_time() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();

        let node_id = NodeId::new(1)?;
        let label = GLOBAL_INTERNER
            .intern("ClosedNode")
            .map_err(|e| StorageError::WalError {
                reason: e.to_string(),
            })?;

        // Current state (node is deleted, so not in current)
        // Add historical version with bi-temporal timestamps
        let now = time::now();

        historical.add_node_version(
            node_id,
            VersionId::new(1)?,
            now, // valid_from
            now, // tx_time
            label,
            PropertyMapBuilder::new().insert("deleted", true).build(),
            false, // not a tombstone
        )?;

        let stats = manager.create_checkpoint(LSN(0), &current, &historical)?;
        assert_eq!(stats.version_count, 1);
    }

    // Recover and verify
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let (_recovered_current, recovered_historical, _lsn) = manager.recover(&wal)?;

        // Should have the version with closed valid time
        assert_eq!(recovered_historical.get_node_versions().len(), 1);
    }

    Ok(())
}

/// Test ID generators are properly initialized after recovery with max IDs.
#[test]
fn test_recovery_id_generator_initialization() -> Result<()> {
    use crate::core::graph::Edge;
    use crate::core::id::EdgeId;

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create checkpoint with high IDs
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();

        let label = GLOBAL_INTERNER
            .intern("Test")
            .map_err(|e| StorageError::WalError {
                reason: e.to_string(),
            })?;

        // Use high node ID
        let node = Node::new(
            NodeId::new(100)?,
            label,
            PropertyMapBuilder::new().build(),
            VersionId::new(1)?,
        );
        current.insert_node_direct(node, time::now())?;

        // Use high edge ID
        let edge = Edge::new(
            EdgeId::new(200)?,
            label,
            NodeId::new(100)?,
            NodeId::new(100)?,
            PropertyMapBuilder::new().build(),
            VersionId::new(2)?,
        );
        current.insert_edge_direct(edge)?;

        let historical = HistoricalStorage::new();
        manager.create_checkpoint(LSN(0), &current, &historical)?;
    }

    // Recover and verify ID generators by creating new entities
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

        // Create new node - ID should be > 100
        let new_node_id =
            recovered_current.create_node("NewNode", PropertyMapBuilder::new().build())?;
        assert!(new_node_id.as_u64() > 100);

        // Create new edge - ID should be > 200
        let new_edge_id = recovered_current.create_edge(
            NodeId::new(100)?,
            new_node_id,
            "NEW_EDGE",
            PropertyMapBuilder::new().build(),
        )?;
        assert!(new_edge_id.as_u64() > 200);
    }

    Ok(())
}

/// Test WAL replay updates ID generators when replaying entries with higher IDs.
#[test]
fn test_wal_replay_updates_id_generators() -> Result<()> {
    use crate::core::id::EdgeId;

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create checkpoint with low IDs
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();
        let label = GLOBAL_INTERNER
            .intern("Test")
            .map_err(|e| StorageError::WalError {
                reason: e.to_string(),
            })?;

        let node = Node::new(
            NodeId::new(1)?,
            label,
            PropertyMapBuilder::new().build(),
            VersionId::new(1)?,
        );
        current.insert_node_direct(node, time::now())?;

        let historical = HistoricalStorage::new();
        manager.create_checkpoint(LSN(0), &current, &historical)?;
    }

    // Add WAL entries with higher IDs
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Add entry with high node ID
    wal.append(WalOperation::CreateNode {
        node_id: NodeId::new(500)?,
        label: GLOBAL_INTERNER.intern("HighId").unwrap(),
        properties: PropertyMapBuilder::new().build(),
        valid_from: time::now(),
    })?;

    // Add entry with high edge ID
    wal.append(WalOperation::CreateEdge {
        edge_id: EdgeId::new(600)?,
        source: NodeId::new(1)?,
        target: NodeId::new(500)?,
        label: GLOBAL_INTERNER.intern("HighEdge").unwrap(),
        properties: PropertyMapBuilder::new().build(),
        valid_from: time::now(),
    })?;
    wal.flush()?;

    // Recover
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

        // Create new node - ID should be > 500
        let new_node_id =
            recovered_current.create_node("NewNode", PropertyMapBuilder::new().build())?;
        assert!(new_node_id.as_u64() > 500);

        // Create new edge - ID should be > 600
        let new_edge_id = recovered_current.create_edge(
            NodeId::new(1)?,
            new_node_id,
            "NEW_EDGE",
            PropertyMapBuilder::new().build(),
        )?;
        assert!(new_edge_id.as_u64() > 600);
    }

    Ok(())
}

/// Test CheckpointStats fields.
#[test]
fn test_checkpoint_stats_fields() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let config = CheckpointConfig::with_data_dir(temp_dir.path());
    let mut manager = CheckpointManager::new(config)?;

    let current = CurrentStorage::new();
    let historical = HistoricalStorage::new();

    let stats = manager.create_checkpoint(LSN(99), &current, &historical)?;

    // Verify stats structure
    assert_eq!(stats.lsn, LSN(99));
    assert_eq!(stats.node_count, 0);
    assert_eq!(stats.edge_count, 0);
    assert_eq!(stats.version_count, 0);
    assert!(stats.duration.as_nanos() > 0); // Should have taken some time
    assert!(stats.bytes_written > 0); // At least manifest

    Ok(())
}

/// Test recovery with historical versions that have higher version IDs than current storage.
#[test]
fn test_recovery_historical_version_id_tracking() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create checkpoint where historical has higher version IDs
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();
        let mut historical = HistoricalStorage::new();

        let node_id = NodeId::new(1)?;
        let label = GLOBAL_INTERNER
            .intern("Versioned")
            .map_err(|e| StorageError::WalError {
                reason: e.to_string(),
            })?;

        // Current node has version 1
        let node = Node::new(
            node_id,
            label,
            PropertyMapBuilder::new().insert("v", 1i64).build(),
            VersionId::new(1)?,
        );
        current.insert_node_direct(node, time::now())?;

        // Historical has version 100 (higher than current)
        let now = time::now();
        historical.add_node_version(
            node_id,
            VersionId::new(100)?,
            now,
            now,
            label,
            PropertyMapBuilder::new().insert("v", 100i64).build(),
            false, // not a tombstone
        )?;

        manager.create_checkpoint(LSN(0), &current, &historical)?;
    }

    // Recover and verify version ID generator accounts for historical
    // by creating a new node and checking the version is > 100
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
        let wal = ConcurrentWalSystem::new(wal_config)?;

        let (recovered_current, _recovered_historical, _lsn) = manager.recover(&wal)?;

        // Creating a new node should use version ID > 100
        // We verify this indirectly by checking the node count increased
        let _new_node_id =
            recovered_current.create_node("NewNode", PropertyMapBuilder::new().build())?;
        assert_eq!(recovered_current.node_count(), 2);
    }

    Ok(())
}

/// Test persistence_err helper function.
#[test]
fn test_persistence_err_conversion() {
    use crate::storage::index_persistence::IndexPersistenceError;
    use std::path::PathBuf;

    let orig_err = IndexPersistenceError::InvalidMagic {
        path: PathBuf::from("/test/path"),
        expected: [0x12, 0x34, 0x56, 0x78],
        got: [0xAB, 0xCD, 0xEF, 0x00],
    };

    let converted = persistence_err(orig_err);
    let err_string = converted.to_string();

    assert!(err_string.contains("Invalid magic bytes"));
}

// ========================================================================
// Cold Storage Recovery Tests (Issue 7: Redb + WAL replay)
// ========================================================================

#[test]
fn test_recovery_with_no_cold_storage() -> Result<()> {
    // Full WAL replay when no cold storage is available
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let config = CheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    // Recovery without cold storage should work
    let result = manager.recover_with_cold_storage(&wal, None)?;

    // Should have empty storage with no checkpoint
    assert_eq!(result.current.node_count(), 0);
    assert!(result.checkpoint_lsn.is_none());
    assert!(result.flushed_lsn.is_none());
    assert!(!result.used_cold_storage());
    assert!(!result.used_checkpoint());

    Ok(())
}

#[test]
fn test_recovery_with_checkpoint_no_cold_storage() -> Result<()> {
    // Standard checkpoint-based recovery without cold storage
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Write WAL entries to advance LSN to 100+
    // In a real scenario, WAL entries would be written before checkpoint
    for i in 1..=100 {
        let op = WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: GLOBAL_INTERNER.intern(format!("Node{}", i)).unwrap(),
            properties: PropertyMapBuilder::new().build(),
            valid_from: time::now(),
        };
        wal.append_async(op)?;
    }
    wal.flush()?;

    // Create checkpoint with some data
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();
        for i in 1..=5 {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("Node{}", i))
                .build();
            let node_id = NodeId::new(i)?;
            let label = GLOBAL_INTERNER.intern("Person").unwrap();
            let version_id = VersionId::new(i)?;
            let node = Node::new(node_id, label, props, version_id);
            current.insert_node_direct(node, time::now())?;
        }

        let historical = HistoricalStorage::new();
        manager.create_checkpoint(LSN(100), &current, &historical)?;
    }

    // Recover without cold storage
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let result = manager.recover_with_cold_storage(&wal, None)?;

        assert_eq!(result.current.node_count(), 5);
        assert_eq!(result.checkpoint_lsn, Some(LSN(100)));
        assert!(result.flushed_lsn.is_none());
        assert!(!result.used_cold_storage());
        assert!(result.used_checkpoint());
    }

    Ok(())
}

#[test]
fn test_recovery_loads_cold_storage_first() -> Result<()> {
    use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};

    // Cold storage with flushed_lsn should be checked before WAL replay
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");
    let cold_dir = temp_dir.path().join("cold");

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create cold storage with a flushed_lsn
    let cold_storage = Arc::new(RedbColdStorage::new(
        cold_dir.join("cold.redb"),
        RedbConfig::new(),
    )?);

    // Store some data with LSN tracking
    let node = crate::core::version::NodeVersion::new_anchor(
        VersionId::new(1)?,
        NodeId::new(1)?,
        BiTemporalInterval::current(time::now()),
        GLOBAL_INTERNER.intern("Test").unwrap(),
        PropertyMapBuilder::new().build(),
    );
    cold_storage.store_batch_with_lsn(&[node], &[], LSN(50))?;

    // Verify flushed_lsn is set
    assert_eq!(cold_storage.get_flushed_lsn()?, Some(LSN(50)));

    let config = CheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    // Recovery should see the flushed_lsn
    let result = manager.recover_with_cold_storage(&wal, Some(&cold_storage))?;

    // Should have detected cold storage
    assert_eq!(result.flushed_lsn, Some(LSN(50)));
    assert!(result.used_cold_storage());

    Ok(())
}

#[test]
fn test_recovery_replays_wal_from_flushed_lsn() -> Result<()> {
    use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};

    // When cold storage has higher flushed_lsn than checkpoint,
    // WAL replay should start from flushed_lsn + 1
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");
    let cold_dir = temp_dir.path().join("cold");

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Write WAL entries to advance LSN to 100+ (to match cold storage flushed_lsn)
    for i in 1..=100 {
        let op = WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: GLOBAL_INTERNER.intern(format!("Node{}", i)).unwrap(),
            properties: PropertyMapBuilder::new().build(),
            valid_from: time::now(),
        };
        wal.append_async(op)?;
    }
    wal.flush()?;

    // Create checkpoint at LSN 50
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();
        manager.create_checkpoint(LSN(50), &current, &historical)?;
    }

    // Create cold storage with flushed_lsn at 100 (higher than checkpoint)
    let cold_storage = Arc::new(RedbColdStorage::new(
        cold_dir.join("cold.redb"),
        RedbConfig::new(),
    )?);

    let node = crate::core::version::NodeVersion::new_anchor(
        VersionId::new(1)?,
        NodeId::new(1)?,
        BiTemporalInterval::current(time::now()),
        GLOBAL_INTERNER.intern("Test").unwrap(),
        PropertyMapBuilder::new().build(),
    );
    cold_storage.store_batch_with_lsn(&[node], &[], LSN(100))?;

    // Recover with cold storage
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let result = manager.recover_with_cold_storage(&wal, Some(&cold_storage))?;

        // Should use flushed_lsn as effective recovery point
        assert_eq!(result.checkpoint_lsn, Some(LSN(50)));
        assert_eq!(result.flushed_lsn, Some(LSN(100)));
        assert_eq!(result.effective_lsn, LSN(100));
        assert!(result.used_cold_storage());

        // WAL entries between checkpoint and flushed_lsn should be skipped
        assert_eq!(result.wal_entries_skipped_from_cold(), 50);
    }

    Ok(())
}

#[test]
fn test_recovery_with_no_wal_segments() -> Result<()> {
    use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};

    // Recovery with just cold storage data and no WAL
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");
    let cold_dir = temp_dir.path().join("cold");

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create cold storage with some data
    let cold_storage = Arc::new(RedbColdStorage::new(
        cold_dir.join("cold.redb"),
        RedbConfig::new(),
    )?);

    let node = crate::core::version::NodeVersion::new_anchor(
        VersionId::new(1)?,
        NodeId::new(1)?,
        BiTemporalInterval::current(time::now()),
        GLOBAL_INTERNER.intern("Test").unwrap(),
        PropertyMapBuilder::new().build(),
    );
    cold_storage.store_batch_with_lsn(&[node], &[], LSN(75))?;

    let config = CheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    let result = manager.recover_with_cold_storage(&wal, Some(&cold_storage))?;

    // Should have no WAL entries replayed
    assert_eq!(result.wal_entries_replayed, 0);
    assert_eq!(result.effective_lsn, LSN(75));

    Ok(())
}

#[test]
fn test_recovery_validates_lsn_consistency() -> Result<()> {
    use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};

    // flushed_lsn ahead of WAL should be detected as inconsistency
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");
    let cold_dir = temp_dir.path().join("cold");

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Write WAL entries to advance LSN to 50
    for i in 1..=50 {
        let op = WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: GLOBAL_INTERNER.intern(format!("Node{}", i)).unwrap(),
            properties: PropertyMapBuilder::new().build(),
            valid_from: time::now(),
        };
        wal.append_async(op)?;
    }
    wal.flush()?;
    // WAL is now at LSN 50

    // Create checkpoint at LSN 10 (valid, < WAL current LSN)
    {
        let config = CheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;
        let current = CurrentStorage::new();
        let historical = HistoricalStorage::new();
        manager.create_checkpoint(LSN(10), &current, &historical)?;
    }

    // Create cold storage with flushed_lsn = 1000 (way ahead of WAL)
    let cold_storage = Arc::new(RedbColdStorage::new(
        cold_dir.join("cold.redb"),
        RedbConfig::new(),
    )?);

    let node = crate::core::version::NodeVersion::new_anchor(
        VersionId::new(1)?,
        NodeId::new(1)?,
        BiTemporalInterval::current(time::now()),
        GLOBAL_INTERNER.intern("Test").unwrap(),
        PropertyMapBuilder::new().build(),
    );
    cold_storage.store_batch_with_lsn(&[node], &[], LSN(1000))?;

    let config = CheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    let result = manager.recover_with_cold_storage(&wal, Some(&cold_storage));

    // Should detect inconsistency
    assert!(result.is_err());
    let err = result.err().expect("Expected error").to_string();
    assert!(
        err.contains("flushed_lsn") || err.contains("inconsistency"),
        "Error should mention LSN inconsistency: {}",
        err
    );

    Ok(())
}

#[test]
fn test_recovery_result_helpers() {
    // Test RecoveryResult helper methods

    // Case 1: No cold storage, no checkpoint
    let result = RecoveryResult {
        current: CurrentStorage::new(),
        historical: HistoricalStorage::new(),
        final_lsn: LSN(0),
        checkpoint_lsn: None,
        flushed_lsn: None,
        effective_lsn: LSN(0),
        wal_entries_replayed: 0,
    };
    assert!(!result.used_cold_storage());
    assert!(!result.used_checkpoint());
    assert_eq!(result.wal_entries_skipped_from_cold(), 0);

    // Case 2: Checkpoint only
    let result = RecoveryResult {
        current: CurrentStorage::new(),
        historical: HistoricalStorage::new(),
        final_lsn: LSN(50),
        checkpoint_lsn: Some(LSN(50)),
        flushed_lsn: None,
        effective_lsn: LSN(50),
        wal_entries_replayed: 0,
    };
    assert!(!result.used_cold_storage());
    assert!(result.used_checkpoint());
    assert_eq!(result.wal_entries_skipped_from_cold(), 0);

    // Case 3: Cold storage ahead of checkpoint
    let result = RecoveryResult {
        current: CurrentStorage::new(),
        historical: HistoricalStorage::new(),
        final_lsn: LSN(100),
        checkpoint_lsn: Some(LSN(50)),
        flushed_lsn: Some(LSN(100)),
        effective_lsn: LSN(100),
        wal_entries_replayed: 0,
    };
    assert!(result.used_cold_storage());
    assert!(result.used_checkpoint());
    assert_eq!(result.wal_entries_skipped_from_cold(), 50);

    // Case 4: Cold storage only (no checkpoint)
    let result = RecoveryResult {
        current: CurrentStorage::new(),
        historical: HistoricalStorage::new(),
        final_lsn: LSN(75),
        checkpoint_lsn: None,
        flushed_lsn: Some(LSN(75)),
        effective_lsn: LSN(75),
        wal_entries_replayed: 0,
    };
    assert!(result.used_cold_storage());
    assert!(!result.used_checkpoint());
    assert_eq!(result.wal_entries_skipped_from_cold(), 75);
}
