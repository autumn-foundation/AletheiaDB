//! Tests for CheckpointManager with index persistence recovery (Issue #294 extension)
//!
//! These tests verify that the checkpoint system correctly:
//! - Persists full graph and temporal state to disk
//! - Recovers database state from persisted indexes
//! - Correctly replays WAL entries after checkpoint LSN
//! - Maintains LSN consistency across checkpoint and recovery

use aletheiadb::{
    GLOBAL_INTERNER, PropertyMapBuilder,
    core::error::Result,
    core::{
        graph::Node,
        id::{EdgeId, NodeId, VersionId},
        property::PropertyMap,
        temporal::time,
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
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: props,
            valid_from: time::now(),
            provenance: None,
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
            label: GLOBAL_INTERNER.intern("Test").unwrap(),
            properties: props,
            valid_from: time::now(),
            provenance: None,
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
            label: GLOBAL_INTERNER.intern("Test").unwrap(),
            properties: props,
            valid_from: time::now(),
            provenance: None,
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

    // Create WAL first so we have a valid LSN for the checkpoint
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

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
        // Use LSN(0) which is valid for an empty WAL
        manager.create_checkpoint(LSN(0), &current, &historical)?;
    }

    // When: Recover using the same WAL
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

    // Create WAL first so we have a valid LSN for the checkpoint
    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

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
        // Use LSN(0) which is valid for an empty WAL
        let stats = manager.create_checkpoint(LSN(0), &current, &historical)?;

        // Verify checkpoint captured the nodes
        assert_eq!(stats.node_count, 5);
    }

    // When: Recover and create new node using the same WAL
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
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new().insert("name", "Alice").build(),
        valid_from: time::now(),
        provenance: None,
    })?;

    // Update node
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(2)?,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("name", "Alice Updated")
            .build(),
        valid_from: time::now(),
        provenance: None,
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
            label: GLOBAL_INTERNER.intern("Test").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
            provenance: None,
        })?;
    }

    // Delete node 2
    wal.append(WalOperation::DeleteNode {
        node_id: NodeId::new(2)?,
        valid_from: time::now(),
        version_id: None,
        provenance: None,
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

// ============================================================================
// Valid-Time Retraction x Checkpoint Interplay (Issue #3230, fix-round #7)
// ============================================================================

/// retract -> checkpoint -> restart/recover: the closed valid interval and
/// the retraction version must survive a recovery that restores state from
/// the checkpoint alone (no WAL entries left to replay past the checkpoint).
#[test]
fn test_retraction_then_checkpoint_survives_recovery() -> Result<()> {
    use aletheiadb::core::hlc::HybridTimestamp;

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1)?;
    let now = time::now().wallclock();
    let valid_from = HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();
    let valid_to = HybridTimestamp::new(now - 1_800_000_000, 0).unwrap();

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("RetractCkpt").unwrap(),
        properties: PropertyMapBuilder::new().insert("name", "Alice").build(),
        valid_from,
        provenance: None,
    })?;
    wal.append(WalOperation::RetractNode {
        node_id,
        valid_to,
        version_id: None,
        provenance: None,
    })?;
    wal.flush()?;

    // First recovery replays the create + retraction, then we checkpoint
    // the post-retraction state at the WAL head.
    let checkpoint_lsn = LSN(wal.current_lsn().0.saturating_sub(1));
    {
        let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;
        let (current, historical, _final_lsn) = manager.recover(&wal)?;
        assert!(
            current.get_node(node_id).is_err(),
            "retracted before checkpoint"
        );
        manager.create_checkpoint(checkpoint_lsn, &current, &historical)?;
    }

    // Simulated restart: a fresh manager restores from the checkpoint with
    // nothing left to replay after the checkpoint LSN.
    let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;
    assert!(manager.has_persisted_state());
    let (recovered_current, recovered_historical, _final_lsn) = manager.recover(&wal)?;

    // Current state: still absent.
    assert!(recovered_current.get_node(node_id).is_err());

    // The retraction version's closed interval [valid_from, valid_to)
    // survived the checkpoint round trip.
    let head_id = recovered_historical
        .get_current_node_version(node_id)
        .expect("retraction version must survive the checkpoint");
    let head = recovered_historical.get_node_version(head_id).unwrap();
    assert_eq!(head.temporal.valid_time().start(), valid_from);
    assert_eq!(
        head.temporal.valid_time().end(),
        valid_to,
        "closed valid interval must survive checkpoint + recovery"
    );

    // Both versions (create + retraction) survived as temporal metadata.
    let stats = recovered_historical.stats();
    assert_eq!(
        stats.total_node_versions, 2,
        "create + retraction versions must both survive the checkpoint"
    );

    // Full bi-temporal fidelity across a restore-only recovery (version
    // chain links, transaction-time closures, delta reconstruction, and
    // AS OF SYSTEM_TIME point reads) is covered by the Issue #3387 tests
    // below (`test_checkpoint_restore_only_preserves_*_bitemporal_fidelity`);
    // the WAL-replay path is covered by
    // `test_retraction_replayed_after_checkpoint` below and
    // `retraction_survives_wal_replay_crash_recovery` in `src/db/ops.rs`.

    Ok(())
}

