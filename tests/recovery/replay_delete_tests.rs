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
        hlc::HybridTimestamp,
        id::{EdgeId, NodeId, VersionId},
        property::{PropertyMap, PropertyMapBuilder},
        temporal::{Timestamp, time},
    },
    storage::{
        checkpoint::{CheckpointConfig, CheckpointManager},
        wal::{
            LSN, WalOperation,
            concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
        },
    },
};
use tempfile::TempDir;

/// Read back the LOGGED timestamp of the first WAL entry matching `pred`.
///
/// Replay stamps transaction time with the WAL entry's logged timestamp (not
/// the replay time), so interval assertions must anchor on this value.
fn logged_timestamp(
    wal: &ConcurrentWalSystem,
    pred: impl Fn(&WalOperation) -> bool,
) -> Result<Timestamp> {
    let entries = wal.read_from(LSN::initial())?;
    Ok(entries
        .iter()
        .find(|e| pred(&e.operation))
        .expect("expected a matching WAL entry")
        .timestamp)
}

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
        provenance: None,
    })?;

    // Delete node
    wal.append(WalOperation::DeleteNode {
        node_id,
        valid_from: timestamp2,
        version_id: None,
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
        provenance: None,
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
        provenance: None,
    })?;

    // Delete node
    wal.append(WalOperation::DeleteNode {
        node_id,
        valid_from: time::now(),
        version_id: None,
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
        provenance: None,
    })?;

    wal.append(WalOperation::CreateNode {
        node_id: target_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    })?;

    // Create edge
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: source_id,
        target: target_id,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    })?;

    // Delete edge
    wal.append(WalOperation::DeleteEdge {
        edge_id,
        valid_from: time::now(),
        version_id: None,
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
fn test_replay_delete_node_preserves_temporal_intervals() -> Result<()> {
    // Issue #452: after replaying Create + Delete, the version chain must
    // carry exact bi-temporal intervals:
    // - prior head: valid_from preserved, valid time closed at the
    //   tombstone's LOGGED valid_from, transaction time closed at the delete
    //   entry's LOGGED timestamp;
    // - tombstone: empty valid interval anchored at the LOGGED valid_from
    //   (issue #3400: replay honors the logged value, mirroring the live
    //   path), transaction [delete ts, open).
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();
    let now_ts = time::now();
    let vf = HybridTimestamp::new(now_ts.wallclock() - 7_200_000_000, 0).unwrap(); // 2h ago
    let delete_vf = now_ts;

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new().insert("name", "Alice").build(),
        valid_from: vf,
        provenance: None,
    })?;
    wal.append(WalOperation::DeleteNode {
        node_id,
        valid_from: delete_vf,
        version_id: None,
    })?;
    wal.flush()?;

    let create_ts = logged_timestamp(&wal, |op| matches!(op, WalOperation::CreateNode { .. }))?;
    let delete_ts = logged_timestamp(&wal, |op| matches!(op, WalOperation::DeleteNode { .. }))?;
    assert!(
        create_ts < delete_ts,
        "premise: WAL entry timestamps must be strictly ordered (clock anomaly?)"
    );

    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    assert!(current.get_node(node_id).is_err());

    let history = historical.get_node_history(node_id)?;
    assert_eq!(history.version_count(), 2, "create + delete tombstone");

    let prior = &history.versions[0];
    let tombstone = &history.versions[1];

    assert_eq!(
        prior.temporal.valid_time().start(),
        vf,
        "prior head's valid_from must survive replay exactly"
    );
    assert_eq!(
        prior.temporal.valid_time().end(),
        delete_vf,
        "prior head's valid time must be closed exactly at the tombstone's LOGGED valid_from"
    );
    assert_eq!(
        prior.temporal.transaction_time().start(),
        create_ts,
        "prior head's transaction time must start at the LOGGED create timestamp"
    );
    assert_eq!(
        prior.temporal.transaction_time().end(),
        delete_ts,
        "prior head's tx-time closure must survive replay at the LOGGED delete timestamp"
    );

    assert_eq!(
        tombstone.temporal.valid_time().start(),
        delete_vf,
        "tombstone valid_from must equal the LOGGED delete valid_from (issue #3400)"
    );
    assert!(
        tombstone.temporal.valid_time().is_empty(),
        "tombstone must carry an empty valid interval"
    );
    assert_eq!(
        tombstone.temporal.transaction_time().start(),
        delete_ts,
        "tombstone tx time must start at the LOGGED delete timestamp"
    );
    assert!(
        tombstone.temporal.transaction_time().is_current(),
        "tombstone's transaction time must be open-ended after replay"
    );

    // Point-in-time visibility matrix:
    // - anchored (in both dimensions) before the delete: visible;
    // - anchored at the delete's commit or now: gone.
    assert!(
        historical.get_node_at_time(node_id, vf, create_ts).is_ok(),
        "node must be visible when anchored before the delete"
    );
    assert!(
        historical.get_node_at_time(node_id, vf, delete_ts).is_err(),
        "node must be gone when anchored at the delete's commit"
    );
    let now_after_replay = time::now();
    assert!(
        historical
            .get_node_at_time(node_id, now_after_replay, now_after_replay)
            .is_err()
    );

    Ok(())
}

