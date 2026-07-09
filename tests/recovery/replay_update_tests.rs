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
    core::error::Result,
    core::{
        hlc::HybridTimestamp,
        id::{EdgeId, NodeId, VersionId},
        property::{PropertyMap, PropertyMapBuilder, PropertyValue},
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
        provenance: None,
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
        provenance: None,
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
        provenance: None,
    })?;

    // Update node to "User" label
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(2).unwrap(),
        label: GLOBAL_INTERNER.intern("User").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
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
        provenance: None,
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
        provenance: None,
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
        provenance: None,
    })?;

    // Update 5 times
    for i in 1..=5 {
        wal.append(WalOperation::UpdateNode {
            node_id,
            version_id: VersionId::new((i + 1) as u64).unwrap(),
            label: GLOBAL_INTERNER.intern("Counter").unwrap(),
            properties: PropertyMapBuilder::new().insert("count", i).build(),
            valid_from: time::now(),
            provenance: None,
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
        properties: PropertyMapBuilder::new().insert("since", 2020_i64).build(),
        valid_from: time::now(),
        provenance: None,
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
        provenance: None,
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
        provenance: None,
    })?;

    wal.append(WalOperation::CreateNode {
        node_id: target_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    })?;

    // Create edge with "KNOWS" label
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: source_id,
        target: target_id,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    })?;

    // Update edge to "FRIENDS_WITH" label
    wal.append(WalOperation::UpdateEdge {
        edge_id,
        version_id: VersionId::new(4).unwrap(),
        label: GLOBAL_INTERNER.intern("FRIENDS_WITH").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
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
fn test_replay_update_preserves_temporal_intervals() -> Result<()> {
    // Issue #452: after replaying Create + Update, the version chain must
    // carry exact bi-temporal intervals:
    // - superseded version: valid [vf1, vf2) — appending a later-valid
    //   version closes the predecessor's valid time at the successor's
    //   valid_from — with transaction time closed at the update entry's
    //   LOGGED timestamp (the tx-time closure must survive replay);
    // - new head: valid [vf2, open), transaction [update ts, open).
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();
    let now = time::now().wallclock();
    let vf1 = HybridTimestamp::new(now - 7_200_000_000, 0).unwrap(); // 2h ago
    let vf2 = HybridTimestamp::new(now - 3_600_000_000, 0).unwrap(); // 1h ago

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Counter").unwrap(),
        properties: PropertyMapBuilder::new().insert("count", 1_i64).build(),
        valid_from: vf1,
        provenance: None,
    })?;
    wal.append(WalOperation::UpdateNode {
        node_id,
        version_id: VersionId::new(2).unwrap(),
        label: GLOBAL_INTERNER.intern("Counter").unwrap(),
        properties: PropertyMapBuilder::new().insert("count", 2_i64).build(),
        valid_from: vf2,
        provenance: None,
    })?;
    wal.flush()?;

    let create_ts = logged_timestamp(&wal, |op| matches!(op, WalOperation::CreateNode { .. }))?;
    let update_ts = logged_timestamp(&wal, |op| matches!(op, WalOperation::UpdateNode { .. }))?;
    assert!(
        create_ts < update_ts,
        "premise: WAL entry timestamps must be strictly ordered (clock anomaly?)"
    );

    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (_current, historical, _lsn) = manager.recover(&wal)?;

    let history = historical.get_node_history(node_id)?;
    assert_eq!(history.version_count(), 2);

    let superseded = &history.versions[0];
    assert_eq!(
        superseded.temporal.valid_time().start(),
        vf1,
        "superseded version's valid_from must survive replay exactly"
    );
    assert_eq!(
        superseded.temporal.valid_time().end(),
        vf2,
        "superseded version's valid time must be closed at the successor's LOGGED valid_from"
    );
    assert_eq!(
        superseded.temporal.transaction_time().start(),
        create_ts,
        "superseded version's transaction time must start at the LOGGED create timestamp"
    );
    assert_eq!(
        superseded.temporal.transaction_time().end(),
        update_ts,
        "superseded version's tx-time closure must survive replay at the LOGGED update timestamp"
    );

    let head = &history.versions[1];
    assert_eq!(
        head.temporal.valid_time().start(),
        vf2,
        "new head's valid_from must equal the LOGGED update valid_from"
    );
    assert!(
        head.temporal.valid_time().is_current(),
        "new head's valid time must be open-ended after replay"
    );
    assert_eq!(
        head.temporal.transaction_time().start(),
        update_ts,
        "new head's transaction time must start at the LOGGED update timestamp"
    );
    assert!(
        head.temporal.transaction_time().is_current(),
        "new head's transaction time must be open-ended after replay"
    );

    // Historical query API: anchoring transaction time BEFORE the update
    // returns the superseded state; the current coordinate returns the new.
    let old = historical.get_node_at_time(node_id, vf1, create_ts)?;
    assert!(matches!(
        old.properties.get("count"),
        Some(PropertyValue::Int(1))
    ));
    let now_after_replay = time::now();
    let new = historical.get_node_at_time(node_id, now_after_replay, now_after_replay)?;
    assert!(matches!(
        new.properties.get("count"),
        Some(PropertyValue::Int(2))
    ));

    Ok(())
}