/// checkpoint -> retract -> recover: a RetractNode WAL entry appended AFTER
/// the checkpoint must replay on top of the checkpoint-restored state,
/// honoring the logged valid_to.
#[test]
fn test_retraction_replayed_after_checkpoint() -> Result<()> {
    use aletheiadb::core::hlc::HybridTimestamp;

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1)?;
    let now = time::now().wallclock();
    let valid_from = HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();
    let valid_to = HybridTimestamp::new(now - 1_800_000_000, 0).unwrap();

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("RetractCkptReplay").unwrap(),
        properties: PropertyMapBuilder::new().insert("name", "Bob").build(),
        valid_from,
        provenance: None,
    })?;
    wal.flush()?;

    // Checkpoint the PRE-retraction state (node present).
    let checkpoint_lsn = LSN(wal.current_lsn().0.saturating_sub(1));
    {
        let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;
        let (current, historical, _final_lsn) = manager.recover(&wal)?;
        assert!(
            current.get_node(node_id).is_ok(),
            "present before checkpoint"
        );
        manager.create_checkpoint(checkpoint_lsn, &current, &historical)?;
    }

    // The retraction lands in the WAL after the checkpoint.
    wal.append(WalOperation::RetractNode {
        node_id,
        valid_to,
        version_id: None,
        provenance: None,
    })?;
    wal.flush()?;

    // Recovery loads the checkpoint, then replays ONLY the retraction.
    let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;
    let (recovered_current, recovered_historical, _final_lsn) = manager.recover(&wal)?;

    assert!(
        recovered_current.get_node(node_id).is_err(),
        "retraction replayed from the checkpoint must remove the node from current state"
    );

    let head_id = recovered_historical
        .get_current_node_version(node_id)
        .expect("retraction version must exist after replay-from-checkpoint");
    let head = recovered_historical.get_node_version(head_id).unwrap();
    assert_eq!(head.temporal.valid_time().start(), valid_from);
    assert_eq!(
        head.temporal.valid_time().end(),
        valid_to,
        "replay-from-checkpoint must honor the logged valid_to"
    );

    let probe_before = HybridTimestamp::new(now - 2_700_000_000, 0).unwrap();
    assert!(
        recovered_historical
            .get_node_at_time(node_id, probe_before, time::now())
            .is_ok()
    );
    assert!(
        recovered_historical
            .get_node_at_time(node_id, valid_to, time::now())
            .is_err()
    );

    Ok(())
}

// ============================================================================
// Checkpoint Bi-Temporal Fidelity (Issue #3387)
// ============================================================================
//
// Checkpoint restore must round-trip transaction-time interval closures and
// version chain links so a recovery that restores state from the checkpoint
// ALONE (no WAL entries left to replay past the checkpoint LSN) serves the
// exact same full bi-temporal reads as the pre-checkpoint state:
// `get_node_history` / `get_edge_history` must return the identical version
// chain (including reconstructed delta properties), and AS OF SYSTEM_TIME
// point reads positioned between a version's creation and its supersession
// must resolve to the same version.

