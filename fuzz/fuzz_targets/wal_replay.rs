#![no_main]

mod support;

use std::collections::BTreeMap;

use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::storage::checkpoint::{CheckpointConfig, CheckpointManager};
use aletheiadb::storage::wal::WalOperation;
use aletheiadb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use support::{MAX_FUZZ_OPS, int_properties, intern_label, timestamp_from_step};

const MAX_NODE_KEY: u64 = 16;

#[derive(Debug, Arbitrary)]
enum FuzzWalMutation {
    CreateNode { node_key: u8, value: i64 },
    UpdateNode { node_key: u8, value: i64 },
    DeleteNode { node_key: u8 },
}

#[derive(Debug, Arbitrary)]
struct WalReplayCase {
    mutations: Vec<FuzzWalMutation>,
}

fuzz_target!(|case: WalReplayCase| {
    let tempdir = tempfile::tempdir().expect("fuzz tempdir must be created");
    let wal_dir = tempdir.path().join("wal");
    let checkpoint_dir = tempdir.path().join("checkpoint");
    let config = ConcurrentWalSystemConfig::new(&wal_dir).with_flush_interval_ms(60_000);
    let mut wal = ConcurrentWalSystem::new(config).expect("fuzz WAL system must initialize");

    let label = intern_label("FuzzNode");
    let mut expected_nodes = BTreeMap::<u64, i64>::new();
    let mut next_version_id = 1_000_u64;
    let mut step = 1_u64;

    for mutation in case.mutations.iter().take(MAX_FUZZ_OPS) {
        let appended = match *mutation {
            FuzzWalMutation::CreateNode { node_key, value } => {
                let raw_node_id = (u64::from(node_key) % MAX_NODE_KEY) + 1;
                if expected_nodes.contains_key(&raw_node_id) {
                    None
                } else {
                    expected_nodes.insert(raw_node_id, value);
                    Some(WalOperation::CreateNode {
                        node_id: NodeId::new(raw_node_id)
                            .expect("bounded fuzz node ID must be valid"),
                        label,
                        properties: int_properties(value),
                        valid_from: timestamp_from_step(step),
                    })
                }
            }
            FuzzWalMutation::UpdateNode { node_key, value } => {
                let raw_node_id = (u64::from(node_key) % MAX_NODE_KEY) + 1;
                if let Some(stored) = expected_nodes.get_mut(&raw_node_id) {
                    *stored = value;
                    next_version_id += 1;
                    Some(WalOperation::UpdateNode {
                        node_id: NodeId::new(raw_node_id)
                            .expect("bounded fuzz node ID must be valid"),
                        version_id: VersionId::new(next_version_id)
                            .expect("bounded fuzz version ID must be valid"),
                        label,
                        properties: int_properties(value),
                        valid_from: timestamp_from_step(step),
                    })
                } else {
                    None
                }
            }
            FuzzWalMutation::DeleteNode { node_key } => {
                let raw_node_id = (u64::from(node_key) % MAX_NODE_KEY) + 1;
                if expected_nodes.remove(&raw_node_id).is_some() {
                    Some(WalOperation::DeleteNode {
                        node_id: NodeId::new(raw_node_id)
                            .expect("bounded fuzz node ID must be valid"),
                        valid_from: timestamp_from_step(step),
                    })
                } else {
                    None
                }
            }
        };

        if let Some(operation) = appended {
            wal.append(operation)
                .expect("valid fuzz WAL operation must append");
            step += 1;
        }
    }

    wal.flush().expect("valid fuzz WAL must flush");

    let recovery = CheckpointManager::new(CheckpointConfig::with_data_dir(checkpoint_dir))
        .and_then(|mut manager| manager.recover(&wal));
    wal.shutdown();

    let (current, _historical, _final_lsn) =
        recovery.expect("valid fuzz WAL sequence must recover");
    assert_eq!(current.node_count(), expected_nodes.len());

    for (raw_node_id, expected_value) in expected_nodes {
        let node = current
            .get_node(NodeId::new(raw_node_id).expect("bounded fuzz node ID must be valid"))
            .expect("expected fuzz node must recover");
        assert_eq!(
            node.properties
                .get("value")
                .and_then(|stored| stored.as_int()),
            Some(expected_value)
        );
    }
});