#[test]
fn test_replay_delete_edge_preserves_temporal_intervals() -> Result<()> {
    // Issue #452: edge mirror of test_replay_delete_node_preserves_temporal_intervals.
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let source_id = NodeId::new(1).unwrap();
    let target_id = NodeId::new(2).unwrap();
    let edge_id = EdgeId::new(1).unwrap();
    let now_ts = time::now();
    let vf = HybridTimestamp::new(now_ts.wallclock() - 7_200_000_000, 0).unwrap();
    let delete_vf = now_ts;

    for node_id in [source_id, target_id] {
        wal.append(WalOperation::CreateNode {
            node_id,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMap::new(),
            valid_from: vf,
            provenance: None,
        })?;
    }
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: source_id,
        target: target_id,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMap::new(),
        valid_from: vf,
        provenance: None,
    })?;
    wal.append(WalOperation::DeleteEdge {
        edge_id,
        valid_from: delete_vf,
        version_id: None,
    })?;
    wal.flush()?;

    let create_edge_ts =
        logged_timestamp(&wal, |op| matches!(op, WalOperation::CreateEdge { .. }))?;
    let delete_edge_ts =
        logged_timestamp(&wal, |op| matches!(op, WalOperation::DeleteEdge { .. }))?;
    assert!(
        create_edge_ts < delete_edge_ts,
        "premise: WAL entry timestamps must be strictly ordered (clock anomaly?)"
    );

    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    assert!(current.get_edge(edge_id).is_err());

    let history = historical.get_edge_history(edge_id)?;
    assert_eq!(history.version_count(), 2, "create + delete tombstone");

    let prior = &history.versions[0];
    let tombstone = &history.versions[1];

    assert_eq!(
        prior.temporal.valid_time().start(),
        vf,
        "prior head's valid_from must survive replay exactly"
    );
    assert_eq!(
        prior.temporal.valid_time().end(),
        delete_vf,
        "prior head's valid time must be closed exactly at the tombstone's LOGGED valid_from"
    );
    assert_eq!(
        prior.temporal.transaction_time().start(),
        create_edge_ts,
        "prior head's transaction time must start at the LOGGED create timestamp"
    );
    assert_eq!(
        prior.temporal.transaction_time().end(),
        delete_edge_ts,
        "prior head's tx-time closure must survive replay at the LOGGED delete timestamp"
    );

    assert_eq!(
        tombstone.temporal.valid_time().start(),
        delete_vf,
        "tombstone valid_from must equal the LOGGED delete valid_from (issue #3400)"
    );
    assert!(
        tombstone.temporal.valid_time().is_empty(),
        "tombstone must carry an empty valid interval"
    );
    assert_eq!(
        tombstone.temporal.transaction_time().start(),
        delete_edge_ts,
        "tombstone tx time must start at the LOGGED delete timestamp"
    );
    assert!(
        tombstone.temporal.transaction_time().is_current(),
        "tombstone's transaction time must be open-ended after replay"
    );

    // Point-in-time visibility matrix.
    assert!(
        historical
            .get_edge_at_time(edge_id, vf, create_edge_ts)
            .is_ok(),
        "edge must be visible when anchored before the delete"
    );
    assert!(
        historical
            .get_edge_at_time(edge_id, vf, delete_edge_ts)
            .is_err(),
        "edge must be gone when anchored at the delete's commit"
    );
    let now_after_replay = time::now();
    assert!(
        historical
            .get_edge_at_time(edge_id, now_after_replay, now_after_replay)
            .is_err()
    );

    Ok(())
}

#[test]
fn test_replay_delete_node_honors_logged_valid_from() -> Result<()> {
    // Issue #3400: a backdated delete (Issue #3221) logs a user-supplied
    // valid_from that differs from the WAL entry's timestamp. Replay must
    // stamp the tombstone with the LOGGED valid_from — mirroring the live
    // path (`apply_node_delete`) — not the entry timestamp.
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();
    let now = time::now().wallclock();
    let create_vf = HybridTimestamp::new(now - 7_200_000_000, 0).unwrap(); // 2h ago
    let delete_vf = HybridTimestamp::new(now - 3_600_000_000, 0).unwrap(); // 1h ago (backdated)

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new().insert("name", "Zoe").build(),
        valid_from: create_vf,
        provenance: None,
    })?;
    wal.append(WalOperation::DeleteNode {
        node_id,
        valid_from: delete_vf,
        version_id: None,
    })?;
    wal.flush()?;

    let delete_ts = logged_timestamp(&wal, |op| matches!(op, WalOperation::DeleteNode { .. }))?;
    assert!(
        delete_vf < delete_ts,
        "premise: the backdated valid_from must differ from the entry timestamp"
    );

    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    assert!(current.get_node(node_id).is_err());

    let history = historical.get_node_history(node_id)?;
    assert_eq!(history.version_count(), 2, "create + delete tombstone");

    let prior = &history.versions[0];
    let tombstone = &history.versions[1];

    assert_eq!(
        tombstone.temporal.valid_time().start(),
        delete_vf,
        "tombstone valid_from must equal the LOGGED backdated valid_from, not the entry timestamp"
    );
    assert!(
        tombstone.temporal.valid_time().is_empty(),
        "tombstone must carry an empty valid interval"
    );
    assert_eq!(
        prior.temporal.valid_time().end(),
        delete_vf,
        "prior head's valid_to must be closed at the LOGGED backdated valid_from"
    );
    assert_eq!(
        tombstone.temporal.transaction_time().start(),
        delete_ts,
        "tombstone tx time must still start at the LOGGED delete entry timestamp"
    );
    assert_eq!(
        prior.temporal.transaction_time().end(),
        delete_ts,
        "prior head's tx-time closure must still use the LOGGED delete entry timestamp"
    );

    Ok(())
}