/// Assert two entity histories are identical version-by-version: same version
/// ids in the same order, same bi-temporal intervals (including closed
/// transaction-time ends), same reconstructed properties, same labels.
fn assert_history_identical(
    pre: &aletheiadb::core::history::EntityHistory,
    post: &aletheiadb::core::history::EntityHistory,
    ctx: &str,
) {
    assert_eq!(
        pre.version_count(),
        post.version_count(),
        "{ctx}: version count must survive a restore-only recovery \
         (chain links lost => history truncated to the head version)"
    );
    for (a, b) in pre.versions.iter().zip(post.versions.iter()) {
        let vctx = format!("{ctx}: version_id {}", a.version_id.as_u64());
        assert_eq!(a.version_id, b.version_id, "{vctx}: version id");
        assert_eq!(
            a.temporal, b.temporal,
            "{vctx}: bi-temporal interval (incl. tx-time closure) must round-trip"
        );
        assert_eq!(
            a.properties, b.properties,
            "{vctx}: reconstructed properties must round-trip"
        );
        assert_eq!(a.label, b.label, "{vctx}: label");
    }
}

/// create -> update -> retract -> checkpoint -> restore-only recovery:
/// node history and AS OF SYSTEM_TIME reads must be identical to
/// pre-checkpoint (Issue #3387).
#[test]
fn test_checkpoint_restore_only_preserves_node_bitemporal_fidelity() -> Result<()> {
    use aletheiadb::core::hlc::HybridTimestamp;

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1)?;
    let now = time::now().wallclock();
    let valid_from = HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();
    let valid_to = HybridTimestamp::new(now - 1_800_000_000, 0).unwrap();

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("CkptFidelityNode").unwrap(),
        properties: PropertyMapBuilder::new().insert("name", "Alice").build(),
        valid_from,
        provenance: None,
    })?;
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(500)?,
        label: GLOBAL_INTERNER.intern("CkptFidelityNode").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("name", "Alice Updated")
            .insert("role", "admin")
            .build(),
        valid_from,
        provenance: None,
    })?;
    wal.append(WalOperation::RetractNode {
        node_id,
        valid_to,
        version_id: None,
        provenance: None,
    })?;
    wal.flush()?;

    // First recovery replays the full WAL (the known-good path), giving the
    // reference bi-temporal state we checkpoint and then must reproduce.
    let checkpoint_lsn = LSN(wal.current_lsn().0.saturating_sub(1));
    let pre_history;
    let pre_probes: Vec<(
        HybridTimestamp,
        u64,
        aletheiadb::core::property::PropertyMap,
    )>;
    {
        let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;
        let (current, historical, _final_lsn) = manager.recover(&wal)?;

        pre_history = historical.get_node_history(node_id)?;
        assert_eq!(
            pre_history.version_count(),
            3,
            "precondition: create + update + retraction versions"
        );
        // Preconditions that make the fidelity comparison meaningful: the two
        // superseded versions have CLOSED transaction time, the head is open.
        assert!(
            !pre_history.versions[0]
                .temporal
                .transaction_time()
                .is_current(),
            "precondition: create version tx-time closed by the update"
        );
        assert!(
            !pre_history.versions[1]
                .temporal
                .transaction_time()
                .is_current(),
            "precondition: update version tx-time closed by the retraction"
        );
        assert!(
            pre_history.versions[2]
                .temporal
                .transaction_time()
                .is_current(),
            "precondition: retraction (head) version tx-time open"
        );
        assert_eq!(
            pre_history.versions[0]
                .properties
                .get("name")
                .and_then(|v| v.as_str().map(str::to_string)),
            Some("Alice".to_string()),
        );
        assert_eq!(
            pre_history.versions[1]
                .properties
                .get("name")
                .and_then(|v| v.as_str().map(str::to_string)),
            Some("Alice Updated".to_string()),
        );

        // AS OF SYSTEM_TIME probes at each version's tx start: positioned
        // exactly inside [creation, supersession) of each version.
        pre_probes = pre_history
            .versions
            .iter()
            .map(|v| {
                let tx = v.temporal.transaction_time().start();
                let node = historical
                    .get_node_at_time(node_id, valid_from, tx)
                    .expect("pre-checkpoint AS OF SYSTEM_TIME probe must resolve");
                (tx, v.version_id.as_u64(), node.properties)
            })
            .collect();

        manager.create_checkpoint(checkpoint_lsn, &current, &historical)?;
    }

    // Simulated restart: restore from the checkpoint with NOTHING left to
    // replay after the checkpoint LSN (restore-only recovery). Assert that
    // explicitly, so an off-by-one silently replaying the retraction cannot
    // weaken this test into a WAL-replay test.
    assert!(
        wal.read_from(checkpoint_lsn.next())?.is_empty(),
        "restore-only precondition: no WAL entries past the checkpoint LSN"
    );
    let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;
    assert!(manager.has_persisted_state());
    let (_recovered_current, recovered_historical, _final_lsn) = manager.recover(&wal)?;

    // (1) Full history identical: chain links + tx-time closures round-trip.
    let post_history = recovered_historical.get_node_history(node_id)?;
    assert_history_identical(&pre_history, &post_history, "node history");

    // (2) AS OF SYSTEM_TIME point reads identical to pre-checkpoint.
    for (tx, _expected_vid, expected_props) in &pre_probes {
        let node = recovered_historical
            .get_node_at_time(node_id, valid_from, *tx)
            .unwrap_or_else(|e| {
                panic!(
                    "AS OF SYSTEM_TIME probe at tx {tx:?} must resolve after \
                     restore-only recovery: {e}"
                )
            });
        assert_eq!(
            &node.properties, expected_props,
            "AS OF SYSTEM_TIME probe at tx {tx:?} must return the same state \
             as before the checkpoint"
        );
    }

    // (3) The mid-history probe specifically returns the SUPERSEDED state,
    // not the head: this is the read the dropped tx-time closure corrupts.
    let (tx_create, _, _) = pre_probes[0];
    let node_at_create = recovered_historical.get_node_at_time(node_id, valid_from, tx_create)?;
    assert_eq!(
        node_at_create
            .properties
            .get("name")
            .and_then(|v| v.as_str().map(str::to_string)),
        Some("Alice".to_string()),
        "probe between create and update must see the pre-update state"
    );

    // (4) Valid-time retraction still enforced at the head.
    let head_tx = pre_probes[2].0;
    assert!(
        recovered_historical
            .get_node_at_time(node_id, valid_to, head_tx)
            .is_err(),
        "valid-time probe at/after valid_to must not resolve"
    );

    Ok(())
}

