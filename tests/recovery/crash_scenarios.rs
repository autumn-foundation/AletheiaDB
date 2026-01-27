//! End-to-end crash scenario tests for recovery validation (Issue #294)
//!
//! These tests simulate real crash scenarios to validate that the GallifreyDB
//! recovery system works correctly under various failure conditions:
//!
//! 1. **Crash Before Checkpoint**: Recovery from WAL only
//! 2. **Crash After Checkpoint**: Recovery replays only post-checkpoint entries
//! 3. **Crash During WAL Write**: Handles truncated/corrupt final entries
//! 4. **Multiple Crashes**: Sequential crash/recovery cycles
//! 5. **Complex Workflow**: Create/update/delete with historical version verification
//!
//! ## Known Limitation: Checkpoint State Serialization
//!
//! The current checkpoint implementation only stores metadata (LSN, counts),
//! NOT the actual database state. This means:
//! - Pre-checkpoint WAL entries are NOT recovered if a checkpoint exists
//! - Full checkpoint serialization via index_persistence is not yet integrated
//!
//! See: <https://github.com/madmax983/GallifreyDB/issues/XXX> (TODO: create issue)

use gallifreydb::{
    GLOBAL_INTERNER,
    core::{
        id::{EdgeId, NodeId, VersionId},
        property::{PropertyMap, PropertyMapBuilder, PropertyValue},
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
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ============================================================================
// Helper Functions
// ============================================================================

/// Find all WAL segment files in the given directory, sorted by name.
///
/// Returns files sorted to ensure deterministic ordering across platforms,
/// since `std::fs::read_dir` does not guarantee any particular order.
fn find_wal_files(wal_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut wal_files: Vec<PathBuf> = std::fs::read_dir(wal_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|s| s.ends_with(".log"))
                .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();

    // Sort by path to ensure deterministic ordering
    wal_files.sort();
    Ok(wal_files)
}

// ============================================================================
// Scenario 1: Crash Before Checkpoint
// ============================================================================

#[test]
fn test_crash_before_checkpoint_nodes_only() -> Result<()> {
    // Given: Create 100 nodes without saving checkpoint
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create 100 nodes
    for i in 1..=100 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern(format!("Node{}", i)).unwrap(),
            properties: PropertyMapBuilder::new().insert("index", i as i64).build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // Simulate crash: don't create checkpoint, just recover from WAL
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;

    // When: Recover (no checkpoint exists)
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: All 100 nodes should be present in current storage
    assert_eq!(
        current.node_count(),
        100,
        "All 100 nodes should be recovered from WAL"
    );

    // Verify random sample of nodes
    for i in [1, 25, 50, 75, 100] {
        let node = current.get_node(NodeId::new(i).unwrap())?;
        assert!(node.has_label_str(&format!("Node{}", i)));
        assert!(matches!(
            node.properties.get("index"),
            Some(PropertyValue::Int(idx)) if *idx == i as i64
        ));
    }

    // Verify historical versions exist
    let hist_stats = historical.stats();
    assert_eq!(
        hist_stats.total_node_versions, 100,
        "Each node should have one historical version"
    );

    Ok(())
}

#[test]
fn test_crash_before_checkpoint_nodes_and_edges() -> Result<()> {
    // Given: Create 50 nodes and 50 edges without saving checkpoint
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create 50 nodes
    for i in 1..=50 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("name", format!("Person{}", i))
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    // Create 50 edges (chain: 1->2, 2->3, ..., 49->50)
    for i in 1..=49 {
        wal.append(WalOperation::CreateEdge {
            edge_id: EdgeId::new(i).unwrap(),
            source: NodeId::new(i).unwrap(),
            target: NodeId::new(i + 1).unwrap(),
            label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            properties: PropertyMapBuilder::new().insert("order", i as i64).build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    // Add one more edge (50->1) to make it circular
    wal.append(WalOperation::CreateEdge {
        edge_id: EdgeId::new(50).unwrap(),
        source: NodeId::new(50).unwrap(),
        target: NodeId::new(1).unwrap(),
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMapBuilder::new().insert("order", 50_i64).build(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;
    wal.flush()?;

    // When: Recover from WAL only
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: All entities should be present
    assert_eq!(current.node_count(), 50, "All 50 nodes should be recovered");
    assert_eq!(current.edge_count(), 50, "All 50 edges should be recovered");

    // Verify edge connectivity
    let edge = current.get_edge(EdgeId::new(1).unwrap())?;
    assert_eq!(edge.source, NodeId::new(1).unwrap());
    assert_eq!(edge.target, NodeId::new(2).unwrap());

    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 50);
    assert_eq!(hist_stats.total_edge_versions, 50);

    Ok(())
}

// ============================================================================
// Scenario 2: Crash After Checkpoint
// ============================================================================

#[test]
fn test_crash_after_checkpoint_recovery() -> Result<()> {
    // Given: Create 50 nodes, save checkpoint, add 50 more nodes
    // NOTE: Current checkpoint implementation stores metadata only, not full state.
    // Recovery replays only WAL entries AFTER the checkpoint LSN.
    // Pre-checkpoint data must be in the checkpoint (not currently implemented).
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let checkpoint_dir = temp_dir.path().join("checkpoints");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Phase 1: Create first 50 nodes
    for i in 1..=50 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Phase1").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("phase", 1_i64)
                .insert("index", i as i64)
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // Create checkpoint at LSN 50
    // NOTE: Checkpoint stores metadata only - pre-checkpoint state not serialized yet
    let current_at_checkpoint = CurrentStorage::new();
    let historical_at_checkpoint = HistoricalStorage::new();
    std::fs::create_dir_all(&checkpoint_dir)?;
    let checkpoint_path = checkpoint_dir.join("checkpoint_50.dat");
    let checkpoint = Checkpoint::new(LSN(50), &current_at_checkpoint, &historical_at_checkpoint);
    checkpoint.save(&checkpoint_path)?;

    // Phase 2: Create 50 more nodes (only these will be replayed)
    for i in 51..=100 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Phase2").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("phase", 2_i64)
                .insert("index", i as i64)
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // When: Recover from checkpoint + WAL replay
    let config = CheckpointConfig {
        checkpoint_dir,
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Only post-checkpoint nodes (51-100) should be present
    // This is because checkpoint only stores metadata, not actual state.
    // In a full implementation, all 100 would be recovered.
    assert_eq!(
        current.node_count(),
        50,
        "Only post-checkpoint nodes recovered (checkpoint doesn't store full state)"
    );

    // Verify only Phase2 nodes are present (post-checkpoint)
    assert!(
        current.get_node(NodeId::new(1).unwrap()).is_err(),
        "Pre-checkpoint node should not exist (not in checkpoint state)"
    );

    let node51 = current.get_node(NodeId::new(51).unwrap())?;
    assert!(node51.has_label_str("Phase2"));

    let node100 = current.get_node(NodeId::new(100).unwrap())?;
    assert!(node100.has_label_str("Phase2"));

    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 50);

    Ok(())
}

#[test]
fn test_checkpoint_with_mixed_operations() -> Result<()> {
    // Given: Create nodes, update some, create checkpoint, then add more
    // NOTE: Checkpoint only stores metadata - recovery replays only post-checkpoint entries
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let checkpoint_dir = temp_dir.path().join("checkpoints");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Phase 1: Create 20 nodes and update first 10 (these will be before checkpoint)
    for i in 1..=20 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Node").unwrap(),
            properties: PropertyMapBuilder::new().insert("value", i as i64).build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    // Update first 10 nodes
    for i in 1..=10 {
        wal.append(WalOperation::UpdateNode {
            node_id: NodeId::new(i).unwrap(),
            version_id: VersionId::new(20 + i).unwrap(),
            label: GLOBAL_INTERNER.intern("UpdatedNode").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("value", (i * 100) as i64)
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // Create checkpoint at LSN 30
    let current_at_checkpoint = CurrentStorage::new();
    let historical_at_checkpoint = HistoricalStorage::new();
    std::fs::create_dir_all(&checkpoint_dir)?;
    let checkpoint = Checkpoint::new(LSN(30), &current_at_checkpoint, &historical_at_checkpoint);
    checkpoint.save(&checkpoint_dir.join("checkpoint_30.dat"))?;

    // Phase 2: Add more operations (only these will be replayed)
    for i in 21..=30 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("NewNode").unwrap(),
            properties: PropertyMapBuilder::new().insert("value", i as i64).build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // When: Recover
    let config = CheckpointConfig {
        checkpoint_dir,
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Only post-checkpoint nodes (21-30) should be present
    // Pre-checkpoint state is not recovered because checkpoint stores only metadata
    assert_eq!(
        current.node_count(),
        10,
        "Only post-checkpoint nodes recovered"
    );

    // Pre-checkpoint nodes should not exist
    assert!(
        current.get_node(NodeId::new(5).unwrap()).is_err(),
        "Pre-checkpoint node should not exist"
    );

    // Newly created nodes (post-checkpoint) should be present
    let node25 = current.get_node(NodeId::new(25).unwrap())?;
    assert!(node25.has_label_str("NewNode"));
    assert!(matches!(
        node25.properties.get("value"),
        Some(PropertyValue::Int(25))
    ));

    // Historical versions: only the 10 post-checkpoint creates
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 10);

    Ok(())
}

// ============================================================================
// Scenario 3: Crash During WAL Write (Corrupt/Truncated Entry)
// ============================================================================

#[test]
fn test_crash_with_truncated_wal() -> Result<()> {
    // Given: Create nodes, then truncate WAL mid-entry to simulate crash during write
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create 10 nodes successfully
    for i in 1..=10 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("SafeNode").unwrap(),
            properties: PropertyMapBuilder::new().insert("index", i as i64).build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // Get the current WAL size - use helper for deterministic file ordering
    let wal_files = find_wal_files(&wal_dir)?;
    assert!(!wal_files.is_empty(), "WAL files should exist");

    // Find the latest WAL file (last in sorted order)
    let wal_file_path = wal_files.last().expect("WAL files should exist");
    let original_size = std::fs::metadata(wal_file_path)?.len();

    // Add one more operation that we'll truncate
    wal.append(WalOperation::CreateNode {
        node_id: NodeId::new(11).unwrap(),
        label: GLOBAL_INTERNER.intern("TruncatedNode").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("index", 11_i64)
            .insert("will_be", "truncated")
            .build(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;
    wal.flush()?;

    // Truncate the WAL to remove the last entry (simulate crash mid-write)
    // We truncate to a point in the middle of the last entry
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(wal_file_path)?;
    let partial_size = original_size + 10; // Truncate to partial entry
    file.set_len(partial_size)?;
    drop(file);

    // When: Recover from truncated WAL
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let wal_config2 = ConcurrentWalSystemConfig::new(wal_dir);
    let wal2 = ConcurrentWalSystem::new(wal_config2)?;
    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal2)?;

    // Then: Only the 10 complete nodes should be recovered
    // The truncated 11th node should be skipped
    assert_eq!(
        current.node_count(),
        10,
        "Only 10 complete nodes should be recovered, truncated entry skipped"
    );

    // Verify the nodes that were recovered
    for i in 1..=10 {
        let node = current.get_node(NodeId::new(i).unwrap())?;
        assert!(node.has_label_str("SafeNode"));
    }

    // The 11th node should not exist
    assert!(
        current.get_node(NodeId::new(11).unwrap()).is_err(),
        "Truncated node should not be recovered"
    );

    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 10);

    Ok(())
}

#[test]
fn test_crash_with_corrupted_wal_header() -> Result<()> {
    // Given: Create WAL with corrupted magic header
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    // First create a valid WAL
    let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
    let wal = ConcurrentWalSystem::new(wal_config)?;

    for i in 1..=5 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Node").unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // Find and corrupt the WAL file header - use helper for deterministic ordering
    let wal_files = find_wal_files(&wal_dir)?;
    let wal_file_path = wal_files.last().expect("WAL files should exist");

    // Corrupt the magic header
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .read(true)
        .open(wal_file_path)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(b"BADD")?; // Replace "GWAL" with "BADD"
    drop(file);

    // When: Try to recover
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let wal_config2 = ConcurrentWalSystemConfig::new(wal_dir);
    let wal2 = ConcurrentWalSystem::new(wal_config2)?;
    let mut manager = PersistenceManager::new(config)?;
    let result = manager.recover(&wal2);

    // Then: Recovery should fail with corrupted data error
    assert!(
        result.is_err(),
        "Recovery should fail with corrupted header"
    );
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("Expected error but got Ok"),
    };
    let err_str = err.to_string();
    assert!(
        err_str.contains("Invalid WAL segment") || err_str.contains("corrupt"),
        "Error should indicate corruption: {}",
        err_str
    );

    Ok(())
}

// ============================================================================
// Scenario 4: Multiple Crashes
// ============================================================================

#[test]
fn test_multiple_crash_recovery_cycles() -> Result<()> {
    // Given: Sequential create → crash → recover cycles
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let checkpoint_dir = temp_dir.path().join("checkpoints");

    // Cycle 1: Create 20 nodes, "crash", recover
    {
        let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        for i in 1..=20 {
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i).unwrap(),
                label: GLOBAL_INTERNER.intern("Cycle1").unwrap(),
                properties: PropertyMapBuilder::new().insert("cycle", 1_i64).build(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }
        wal.flush()?;

        // Recover and verify
        let config = CheckpointConfig {
            checkpoint_dir: checkpoint_dir.clone(),
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;
        let (current, _, _) = manager.recover(&wal)?;
        assert_eq!(current.node_count(), 20, "Cycle 1: 20 nodes");
    }

    // Cycle 2: Add 20 more nodes, "crash", recover
    {
        let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        for i in 21..=40 {
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i).unwrap(),
                label: GLOBAL_INTERNER.intern("Cycle2").unwrap(),
                properties: PropertyMapBuilder::new().insert("cycle", 2_i64).build(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }
        wal.flush()?;

        // Recover and verify
        let config = CheckpointConfig {
            checkpoint_dir: checkpoint_dir.clone(),
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;
        let (current, _, _) = manager.recover(&wal)?;
        assert_eq!(current.node_count(), 40, "Cycle 2: 40 total nodes");
    }

    // Cycle 3: Add 20 more nodes with some updates
    {
        let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        for i in 41..=60 {
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i).unwrap(),
                label: GLOBAL_INTERNER.intern("Cycle3").unwrap(),
                properties: PropertyMapBuilder::new().insert("cycle", 3_i64).build(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }

        // Update some existing nodes
        for i in 1..=5 {
            wal.append(WalOperation::UpdateNode {
                node_id: NodeId::new(i).unwrap(),
                version_id: VersionId::new(60 + i).unwrap(),
                label: GLOBAL_INTERNER.intern("UpdatedCycle1").unwrap(),
                properties: PropertyMapBuilder::new()
                    .insert("cycle", 1_i64)
                    .insert("updated", true)
                    .build(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }
        wal.flush()?;

        // Final recovery and verification
        let config = CheckpointConfig {
            checkpoint_dir: checkpoint_dir.clone(),
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;
        let (current, historical, _) = manager.recover(&wal)?;

        // Then: All 60 nodes should be present
        assert_eq!(current.node_count(), 60, "Final: 60 total nodes");

        // Updated nodes should have correct state
        let node1 = current.get_node(NodeId::new(1).unwrap())?;
        assert!(node1.has_label_str("UpdatedCycle1"));
        assert!(matches!(
            node1.properties.get("updated"),
            Some(PropertyValue::Bool(true))
        ));

        // Non-updated nodes should retain original labels
        let node10 = current.get_node(NodeId::new(10).unwrap())?;
        assert!(node10.has_label_str("Cycle1"));

        let node30 = current.get_node(NodeId::new(30).unwrap())?;
        assert!(node30.has_label_str("Cycle2"));

        let node50 = current.get_node(NodeId::new(50).unwrap())?;
        assert!(node50.has_label_str("Cycle3"));

        // Historical versions: 60 creates + 5 updates = 65
        let hist_stats = historical.stats();
        assert_eq!(hist_stats.total_node_versions, 65);
    }

    Ok(())
}

#[test]
fn test_crash_recovery_with_deletions_across_cycles() -> Result<()> {
    // Given: Create → delete → crash → recover → create more → crash → recover
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let checkpoint_dir = temp_dir.path().join("checkpoints");

    // Cycle 1: Create 10 nodes, delete 5
    {
        let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        for i in 1..=10 {
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i).unwrap(),
                label: GLOBAL_INTERNER.intern("Node").unwrap(),
                properties: PropertyMap::new(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }

        // Delete nodes 2, 4, 6, 8, 10
        for i in [2, 4, 6, 8, 10] {
            wal.append(WalOperation::DeleteNode {
                node_id: NodeId::new(i).unwrap(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }
        wal.flush()?;

        let config = CheckpointConfig {
            checkpoint_dir: checkpoint_dir.clone(),
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;
        let (current, _, _) = manager.recover(&wal)?;

        // 5 nodes remaining (1, 3, 5, 7, 9)
        assert_eq!(current.node_count(), 5);
    }

    // Cycle 2: Create new nodes in place of deleted ones
    {
        let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
        let wal = ConcurrentWalSystem::new(wal_config)?;

        // Create nodes 11-15 (new IDs)
        for i in 11..=15 {
            wal.append(WalOperation::CreateNode {
                node_id: NodeId::new(i).unwrap(),
                label: GLOBAL_INTERNER.intern("NewNode").unwrap(),
                properties: PropertyMap::new(),
                temporal: BiTemporalInterval::current(time::now()),
            })?;
        }
        wal.flush()?;

        let config = CheckpointConfig {
            checkpoint_dir: checkpoint_dir.clone(),
            ..Default::default()
        };
        let mut manager = PersistenceManager::new(config)?;
        let (current, historical, _) = manager.recover(&wal)?;

        // Then: 10 nodes total (5 original + 5 new)
        assert_eq!(current.node_count(), 10);

        // Deleted nodes should not exist
        assert!(current.get_node(NodeId::new(2).unwrap()).is_err());
        assert!(current.get_node(NodeId::new(4).unwrap()).is_err());

        // Original survivors should exist
        assert!(current.get_node(NodeId::new(1).unwrap()).is_ok());
        assert!(current.get_node(NodeId::new(3).unwrap()).is_ok());

        // New nodes should exist
        assert!(current.get_node(NodeId::new(11).unwrap()).is_ok());
        assert!(current.get_node(NodeId::new(15).unwrap()).is_ok());

        // Historical: 10 creates + 5 deletes + 5 creates = 20 versions
        let hist_stats = historical.stats();
        assert_eq!(hist_stats.total_node_versions, 20);
    }

    Ok(())
}

// ============================================================================
// Scenario 5: Complex Workflow
// ============================================================================

#[test]
fn test_complex_workflow_create_update_delete() -> Result<()> {
    // Given: Complex workflow with creates, updates, deletes
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let mut next_version_id = 1_u64;

    // Step 1: Create initial nodes (1-10)
    for i in 1..=10 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("name", format!("Person{}", i))
                .insert("age", (20 + i) as i64)
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        next_version_id += 1;
    }

    // Step 2: Update nodes 1-5 (change age)
    for i in 1..=5 {
        wal.append(WalOperation::UpdateNode {
            node_id: NodeId::new(i).unwrap(),
            version_id: VersionId::new(next_version_id).unwrap(),
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("name", format!("Person{}", i))
                .insert("age", (30 + i) as i64)
                .insert("updated", true)
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
        next_version_id += 1;
    }

    // Step 3: Delete nodes 3 and 4
    for i in [3, 4] {
        wal.append(WalOperation::DeleteNode {
            node_id: NodeId::new(i).unwrap(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    // Step 4: Create new nodes 11-15
    for i in 11..=15 {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Employee").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("name", format!("Employee{}", i))
                .insert("department", "Engineering")
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    // Step 5: Create edges between employees
    for i in 11..=14 {
        wal.append(WalOperation::CreateEdge {
            edge_id: EdgeId::new(i - 10).unwrap(),
            source: NodeId::new(i).unwrap(),
            target: NodeId::new(i + 1).unwrap(),
            label: GLOBAL_INTERNER.intern("WORKS_WITH").unwrap(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    wal.flush()?;

    // When: Recover
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Verify current state

    // 13 nodes total: 10 created - 2 deleted + 5 new = 13
    assert_eq!(current.node_count(), 13);

    // 4 edges
    assert_eq!(current.edge_count(), 4);

    // Deleted nodes should not exist
    assert!(current.get_node(NodeId::new(3).unwrap()).is_err());
    assert!(current.get_node(NodeId::new(4).unwrap()).is_err());

    // Updated nodes should have new values
    let node1 = current.get_node(NodeId::new(1).unwrap())?;
    assert!(matches!(
        node1.properties.get("age"),
        Some(PropertyValue::Int(31)) // 30 + 1
    ));
    assert!(matches!(
        node1.properties.get("updated"),
        Some(PropertyValue::Bool(true))
    ));

    // Non-updated nodes should have original values
    let node6 = current.get_node(NodeId::new(6).unwrap())?;
    assert!(matches!(
        node6.properties.get("age"),
        Some(PropertyValue::Int(26)) // 20 + 6
    ));
    assert!(node6.properties.get("updated").is_none());

    // New nodes should be present
    let node11 = current.get_node(NodeId::new(11).unwrap())?;
    assert!(node11.has_label_str("Employee"));
    assert!(matches!(
        node11.properties.get("department"),
        Some(PropertyValue::String(s)) if s.as_ref() == "Engineering"
    ));

    // Verify historical versions
    // 10 creates + 5 updates + 2 delete tombstones + 5 creates = 22 node versions
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 22);
    assert_eq!(hist_stats.total_edge_versions, 4);

    Ok(())
}

#[test]
fn test_complex_workflow_with_edges_and_updates() -> Result<()> {
    // Given: Create graph with updates to both nodes and edges
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create nodes
    wal.append(WalOperation::CreateNode {
        node_id: NodeId::new(1).unwrap(),
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new().insert("name", "Alice").build(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    wal.append(WalOperation::CreateNode {
        node_id: NodeId::new(2).unwrap(),
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new().insert("name", "Bob").build(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    wal.append(WalOperation::CreateNode {
        node_id: NodeId::new(3).unwrap(),
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new().insert("name", "Charlie").build(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Create edges
    wal.append(WalOperation::CreateEdge {
        edge_id: EdgeId::new(1).unwrap(),
        source: NodeId::new(1).unwrap(),
        target: NodeId::new(2).unwrap(),
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMapBuilder::new().insert("since", 2020_i64).build(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    wal.append(WalOperation::CreateEdge {
        edge_id: EdgeId::new(2).unwrap(),
        source: NodeId::new(2).unwrap(),
        target: NodeId::new(3).unwrap(),
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMapBuilder::new().insert("since", 2021_i64).build(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Update edge 1
    wal.append(WalOperation::UpdateEdge {
        edge_id: EdgeId::new(1).unwrap(),
        version_id: VersionId::new(6).unwrap(),
        label: GLOBAL_INTERNER.intern("FRIENDS_WITH").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("since", 2020_i64)
            .insert("strength", "strong")
            .build(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Delete node 3 (should not affect edge 2 in current storage model)
    wal.append(WalOperation::DeleteNode {
        node_id: NodeId::new(3).unwrap(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Delete edge 2
    wal.append(WalOperation::DeleteEdge {
        edge_id: EdgeId::new(2).unwrap(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    wal.flush()?;

    // When: Recover
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: Verify state
    assert_eq!(current.node_count(), 2); // Alice, Bob (Charlie deleted)
    assert_eq!(current.edge_count(), 1); // Only edge 1 (edge 2 deleted)

    // Edge 1 should have updated label and properties
    let edge1 = current.get_edge(EdgeId::new(1).unwrap())?;
    assert!(edge1.has_label_str("FRIENDS_WITH"));
    assert!(matches!(
        edge1.properties.get("strength"),
        Some(PropertyValue::String(s)) if s.as_ref() == "strong"
    ));

    // Historical versions:
    // - 3 node creates + 1 node delete tombstone = 4 node versions
    // - 2 edge creates + 1 edge update + 1 edge delete tombstone = 4 edge versions
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 4);
    assert_eq!(hist_stats.total_edge_versions, 4);

    Ok(())
}

#[test]
fn test_complex_workflow_with_vector_properties() -> Result<()> {
    // Given: Create nodes with vector embeddings, update some, delete some
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    // Create documents with embeddings
    for i in 1..=5 {
        let embedding: Vec<f32> = (0..128)
            .map(|j| (i as f32 * 0.1) + (j as f32 * 0.001))
            .collect();
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern("Document").unwrap(),
            properties: PropertyMapBuilder::new()
                .insert("title", format!("Doc{}", i))
                .insert_vector("embedding", &embedding)
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }

    // Update document 1's embedding
    let new_embedding: Vec<f32> = (0..128).map(|j| 0.5 + (j as f32 * 0.002)).collect();
    wal.append(WalOperation::UpdateNode {
        node_id: NodeId::new(1).unwrap(),
        version_id: VersionId::new(6).unwrap(),
        label: GLOBAL_INTERNER.intern("Document").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("title", "Doc1 - Updated")
            .insert_vector("embedding", &new_embedding)
            .build(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    // Delete document 3
    wal.append(WalOperation::DeleteNode {
        node_id: NodeId::new(3).unwrap(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;

    wal.flush()?;

    // When: Recover
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    // Then: 4 documents present
    assert_eq!(current.node_count(), 4);

    // Document 1 should have updated embedding
    let doc1 = current.get_node(NodeId::new(1).unwrap())?;
    assert!(matches!(
        doc1.properties.get("title"),
        Some(PropertyValue::String(s)) if s.as_ref() == "Doc1 - Updated"
    ));
    if let Some(PropertyValue::Vector(vec)) = doc1.properties.get("embedding") {
        assert_eq!(vec.len(), 128);
        // First element should be 0.5 from new embedding
        assert!((vec[0] - 0.5).abs() < 0.001);
    } else {
        panic!("Expected vector property");
    }

    // Document 3 should not exist
    assert!(current.get_node(NodeId::new(3).unwrap()).is_err());

    // Document 2 should have original embedding
    let doc2 = current.get_node(NodeId::new(2).unwrap())?;
    if let Some(PropertyValue::Vector(vec)) = doc2.properties.get("embedding") {
        // First element should be 0.2 (2 * 0.1)
        assert!((vec[0] - 0.2).abs() < 0.001);
    }

    // Historical: 5 creates + 1 update + 1 delete = 7 versions
    let hist_stats = historical.stats();
    assert_eq!(hist_stats.total_node_versions, 7);

    Ok(())
}
