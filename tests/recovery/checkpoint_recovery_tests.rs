//! Tests for CheckpointManager with index persistence recovery (Issue #294 extension)
//!
//! These tests verify that the checkpoint system correctly:
//! - Persists full graph and temporal state to disk
//! - Recovers database state from persisted indexes
//! - Correctly replays WAL entries after checkpoint LSN
//! - Maintains LSN consistency across checkpoint and recovery

use gallifreydb::{
    PropertyMapBuilder,
    core::{
        GLOBAL_INTERNER,
        graph::Node,
        id::{EdgeId, NodeId, VersionId},
        property::PropertyMap,
        temporal::{BiTemporalInterval, time},
    },
    storage::{
        CheckpointManager, UnifiedCheckpointConfig,
        current::CurrentStorage,
        historical::HistoricalStorage,
        wal::{
            LSN, WalOperation,
            concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
        },
    },
    utils::error::Result,
};
use tempfile::TempDir;

// ============================================================================
// CheckpointManager Recovery Tests
// ============================================================================

#[test]
fn test_checkpoint_recovery_basic() -> Result<()> {
    // Given: WAL with some entries and a checkpoint
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create WAL and add entries
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    for i in 1..=5 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node{}", i))
            .build();
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: "Person".to_string(),
            properties: props,
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // Create checkpoint with matching state
    let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    // When: Recover from empty checkpoint + WAL
    let (recovered_current, _recovered_historical, _final_lsn) = manager.recover(&wal)?;

    // Then: All nodes should be recovered
    assert_eq!(recovered_current.node_count(), 5);

    for i in 1..=5 {
        let node = recovered_current.get_node(NodeId::new(i)?)?;
        let name = node.get_property("name").unwrap().as_str().unwrap();
        assert_eq!(name, format!("Node{}", i));
    }

    Ok(())
}

#[test]
fn test_checkpoint_with_persisted_state_and_wal_replay() -> Result<()> {
    // Given: Checkpoint with 3 nodes, WAL has 2 more nodes after checkpoint
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create WAL entries
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // First 3 entries will be checkpointed
    for i in 1..=3 {
        let props = PropertyMapBuilder::new().insert("value", i as i64).build();
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: "Test".to_string(),
            properties: props,
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // Create checkpoint at LSN 3
    let checkpoint_lsn = LSN(wal.current_lsn().0.saturating_sub(1));
    {
        let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        // Build current storage state matching WAL
        let current = CurrentStorage::new();
        let label = GLOBAL_INTERNER.intern("Test")?;
        for i in 1..=3 {
            let props = PropertyMapBuilder::new().insert("value", i as i64).build();
            let node = Node::new(NodeId::new(i)?, label, props, VersionId::new(i)?);
            current.insert_node_direct(node, time::now())?;
        }

        let historical = HistoricalStorage::new();
        manager.create_checkpoint(checkpoint_lsn, &current, &historical)?;
    }

    // Add 2 more WAL entries after checkpoint
    for i in 4..=5 {
        let props = PropertyMapBuilder::new().insert("value", i as i64).build();
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: "Test".to_string(),
            properties: props,
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // When: Recover with new manager (simulates restart)
    let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;
    let (recovered_current, _recovered_historical, _final_lsn) = manager.recover(&wal)?;

    // Then: Should have all 5 nodes (3 from checkpoint + 2 from WAL)
    assert_eq!(recovered_current.node_count(), 5);

    for i in 1..=5 {
        let node = recovered_current.get_node(NodeId::new(i)?)?;
        let value = node.get_property("value").unwrap().as_int().unwrap();
        assert_eq!(value, i as i64);
    }

    Ok(())
}

#[test]
fn test_checkpoint_recovery_preserves_edges() -> Result<()> {
    // Given: Checkpoint with nodes and edges
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Track the edge ID we create
    let edge_id: EdgeId;

    // Create checkpoint with nodes and edges
    {
        let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();

        // Create nodes
        let node1 = current.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )?;
        let node2 = current.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )?;

        // Create edge and store its ID
        edge_id = current.create_edge(
            node1,
            node2,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )?;

        let historical = HistoricalStorage::new();
        manager.create_checkpoint(LSN(10), &current, &historical)?;
    }

    // When: Recover
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;
    let (recovered_current, _recovered_historical, _final_lsn) = manager.recover(&wal)?;

    // Then: Nodes and edges should be recovered
    assert_eq!(recovered_current.node_count(), 2);
    assert_eq!(recovered_current.edge_count(), 1);

    let edge = recovered_current.get_edge(edge_id)?;
    let since = edge.get_property("since").unwrap().as_int().unwrap();
    assert_eq!(since, 2020);

    Ok(())
}