/// Edge mirror of the node fidelity test: create -> update -> retract an
/// edge, checkpoint, restore-only recovery, assert `get_edge_history` and
/// AS OF SYSTEM_TIME reads identical to pre-checkpoint (Issue #3387).
#[test]
fn test_checkpoint_restore_only_preserves_edge_bitemporal_fidelity() -> Result<()> {
    use aletheiadb::core::hlc::HybridTimestamp;

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let source = NodeId::new(1)?;
    let target = NodeId::new(2)?;
    let edge_id = EdgeId::new(1)?;
    let now = time::now().wallclock();
    let valid_from = HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();
    let valid_to = HybridTimestamp::new(now - 1_800_000_000, 0).unwrap();

    for node_id in [source, target] {
        wal.append(WalOperation::CreateNode {
            node_id,
            label: GLOBAL_INTERNER.intern("CkptFidelityEdgeNode").unwrap(),
            properties: PropertyMap::new(),
            valid_from,
            provenance: None,
        })?;
    }
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source,
        target,
        label: GLOBAL_INTERNER.intern("CKPT_FIDELITY_KNOWS").unwrap(),
        properties: PropertyMapBuilder::new().insert("weight", 1i64).build(),
        valid_from,
        provenance: None,
    })?;
    wal.append(WalOperation::UpdateEdge {
        edge_id,
        version_id: VersionId::new(600)?,
        label: GLOBAL_INTERNER.intern("CKPT_FIDELITY_KNOWS").unwrap(),
        properties: PropertyMapBuilder::new().insert("weight", 2i64).build(),
        valid_from,
        provenance: None,
    })?;
    wal.append(WalOperation::RetractEdge {
        edge_id,
        valid_to,
        version_id: None,
        provenance: None,
    })?;
    wal.flush()?;

    let checkpoint_lsn = LSN(wal.current_lsn().0.saturating_sub(1));
    let pre_history;
    let pre_probes: Vec<(HybridTimestamp, aletheiadb::core::property::PropertyMap)>;
    {
        let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;
        let (current, historical, _final_lsn) = manager.recover(&wal)?;

        pre_history = historical.get_edge_history(edge_id)?;
        assert_eq!(
            pre_history.version_count(),
            3,
            "precondition: create + update + retraction edge versions"
        );
        assert!(
            !pre_history.versions[0]
                .temporal
                .transaction_time()
                .is_current(),
            "precondition: create version tx-time closed by the update"
        );
        assert!(
            !pre_history.versions[1]
                .temporal
                .transaction_time()
                .is_current(),
            "precondition: update version tx-time closed by the retraction"
        );

        pre_probes = pre_history
            .versions
            .iter()
            .map(|v| {
                let tx = v.temporal.transaction_time().start();
                let edge = historical
                    .get_edge_at_time(edge_id, valid_from, tx)
                    .expect("pre-checkpoint AS OF SYSTEM_TIME edge probe must resolve");
                (tx, edge.properties)
            })
            .collect();

        manager.create_checkpoint(checkpoint_lsn, &current, &historical)?;
    }

    // Restore-only precondition (see the node fidelity test above).
    assert!(
        wal.read_from(checkpoint_lsn.next())?.is_empty(),
        "restore-only precondition: no WAL entries past the checkpoint LSN"
    );
    let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;
    let (_recovered_current, recovered_historical, _final_lsn) = manager.recover(&wal)?;

    let post_history = recovered_historical.get_edge_history(edge_id)?;
    assert_history_identical(&pre_history, &post_history, "edge history");

    for (tx, expected_props) in &pre_probes {
        let edge = recovered_historical
            .get_edge_at_time(edge_id, valid_from, *tx)
            .unwrap_or_else(|e| {
                panic!(
                    "AS OF SYSTEM_TIME edge probe at tx {tx:?} must resolve after \
                     restore-only recovery: {e}"
                )
            });
        assert_eq!(
            &edge.properties, expected_props,
            "AS OF SYSTEM_TIME edge probe at tx {tx:?} must return the same \
             state as before the checkpoint"
        );
    }

    // Mid-history probe returns the superseded weight, not the head's.
    let (tx_create, _) = pre_probes[0];
    let edge_at_create = recovered_historical.get_edge_at_time(edge_id, valid_from, tx_create)?;
    assert_eq!(
        edge_at_create
            .properties
            .get("weight")
            .and_then(|v| v.as_int()),
        Some(1),
        "probe between create and update must see the pre-update state"
    );

    Ok(())
}

