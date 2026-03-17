//! MVCC snapshot integration tests.
//!
//! These tests exercise snapshot isolation behavior for current and historical
//! storage and verify checkpoint/recovery expectations tied to snapshotting.

#![cfg(feature = "mvcc-snapshots-tdd")]

use aletheiadb::core::GLOBAL_INTERNER;
use aletheiadb::core::id::VersionId;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::temporal::time;
use aletheiadb::core::version::VersionData;
use aletheiadb::storage::checkpoint::{CheckpointConfig, CheckpointManager};
use aletheiadb::storage::current::CurrentStorage;
use aletheiadb::storage::historical::HistoricalStorage;
use aletheiadb::storage::wal::LSN;
use aletheiadb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
use std::collections::HashMap;
use tempfile::tempdir;

#[test]
fn test_current_snapshot_isolated_from_late_writes() {
    let current = CurrentStorage::new();

    for i in 0..100 {
        let props = PropertyMapBuilder::new()
            .insert("id", i as i64)
            .insert("generation", 1i64)
            .build();
        current.create_node("Person", props).unwrap();
    }

    let snapshot = current.create_snapshot(LSN(10));

    for i in 100..200 {
        let props = PropertyMapBuilder::new()
            .insert("id", i as i64)
            .insert("generation", 2i64)
            .build();
        current.create_node("Person", props).unwrap();
    }

    assert_eq!(snapshot.lsn(), LSN(10));
    assert_eq!(snapshot.node_count(), 100);
    assert_eq!(current.node_count(), 200);

    let all_generation_one = snapshot
        .iter_nodes()
        .all(|node| node.get_property("generation").and_then(|v| v.as_int()) == Some(1));
    assert!(
        all_generation_one,
        "snapshot must not observe writes that happen after snapshot creation"
    );
}

#[test]
fn test_historical_snapshot_isolated_from_late_versions() {
    let current = CurrentStorage::new();
    let mut historical = HistoricalStorage::new();
    let label = GLOBAL_INTERNER.intern("VersionedNode").unwrap();

    let node_id = current
        .create_node(
            "VersionedNode",
            PropertyMapBuilder::new().insert("seed", 0i64).build(),
        )
        .unwrap();

    let first_props = PropertyMapBuilder::new().insert("generation", 1i64).build();
    historical
        .add_node_version(
            node_id,
            VersionId::new(100).unwrap(),
            time::from_millis(1_000),
            time::from_millis(1_000),
            label,
            first_props,
            false,
        )
        .unwrap();

    let snapshot = historical.create_snapshot(LSN(20));

    let second_props = PropertyMapBuilder::new().insert("generation", 2i64).build();
    historical
        .add_node_version(
            node_id,
            VersionId::new(101).unwrap(),
            time::from_millis(2_000),
            time::from_millis(2_000),
            label,
            second_props,
            false,
        )
        .unwrap();

    assert_eq!(snapshot.lsn(), LSN(20));
    assert_eq!(snapshot.node_version_count(), 1);

    let only_version = snapshot.iter_node_versions().next().unwrap();
    assert_eq!(only_version.id, VersionId::new(100).unwrap());

    match &only_version.data {
        VersionData::Anchor { properties, .. } => {
            let generation = properties
                .get("generation")
                .and_then(|value| value.as_int())
                .unwrap();
            assert_eq!(generation, 1);
        }
        VersionData::Delta { .. } => {
            panic!("first node version should be an anchor");
        }
    }
}

#[test]
fn test_checkpoint_persists_snapshot_lsn() {
    let dir = tempdir().unwrap();
    let current = CurrentStorage::new();
    let historical = HistoricalStorage::new();

    for i in 0..10 {
        let props = PropertyMapBuilder::new()
            .insert("id", i as i64)
            .insert("name", format!("node-{i}"))
            .build();
        current.create_node("CheckpointNode", props).unwrap();
    }

    let config = CheckpointConfig::with_data_dir(dir.path());
    let mut manager = CheckpointManager::new(config).unwrap();
    let checkpoint_lsn = LSN(42);

    let stats = manager
        .create_checkpoint(checkpoint_lsn, &current, &historical)
        .unwrap();

    assert_eq!(stats.lsn, checkpoint_lsn);
    assert_eq!(stats.node_count, 10);
    assert_eq!(manager.get_persisted_lsn(), Some(checkpoint_lsn));
}

#[test]
fn test_checkpoint_recovery_preserves_node_version_ids() {
    let dir = tempdir().unwrap();
    let current = CurrentStorage::new();
    let historical = HistoricalStorage::new();
    let mut expected_versions = HashMap::new();

    for i in 0..20 {
        let props = PropertyMapBuilder::new()
            .insert("id", i as i64)
            .insert("kind", "version-check")
            .build();
        let node_id = current.create_node("VersionedNode", props).unwrap();
        let node = current.get_node(node_id).unwrap();
        expected_versions.insert(node_id, node.current_version);
    }

    let config = CheckpointConfig::with_data_dir(dir.path());
    let mut manager = CheckpointManager::new(config).unwrap();
    manager
        .create_checkpoint(LSN(0), &current, &historical)
        .unwrap();

    let wal_config = ConcurrentWalSystemConfig::new(dir.path().join("wal"));
    let wal = ConcurrentWalSystem::new(wal_config).unwrap();
    let (recovered, _, _) = manager.recover(&wal).unwrap();

    assert_eq!(recovered.node_count(), expected_versions.len());

    for (node_id, expected_version_id) in expected_versions {
        let recovered_node = recovered.get_node(node_id).unwrap();
        assert_eq!(recovered_node.current_version, expected_version_id);
    }
}