#[test]
fn test_checkpoint_recovery_id_generators_initialized() -> Result<()> {
    // Given: Checkpoint with nodes having specific (high) IDs
    // This tests that after recovery, ID generators continue from max existing IDs
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create nodes and checkpoint
    {
        let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;

        let current = CurrentStorage::new();

        // Create 5 nodes using the normal API, which generates sequential IDs
        for i in 1..=5 {
            current.create_node(
                "Test",
                PropertyMapBuilder::new().insert("idx", i as i64).build(),
            )?;
        }

        // Verify we have 5 nodes
        assert_eq!(current.node_count(), 5);

        let historical = HistoricalStorage::new();
        let stats = manager.create_checkpoint(LSN(5), &current, &historical)?;

        // Verify checkpoint captured the nodes
        assert_eq!(stats.node_count, 5);
    }

    // When: Recover and create new node
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;

    // First verify persisted state exists
    assert!(
        manager.has_persisted_state(),
        "Should have persisted state after checkpoint"
    );

    let (recovered_current, _recovered_historical, _final_lsn) = manager.recover(&wal)?;

    // Verify recovery loaded the nodes
    assert_eq!(
        recovered_current.node_count(),
        5,
        "Should have 5 nodes after recovery"
    );

    // Create a new node - ID should be >= 5 (since IDs start at 0, nodes have IDs 0-4)
    let new_node_id = recovered_current.create_node("NewNode", PropertyMap::new())?;

    // Then: New node ID should be >= 5 (since IDs 0-4 were used by the 5 recovered nodes)
    assert!(
        new_node_id.as_u64() >= 5,
        "New node ID {} should be >= 5 (after max recovered ID 4)",
        new_node_id.as_u64()
    );

    Ok(())
}

#[test]
fn test_checkpoint_manager_should_checkpoint() -> Result<()> {
    // Given: CheckpointManager with specific thresholds
    let temp_dir = TempDir::new().unwrap();
    let config = UnifiedCheckpointConfig {
        data_dir: temp_dir.path().to_path_buf(),
        checkpoint_interval: std::time::Duration::from_secs(3600), // 1 hour
        min_wal_entries: 100,
        ..Default::default()
    };
    let mut manager = CheckpointManager::new(config)?;

    // When: Check with various LSNs

    // Then: Should checkpoint initially (never checkpointed)
    assert!(manager.should_checkpoint(LSN(1)));

    // Simulate a checkpoint
    manager.create_checkpoint(LSN(50), &CurrentStorage::new(), &HistoricalStorage::new())?;

    // Should NOT checkpoint (not enough entries)
    assert!(!manager.should_checkpoint(LSN(60)));

    // Should checkpoint when threshold exceeded
    assert!(manager.should_checkpoint(LSN(200)));

    Ok(())
}