/// Issue #3387 availability regression: a routine small embedding edit
/// produces a `VectorDelta::Sparse` in hot history (changed*2 < dim); the
/// checkpoint must MATERIALIZE it in the persisted copy (not hard-fail,
/// which would stall checkpoints and WAL truncation for vector workloads)
/// and the vector must round-trip a restore-only recovery.
#[test]
fn test_checkpoint_materializes_sparse_vector_delta_and_round_trips() -> Result<()> {
    use aletheiadb::core::version::{VectorDelta, VersionData};

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1)?;
    let dim = 128usize;
    let base_vec: Vec<f32> = (0..dim).map(|i| i as f32 * 0.01).collect();
    let mut updated_vec = base_vec.clone();
    updated_vec[3] = 42.0; // 1 change * 2 < 128 -> sparse delta

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("SparseVecNode").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("name", "doc")
            .insert_vector("embedding", &base_vec)
            .build(),
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(800)?,
        label: GLOBAL_INTERNER.intern("SparseVecNode").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("name", "doc")
            .insert_vector("embedding", &updated_vec)
            .build(),
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.flush()?;

    let checkpoint_lsn = LSN(wal.current_lsn().0.saturating_sub(1));
    {
        let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;
        let (current, historical, _final_lsn) = manager.recover(&wal)?;

        // Precondition: the update really produced a SPARSE vector delta in
        // hot history -- otherwise this test would not exercise
        // materialization at all.
        let head_id = historical
            .get_current_node_version(node_id)
            .expect("head version");
        let head = historical.get_node_version(head_id).unwrap();
        let VersionData::Delta { delta } = &head.data else {
            panic!("precondition: update version must be a delta");
        };
        assert!(
            delta
                .vector_deltas
                .values()
                .any(|d| matches!(d, VectorDelta::Sparse { .. })),
            "precondition: small embedding edit must produce VectorDelta::Sparse"
        );

        // The checkpoint must succeed (pre-fix: hard error demanding
        // materialize_vector_deltas, stalling checkpoints persistently).
        manager.create_checkpoint(checkpoint_lsn, &current, &historical)?;

        // The LIVE in-memory version keeps its sparse delta untouched.
        let head_after = historical.get_node_version(head_id).unwrap();
        let VersionData::Delta { delta } = &head_after.data else {
            panic!("live version must remain a delta");
        };
        assert!(
            delta
                .vector_deltas
                .values()
                .any(|d| matches!(d, VectorDelta::Sparse { .. })),
            "checkpoint must not mutate live in-memory state"
        );
    }

    // Restore-only recovery: the updated vector must round-trip.
    let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;
    let (_current, restored, _final_lsn) = manager.recover(&wal)?;

    let history = restored.get_node_history(node_id)?;
    assert_eq!(history.version_count(), 2);
    let restored_updated = history.versions[1]
        .properties
        .get("embedding")
        .and_then(|v| v.as_vector().map(<[f32]>::to_vec))
        .expect("restored head must carry the embedding");
    assert_eq!(
        restored_updated, updated_vec,
        "sparse vector delta must be materialized into the checkpoint and \
         reconstruct to the updated embedding after restore"
    );
    let restored_base = history.versions[0]
        .properties
        .get("embedding")
        .and_then(|v| v.as_vector().map(<[f32]>::to_vec))
        .expect("restored anchor must carry the embedding");
    assert_eq!(restored_base, base_vec);

    Ok(())
}