#[test]
fn test_replay_update_edge_preserves_temporal_intervals() -> Result<()> {
    // Issue #452: edge mirror of test_replay_update_preserves_temporal_intervals.
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let source_id = NodeId::new(1).unwrap();
    let target_id = NodeId::new(2).unwrap();
    let edge_id = EdgeId::new(1).unwrap();
    let now = time::now().wallclock();
    let vf1 = HybridTimestamp::new(now - 7_200_000_000, 0).unwrap();
    let vf2 = HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();

    for node_id in [source_id, target_id] {
        wal.append(WalOperation::CreateNode {
            node_id,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMap::new(),
            valid_from: vf1,
            provenance: None,
        })?;
    }
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: source_id,
        target: target_id,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMapBuilder::new().insert("weight", 1_i64).build(),
        valid_from: vf1,
        provenance: None,
    })?;
    wal.append(WalOperation::UpdateEdge {
        edge_id,
        version_id: VersionId::new(4).unwrap(),
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMapBuilder::new().insert("weight", 2_i64).build(),
        valid_from: vf2,
        provenance: None,
    })?;
    wal.flush()?;

    let create_edge_ts =
        logged_timestamp(&wal, |op| matches!(op, WalOperation::CreateEdge { .. }))?;
    let update_edge_ts =
        logged_timestamp(&wal, |op| matches!(op, WalOperation::UpdateEdge { .. }))?;
    assert!(
        create_edge_ts < update_edge_ts,
        "premise: WAL entry timestamps must be strictly ordered (clock anomaly?)"
    );

    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (_current, historical, _lsn) = manager.recover(&wal)?;

    let history = historical.get_edge_history(edge_id)?;
    assert_eq!(history.version_count(), 2);

    let superseded = &history.versions[0];
    assert_eq!(
        superseded.temporal.valid_time().start(),
        vf1,
        "superseded edge version's valid_from must survive replay exactly"
    );
    assert_eq!(
        superseded.temporal.valid_time().end(),
        vf2,
        "superseded edge version's valid time must be closed at the successor's LOGGED valid_from"
    );
    assert_eq!(
        superseded.temporal.transaction_time().start(),
        create_edge_ts,
        "superseded edge version's transaction time must start at the LOGGED create timestamp"
    );
    assert_eq!(
        superseded.temporal.transaction_time().end(),
        update_edge_ts,
        "superseded edge version's tx-time closure must survive replay"
    );

    let head = &history.versions[1];
    assert_eq!(
        head.temporal.valid_time().start(),
        vf2,
        "new edge head's valid_from must equal the LOGGED update valid_from"
    );
    assert!(
        head.temporal.valid_time().is_current(),
        "new edge head's valid time must be open-ended after replay"
    );
    assert_eq!(
        head.temporal.transaction_time().start(),
        update_edge_ts,
        "new edge head's transaction time must start at the LOGGED update timestamp"
    );
    assert!(
        head.temporal.transaction_time().is_current(),
        "new edge head's transaction time must be open-ended after replay"
    );

    // Point-in-time reads before/after the update return old/new state.
    let old = historical.get_edge_at_time(edge_id, vf1, create_edge_ts)?;
    assert!(matches!(
        old.properties.get("weight"),
        Some(PropertyValue::Int(1))
    ));
    let now_after_replay = time::now();
    let new = historical.get_edge_at_time(edge_id, now_after_replay, now_after_replay)?;
    assert!(matches!(
        new.properties.get("weight"),
        Some(PropertyValue::Int(2))
    ));

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
            provenance: None,
        })?;
    }

    // Update nodes 1 and 2
    wal.append(WalOperation::UpdateNode {
        node_id: NodeId::new(1).unwrap(),
        version_id: VersionId::new(4).unwrap(),
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMapBuilder::new().insert("value", 10_i64).build(),
        valid_from: time::now(),
        provenance: None,
    })?;

    wal.append(WalOperation::UpdateNode {
        node_id: NodeId::new(2).unwrap(),
        version_id: VersionId::new(5).unwrap(),
        label: GLOBAL_INTERNER.intern("Node").unwrap(),
        properties: PropertyMapBuilder::new().insert("value", 20_i64).build(),
        valid_from: time::now(),
        provenance: None,
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