#[test]
fn test_replay_delete_edge_honors_logged_valid_from() -> Result<()> {
    // Issue #3400: edge mirror of test_replay_delete_node_honors_logged_valid_from.
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let source_id = NodeId::new(1).unwrap();
    let target_id = NodeId::new(2).unwrap();
    let edge_id = EdgeId::new(1).unwrap();
    let now = time::now().wallclock();
    let create_vf = HybridTimestamp::new(now - 7_200_000_000, 0).unwrap(); // 2h ago
    let delete_vf = HybridTimestamp::new(now - 3_600_000_000, 0).unwrap(); // 1h ago (backdated)

    for node_id in [source_id, target_id] {
        wal.append(WalOperation::CreateNode {
            node_id,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMap::new(),
            valid_from: create_vf,
            provenance: None,
        })?;
    }
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: source_id,
        target: target_id,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMap::new(),
        valid_from: create_vf,
        provenance: None,
    })?;
    wal.append(WalOperation::DeleteEdge {
        edge_id,
        valid_from: delete_vf,
        version_id: None,
    })?;
    wal.flush()?;

    let delete_ts = logged_timestamp(&wal, |op| matches!(op, WalOperation::DeleteEdge { .. }))?;
    assert!(
        delete_vf < delete_ts,
        "premise: the backdated valid_from must differ from the entry timestamp"
    );

    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(&wal)?;

    assert!(current.get_edge(edge_id).is_err());

    let history = historical.get_edge_history(edge_id)?;
    assert_eq!(history.version_count(), 2, "create + delete tombstone");

    let prior = &history.versions[0];
    let tombstone = &history.versions[1];

    assert_eq!(
        tombstone.temporal.valid_time().start(),
        delete_vf,
        "tombstone valid_from must equal the LOGGED backdated valid_from, not the entry timestamp"
    );
    assert!(
        tombstone.temporal.valid_time().is_empty(),
        "tombstone must carry an empty valid interval"
    );
    assert_eq!(
        prior.temporal.valid_time().end(),
        delete_vf,
        "prior head's valid_to must be closed at the LOGGED backdated valid_from"
    );
    assert_eq!(
        tombstone.temporal.transaction_time().start(),
        delete_ts,
        "tombstone tx time must still start at the LOGGED delete entry timestamp"
    );
    assert_eq!(
        prior.temporal.transaction_time().end(),
        delete_ts,
        "prior head's tx-time closure must still use the LOGGED delete entry timestamp"
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
            provenance: None,
        })?;
    }

    // Delete nodes 2 and 4
    for id in [2, 4] {
        wal.append(WalOperation::DeleteNode {
            node_id: NodeId::new(id).unwrap(),
            valid_from: time::now(),
            version_id: None,
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
        provenance: None,
    })?;

    // Delete node
    wal.append(WalOperation::DeleteNode {
        node_id,
        valid_from: time::now(),
        version_id: None,
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
        provenance: None,
    })?;

    // Create node 2
    wal.append(WalOperation::CreateNode {
        node_id: NodeId::new(2).unwrap(),
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMapBuilder::new().insert("value", 2_i64).build(),
        valid_from: time::now(),
        provenance: None,
    })?;

    // Update node 1
    wal.append(WalOperation::UpdateNode {
        node_id: NodeId::new(1).unwrap(),
        version_id: VersionId::new(3).unwrap(),
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMapBuilder::new().insert("value", 10_i64).build(),
        valid_from: time::now(),
        provenance: None,
    })?;

    // Delete node 2
    wal.append(WalOperation::DeleteNode {
        node_id: NodeId::new(2).unwrap(),
        valid_from: time::now(),
        version_id: None,
    })?;

    // Create node 3
    wal.append(WalOperation::CreateNode {
        node_id: NodeId::new(3).unwrap(),
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMapBuilder::new().insert("value", 3_i64).build(),
        valid_from: time::now(),
        provenance: None,
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