/// Edge mirror of the sparse-vector materialization regression test: the
/// checkpoint's edge extract path (snapshot base reconstruction +
/// materialization) is separate code from the node path and must be
/// exercised in its own right (Issue #3387).
#[test]
fn test_checkpoint_materializes_sparse_edge_vector_delta_and_round_trips() -> Result<()> {
    use aletheiadb::core::version::{VectorDelta, VersionData};

    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let wal_config = ConcurrentWalSystemConfig::new(&wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let source = NodeId::new(1)?;
    let target = NodeId::new(2)?;
    let edge_id = EdgeId::new(1)?;
    let dim = 128usize;
    let base_vec: Vec<f32> = (0..dim).map(|i| i as f32 * 0.02).collect();
    let mut updated_vec = base_vec.clone();
    updated_vec[11] = 7.0; // 1 change * 2 < 128 -> sparse delta

    for node_id in [source, target] {
        wal.append(WalOperation::CreateNode {
            node_id,
            label: GLOBAL_INTERNER.intern("SparseVecEdgeNode").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
            provenance: None,
        })?;
    }
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source,
        target,
        label: GLOBAL_INTERNER.intern("SPARSE_VEC_EDGE").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert_vector("embedding", &base_vec)
            .build(),
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.append(WalOperation::UpdateEdge {
        edge_id,
        version_id: VersionId::new(900)?,
        label: GLOBAL_INTERNER.intern("SPARSE_VEC_EDGE").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert_vector("embedding", &updated_vec)
            .build(),
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.flush()?;

    let checkpoint_lsn = LSN(wal.current_lsn().0.saturating_sub(1));
    {
        let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
        let mut manager = CheckpointManager::new(config)?;
        let (current, historical, _final_lsn) = manager.recover(&wal)?;

        // Precondition: the edge update really produced a SPARSE delta.
        let head_id = historical
            .get_current_edge_version(edge_id)
            .expect("edge head version");
        let head = historical.get_edge_version(head_id).unwrap();
        let VersionData::Delta { delta } = &head.data else {
            panic!("precondition: edge update version must be a delta");
        };
        assert!(
            delta
                .vector_deltas
                .values()
                .any(|d| matches!(d, VectorDelta::Sparse { .. })),
            "precondition: small edge embedding edit must produce VectorDelta::Sparse"
        );

        manager.create_checkpoint(checkpoint_lsn, &current, &historical)?;
    }

    // Restore-only recovery: the updated edge vector must round-trip.
    let config = UnifiedCheckpointConfig::with_data_dir(&data_dir);
    let mut manager = CheckpointManager::new(config)?;
    let (_current, restored, _final_lsn) = manager.recover(&wal)?;

    let history = restored.get_edge_history(edge_id)?;
    assert_eq!(history.version_count(), 2);
    let restored_updated = history.versions[1]
        .properties
        .get("embedding")
        .and_then(|v| v.as_vector().map(<[f32]>::to_vec))
        .expect("restored edge head must carry the embedding");
    assert_eq!(
        restored_updated, updated_vec,
        "sparse edge vector delta must be materialized into the checkpoint \
         and reconstruct after restore"
    );

    Ok(())
}
