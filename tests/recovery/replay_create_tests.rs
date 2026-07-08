//! Tests for CreateNode/CreateEdge replay handlers (Issue #288)
//!
//! These tests verify that recovery correctly replays:
//! - CreateNode operations with proper ID tracking and storage
//! - CreateEdge operations with proper ID tracking and storage
//! - Property handling (including vectors)
//! - Bi-temporal interval preservation
//! - Version metadata creation
//! - Temporal index updates

use aletheiadb::{
    GLOBAL_INTERNER,
    core::error::Result,
    core::{
        hlc::HybridTimestamp,
        id::{EdgeId, NodeId},
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
        valid_from: timestamp,
        provenance: None,
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
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
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: Node has correct properties
    let node = current.get_node(node_id)?;
    use aletheiadb::core::property::PropertyValue;
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
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: Node has correct vector property
    let node = current.get_node(node_id)?;
    use aletheiadb::core::property::PropertyValue;
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
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
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
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, _historical, _lsn) = manager.recover(&wal)?;

    // Then: Edge has correct properties
    let edge = current.get_edge(edge_id)?;
    use aletheiadb::core::property::PropertyValue;
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
            valid_from: time::now(),
            provenance: None,
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
            valid_from: time::now(),
            provenance: None,
        })?;
    }
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
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
            valid_from: time::now(),
            provenance: None,
        })?;
    }
    wal.flush()?;

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
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
    // Issue #452: verify the reconstructed bi-temporal interval exactly,
    // not merely that a version exists.
    //
    // Given: WAL with CreateNode using a specific valid_from
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();
    let now = time::now().wallclock();
    // Backdate by one hour so wallclock races cannot flake the assertions.
    let valid_from = HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Test").unwrap(),
        properties: PropertyMap::new(),
        valid_from,
        provenance: None,
    })?;
    wal.flush()?;

    // The transaction time after replay must equal the entry's LOGGED
    // timestamp (assigned by the WAL at append time), not the replay time.
    let create_ts = logged_timestamp(&wal, |op| matches!(op, WalOperation::CreateNode { .. }))?;
    assert!(
        valid_from < create_ts,
        "premise: the backdated valid_from must precede the logged entry timestamp (clock anomaly?)"
    );

    // When: recover()
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (_current, historical, _lsn) = manager.recover(&wal)?;

    // Then: exactly one version, with the exact bi-temporal interval:
    // valid time [valid_from, open), transaction time [logged create ts, open).
    let history = historical.get_node_history(node_id)?;
    assert_eq!(history.version_count(), 1);
    let version = &history.versions[0];
    assert_eq!(
        version.temporal.valid_time().start(),
        valid_from,
        "valid_from must equal the LOGGED valid_from"
    );
    assert!(
        version.temporal.valid_time().is_current(),
        "valid time must remain open-ended after replay"
    );
    assert_eq!(
        version.temporal.transaction_time().start(),
        create_ts,
        "transaction time must start at the entry's LOGGED timestamp"
    );
    assert!(
        version.temporal.transaction_time().is_current(),
        "transaction time must remain open-ended after replay"
    );

    // And: the historical query API agrees — the node is visible at the
    // bi-temporal coordinate (valid_from, create_ts) and any point after.
    assert!(
        historical
            .get_node_at_time(node_id, valid_from, create_ts)
            .is_ok(),
        "node must be visible at its creation bi-temporal coordinate after replay"
    );
    let now_after_replay = time::now();
    assert!(
        historical
            .get_node_at_time(node_id, now_after_replay, now_after_replay)
            .is_ok(),
        "node must be visible at the current bi-temporal coordinate after replay"
    );

    Ok(())
}