#[test]
fn test_checkpoint_recovery_with_updates() -> Result<()> {
    // Given: WAL with create and update operations
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create WAL with create and update
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1)?;

    // Create node
    wal.append(WalOperation::CreateNode {
        node_id,
        label: "Person".to_string(),
        properties: PropertyMapBuilder::new().insert("name", "Alice").build(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Update node
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(2)?,
        label: "Person".to_string(),
        properties: PropertyMapBuilder::new()
            .insert("name", "Alice Updated")
            .build(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    wal.flush()?;

    // When: Recover
    let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;
    let (recovered_current, recovered_historical, _final_lsn) = manager.recover(&wal)?;

    // Then: Current state should reflect the update
    let node = recovered_current.get_node(node_id)?;
    let name = node.get_property("name").unwrap().as_str().unwrap();
    assert_eq!(name, "Alice Updated");

    // Historical should have version entries
    let hist_stats = recovered_historical.stats();
    assert!(
        hist_stats.total_node_versions >= 1,
        "Should have historical versions"
    );

    Ok(())
}

#[test]
fn test_checkpoint_recovery_with_deletes() -> Result<()> {
    // Given: WAL with create and delete operations
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    // Create WAL
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create 3 nodes
    for i in 1..=3 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i)?,
            label: "Test".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    // Delete node 2
    wal.append(WalOperation::DeleteNode {
        node_id: NodeId::new(2)?,
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    wal.flush()?;

    // When: Recover
    let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;
    let (recovered_current, _recovered_historical, _final_lsn) = manager.recover(&wal)?;

    // Then: Should have 2 nodes (node 2 was deleted)
    assert_eq!(recovered_current.node_count(), 2);
    assert!(recovered_current.get_node(NodeId::new(1)?).is_ok());
    assert!(recovered_current.get_node(NodeId::new(2)?).is_err()); // Deleted
    assert!(recovered_current.get_node(NodeId::new(3)?).is_ok());

    Ok(())
}

#[test]
fn test_checkpoint_stats() -> Result<()> {
    // Given: Storage with specific data
    let temp_dir = TempDir::new().unwrap();
    let config = UnifiedCheckpointConfig::with_data_dir(temp_dir.path());
    let mut manager = CheckpointManager::new(config)?;

    let current = CurrentStorage::new();

    // Create 10 nodes and collect their IDs
    let mut node_ids = Vec::new();
    for i in 1..=10 {
        let node_id = current.create_node(
            "Test",
            PropertyMapBuilder::new().insert("idx", i as i64).build(),
        )?;
        node_ids.push(node_id);
    }

    // Create 5 edges between nodes
    for i in 0..5 {
        current.create_edge(node_ids[i], node_ids[i + 5], "LINKS", PropertyMap::new())?;
    }

    let historical = HistoricalStorage::new();

    // When: Create checkpoint
    let stats = manager.create_checkpoint(LSN(50), &current, &historical)?;

    // Then: Stats should reflect the data
    assert_eq!(stats.node_count, 10);
    assert_eq!(stats.edge_count, 5);
    assert_eq!(stats.lsn, LSN(50));
    assert!(stats.bytes_written > 0);
    assert!(stats.duration.as_nanos() > 0);

    Ok(())
}

#[test]
fn test_checkpoint_has_persisted_state() -> Result<()> {
    // Given: Fresh CheckpointManager
    let temp_dir = TempDir::new().unwrap();
    let config = UnifiedCheckpointConfig::with_data_dir(temp_dir.path());
    let mut manager = CheckpointManager::new(config)?;

    // Then: Initially no persisted state
    assert!(!manager.has_persisted_state());

    // When: Create checkpoint
    manager.create_checkpoint(LSN(1), &CurrentStorage::new(), &HistoricalStorage::new())?;

    // Then: Now has persisted state
    assert!(manager.has_persisted_state());

    Ok(())
}

#[test]
fn test_checkpoint_get_persisted_lsn() -> Result<()> {
    // Given: CheckpointManager with no checkpoint
    let temp_dir = TempDir::new().unwrap();
    let config = UnifiedCheckpointConfig::with_data_dir(temp_dir.path());
    let mut manager = CheckpointManager::new(config)?;

    // Then: No persisted LSN
    assert_eq!(manager.get_persisted_lsn(), None);

    // When: Create checkpoint at LSN 42
    manager.create_checkpoint(LSN(42), &CurrentStorage::new(), &HistoricalStorage::new())?;

    // Then: Persisted LSN is 42
    assert_eq!(manager.get_persisted_lsn(), Some(LSN(42)));

    Ok(())
}
