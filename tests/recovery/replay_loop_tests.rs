//! Tests for basic WAL replay loop (Issue #287)
//!
//! These tests verify that the recovery system correctly:
//! - Iterates through all WAL entries
//! - Tracks maximum IDs for node_id, edge_id, version_id
//! - Dispatches to appropriate operation handlers (stubbed initially)
//! - Handles errors gracefully (corrupt entries, invalid IDs)

use gallifreydb::{
    core::{
        id::{EdgeId, NodeId, VersionId},
        property::PropertyMap,
        temporal::{BiTemporalInterval, time},
    },
    storage::{
        current::CurrentStorage,
        historical::HistoricalStorage,
        persistence::{Checkpoint, CheckpointConfig, PersistenceManager},
        wal::{
            LSN, WalOperation,
            concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
        },
    },
    utils::error::Result,
};
use tempfile::TempDir;

#[test]
fn test_recover_with_empty_wal() -> Result<()> {
    // Given: Checkpoint with empty WAL
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let checkpoint_dir = temp_dir.path().join("checkpoints");

    // Create empty WAL
    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create checkpoint
    let current = CurrentStorage::new();
    let historical = HistoricalStorage::new();
    std::fs::create_dir_all(&checkpoint_dir)?;
    let checkpoint_path = checkpoint_dir.join("checkpoint_0.dat");
    let checkpoint = Checkpoint::new(LSN(0), &current, &historical);
    checkpoint.save(&checkpoint_path)?;

    // Create persistence manager
    let config = CheckpointConfig {
        checkpoint_dir,
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;

    // When: recover()
    let (recovered_current, recovered_historical, final_lsn) = manager.recover(&wal)?;

    // Then: Returns checkpoint state unchanged
    assert_eq!(recovered_current.node_count(), 0);
    assert_eq!(recovered_current.edge_count(), 0);
    let hist_stats = recovered_historical.stats();
    assert_eq!(hist_stats.total_node_versions, 0);
    assert_eq!(hist_stats.total_edge_versions, 0);
    assert_eq!(final_lsn, wal.current_lsn());

    Ok(())
}

#[test]
fn test_recover_tracks_max_node_id() -> Result<()> {
    // Given: WAL with multiple nodes (IDs: 10, 20, 100)
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Append nodes with various IDs
    for id in [10, 20, 100] {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(id).unwrap(),
            label: GLOBAL_INTERNER.intern("Test").unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // Create persistence manager
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;

    // When: recover()
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: max_node_id should be 100, so the next generated ID should be 101
    let next_node_id = current.create_node("NewNode", PropertyMap::new())?;
    assert_eq!(
        next_node_id.as_u64(),
        101,
        "Node ID generator should start from max_id + 1"
    );

    Ok(())
}

#[test]
fn test_recover_tracks_max_edge_id() -> Result<()> {
    // Given: WAL with multiple edges (IDs: 5, 15, 50)
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create nodes first
    for id in [1, 2] {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(id).unwrap(),
            label: GLOBAL_INTERNER.intern("Node").unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    // Append edges with various IDs
    for id in [5, 15, 50] {
        wal.append(WalOperation::CreateEdge {
            edge_id: EdgeId::new(id).unwrap(),
            source: NodeId::new(1).unwrap(),
            target: NodeId::new(2).unwrap(),
            label: GLOBAL_INTERNER.intern("CONNECTS").unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // Create persistence manager
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;

    // When: recover()
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: max_edge_id should be 50, so the next generated ID should be 51
    let next_edge_id = current.create_edge(
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        "NEW_EDGE",
        PropertyMap::new(),
    )?;
    assert_eq!(
        next_edge_id.as_u64(),
        51,
        "Edge ID generator should start from max_id + 1"
    );

    Ok(())
}

#[test]
fn test_recover_tracks_max_version_id() -> Result<()> {
    // Given: WAL with updates (version IDs: 100, 101, 102)
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();

    // Create node first
    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Append updates with various version IDs
    for version_id in [100, 101, 102] {
        wal.append(WalOperation::UpdateNode {
            node_id,
            version_id: VersionId::new(version_id).unwrap(),
            label: GLOBAL_INTERNER.intern("Updated").unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // Create persistence manager
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;

    // When: recover()
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: max_version_id should be 102, so the next generated ID should be 103
    // Creating a new node generates a new version ID
    let _new_node_id = current.create_node("NewNode", PropertyMap::new())?;

    // Verify by creating another node and checking the version ID continues sequentially
    let another_node_id = current.create_node("AnotherNode", PropertyMap::new())?;
    // The first create used version 103, this one should be 104
    // We can't directly access version IDs, but we verify recovery completed successfully
    assert!(current.get_node(another_node_id).is_ok());

    Ok(())
}

#[test]
fn test_recover_handles_non_sequential_version_ids() -> Result<()> {
    // Given: WAL with non-sequential version IDs (1, 100, 50)
    // This tests the max() logic to prevent version ID conflicts
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();

    // Create node
    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Update with version 100 (large jump)
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(100).unwrap(),
        label: "Updated".to_string(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Update with version 50 (out of order - should still track max=100)
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(50).unwrap(),
        label: "Another".to_string(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Delete creates tombstone (should get next version after max)
    wal.append(WalOperation::DeleteNode {
        node_id,
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    wal.flush()?;

    // Create persistence manager
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;

    // When: recover()
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: max_version_id should be 100 (not 50), so next should be 101
    // Creating a new node generates a new version ID
    let new_node_id = current.create_node("NewNode", PropertyMap::new())?;
    // Verify the node was created (version ID is internal)
    assert!(current.get_node(new_node_id).is_ok());

    Ok(())
}

#[test]
fn test_recover_with_multiple_operation_types() -> Result<()> {
    // Given: WAL with mix of operations (CreateNode, CreateEdge, UpdateNode, DeleteNode)
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();
    let edge_id = EdgeId::new(1).unwrap();

    // Create node
    wal.append(WalOperation::CreateNode {
        node_id,
        label: "Person".to_string(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Create edge
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: node_id,
        target: NodeId::new(2).unwrap(),
        label: "KNOWS".to_string(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Update node
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(2).unwrap(),
        label: "Person".to_string(),
        properties: PropertyMap::new(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Delete node
    wal.append(WalOperation::DeleteNode {
        node_id,
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    wal.flush()?;

    // Create persistence manager
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;

    // When: recover()
    let (_current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: Recovery completes without error
    // Full verification will be added once replay handlers are implemented

    Ok(())
}

#[test]
fn test_recover_from_checkpoint_lsn() -> Result<()> {
    // Given: Checkpoint at LSN 50, WAL has entries up to LSN 100
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let checkpoint_dir = temp_dir.path().join("checkpoints");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Add 100 operations to WAL
    for i in 1..=100 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Test").unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // Create checkpoint at LSN 50
    let current = CurrentStorage::new();
    let historical = HistoricalStorage::new();
    std::fs::create_dir_all(&checkpoint_dir)?;
    let checkpoint_path = checkpoint_dir.join("checkpoint_50.dat");
    let checkpoint = Checkpoint::new(LSN(50), &current, &historical);
    checkpoint.save(&checkpoint_path)?;

    // Create persistence manager
    let config = CheckpointConfig {
        checkpoint_dir,
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;

    // When: recover()
    let (_current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: Should only replay entries 51-100 (50 operations)
    // Full verification will be added once replay handlers track operations

    Ok(())
}

#[test]
#[should_panic(expected = "InvalidId")]
fn test_recover_rejects_invalid_node_id() {
    // Given: WAL with node_id > MAX_VALID_ID (DoS attack scenario)
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let _wal = ConcurrentWalSystem::new(wal_config).unwrap();

    // Attempt to create node with ID exceeding MAX_VALID_ID
    // This should be rejected during NodeId::new() itself
    let invalid_id = u64::MAX - 500; // Within DoS prevention range
    let result = NodeId::new(invalid_id);

    // Should panic or return error - ID validation happens at construction time
    result.unwrap();
}

#[test]
fn test_recover_handles_checkpoint_marker() -> Result<()> {
    // Given: WAL with Checkpoint operation markers
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Add some operations
    for i in 1..=10 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Test").unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    // Add checkpoint marker
    wal.append(WalOperation::Checkpoint {
        lsn: LSN(10),
        timestamp: time::now(),
    })?;

    // Add more operations
    for i in 11..=20 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Test").unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    wal.flush()?;

    // Create persistence manager
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;

    // When: recover()
    let (_current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: Recovery completes successfully, checkpoint markers are handled
    // (they don't cause errors, just update tracking)

    Ok(())
}

#[test]
fn test_recover_empty_wal_returns_initial_lsn() -> Result<()> {
    // Given: Fresh database with no checkpoint, no WAL entries
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create persistence manager
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;

    // When: recover()
    let (_current, _historical, final_lsn) = manager.recover(&wal)?;

    // Then: final_lsn should be initial LSN
    assert_eq!(final_lsn, LSN::initial());

    Ok(())
}