#[test]
fn test_replay_preserves_backdated_create_valid_from() -> Result<()> {
    // Issue #452 + #3221: a BACKDATED create (valid_from in the past) must
    // survive replay with its exact valid_from — replay must honor the
    // logged valid_from, never substitute the entry/replay timestamp.
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let node_id = NodeId::new(1).unwrap();
    let now = time::now().wallclock();
    // Backdate by one hour so wallclock races cannot flake the assertions.
    let valid_from = HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("BackdatedTest").unwrap(),
        properties: PropertyMapBuilder::new().insert("name", "Alice").build(),
        valid_from,
        provenance: None,
    })?;
    wal.flush()?;

    let create_ts = logged_timestamp(&wal, |op| matches!(op, WalOperation::CreateNode { .. }))?;
    assert!(
        valid_from < create_ts,
        "test premise: valid_from is backdated relative to the logged entry timestamp"
    );

    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (_current, historical, _lsn) = manager.recover(&wal)?;

    let history = historical.get_node_history(node_id)?;
    assert_eq!(history.version_count(), 1);
    let version = &history.versions[0];
    assert_eq!(
        version.temporal.valid_time().start(),
        valid_from,
        "backdated valid_from must survive replay exactly"
    );
    assert!(
        version.temporal.valid_time().is_current(),
        "valid time must remain open-ended after replay"
    );
    assert_eq!(
        version.temporal.transaction_time().start(),
        create_ts,
        "transaction time must start at the entry's LOGGED timestamp"
    );
    assert!(
        version.temporal.transaction_time().is_current(),
        "transaction time must remain open-ended after replay"
    );

    // Point-in-time visibility: visible between the backdate and now,
    // invisible strictly before the backdated valid_from.
    let probe_within = HybridTimestamp::new(now - 1_800_000_000, 0).unwrap();
    let probe_before = HybridTimestamp::new(now - 7_200_000_000, 0).unwrap();
    assert!(
        historical
            .get_node_at_time(node_id, probe_within, time::now())
            .is_ok(),
        "node must be visible at a valid time after the backdated valid_from"
    );
    assert!(
        historical
            .get_node_at_time(node_id, probe_before, time::now())
            .is_err(),
        "node must NOT be visible at a valid time before the backdated valid_from"
    );

    Ok(())
}

#[test]
fn test_replay_preserves_create_edge_temporal_interval() -> Result<()> {
    // Issue #452: edge mirror of test_replay_preserves_temporal_interval,
    // with a backdated valid_from.
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config)?;

    let source_id = NodeId::new(1).unwrap();
    let target_id = NodeId::new(2).unwrap();
    let edge_id = EdgeId::new(1).unwrap();
    let now = time::now().wallclock();
    let valid_from = HybridTimestamp::new(now - 3_600_000_000, 0).unwrap();

    for node_id in [source_id, target_id] {
        wal.append(WalOperation::CreateNode {
            node_id,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMap::new(),
            valid_from,
            provenance: None,
        })?;
    }
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: source_id,
        target: target_id,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMap::new(),
        valid_from,
        provenance: None,
    })?;
    wal.flush()?;

    let create_edge_ts =
        logged_timestamp(&wal, |op| matches!(op, WalOperation::CreateEdge { .. }))?;

    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (_current, historical, _lsn) = manager.recover(&wal)?;

    let history = historical.get_edge_history(edge_id)?;
    assert_eq!(history.version_count(), 1);
    let version = &history.versions[0];
    assert_eq!(
        version.temporal.valid_time().start(),
        valid_from,
        "edge valid_from must equal the LOGGED valid_from"
    );
    assert!(
        version.temporal.valid_time().is_current(),
        "edge valid time must remain open-ended after replay"
    );
    assert_eq!(
        version.temporal.transaction_time().start(),
        create_edge_ts,
        "edge transaction time must start at the entry's LOGGED timestamp"
    );
    assert!(
        version.temporal.transaction_time().is_current(),
        "edge transaction time must remain open-ended after replay"
    );

    // Historical query API agrees on visibility.
    let probe_within = HybridTimestamp::new(now - 1_800_000_000, 0).unwrap();
    let probe_before = HybridTimestamp::new(now - 7_200_000_000, 0).unwrap();
    assert!(
        historical
            .get_edge_at_time(edge_id, probe_within, time::now())
            .is_ok(),
        "edge must be visible at a valid time after the backdated valid_from"
    );
    assert!(
        historical
            .get_edge_at_time(edge_id, probe_before, time::now())
            .is_err(),
        "edge must NOT be visible at a valid time before the backdated valid_from"
    );

    Ok(())
}
