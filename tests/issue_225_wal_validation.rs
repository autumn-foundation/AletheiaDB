//! Test for issue #225: Validation of InternedString IDs during WAL replay
//!
//! This test verifies that invalid InternedString IDs in WAL entries are
//! detected and rejected during recovery, preventing crashes from corrupted
//! WAL files or missing checkpoint data.

use gallifreydb::core::id::NodeId;
use gallifreydb::core::interning::{InternedString, GLOBAL_INTERNER};
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::core::temporal::{BiTemporalInterval, time};
use gallifreydb::storage::persistence::{CheckpointConfig, PersistenceManager};
use gallifreydb::storage::wal::WalOperation;
use gallifreydb::storage::wal::concurrent_system::{
    ConcurrentWalSystem, ConcurrentWalSystemConfig,
};
use gallifreydb::utils::error::Error;
use tempfile::TempDir;

/// Test that WAL replay rejects invalid InternedString IDs
#[test]
fn test_wal_replay_rejects_invalid_interned_string() {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
    let wal = ConcurrentWalSystem::new(wal_config).unwrap();

    // Create a WalOperation with an INVALID InternedString ID
    // Use a very high ID that is unlikely to exist in the interner
    let invalid_label = InternedString::from_raw(999999);
    let node_id = NodeId::new(1).unwrap();
    let properties = PropertyMapBuilder::new().build();
    let temporal = BiTemporalInterval::current(time::now());

    let op = WalOperation::CreateNode {
        node_id,
        label: invalid_label,
        properties,
        temporal,
    };

    // Write the operation to WAL
    wal.append(op).unwrap();
    wal.flush().unwrap();

    // Now try to recover from this WAL
    // This should fail with a CorruptedData error because the InternedString ID
    // doesn't exist in the interner
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config).unwrap();
    let recovery_result = manager.recover(&wal);

    match recovery_result {
        Err(Error::Storage(storage_err)) => {
            let err_msg = format!("{}", storage_err);
            assert!(
                err_msg.contains("InternedString ID") && err_msg.contains("not found in interner"),
                "Expected error message about InternedString not found, got: {}",
                err_msg
            );
        }
        Err(other) => panic!("Expected StorageError::CorruptedData, got: {:?}", other),
        Ok(_) => panic!("Expected recovery to fail with corrupted InternedString ID, but it succeeded"),
    }
}

/// Test that valid InternedString IDs are accepted during WAL replay
#[test]
fn test_wal_replay_accepts_valid_interned_string() {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config).unwrap();

    // Create a WalOperation with a VALID InternedString (intern it first)
    let valid_label = GLOBAL_INTERNER.intern("TestNode").unwrap();
    let node_id = NodeId::new(1).unwrap();
    let properties = PropertyMapBuilder::new().build();
    let temporal = BiTemporalInterval::current(time::now());

    let op = WalOperation::CreateNode {
        node_id,
        label: valid_label,
        properties,
        temporal,
    };

    // Write the operation to WAL
    wal.append(op).unwrap();
    wal.flush().unwrap();

    // Now try to recover from this WAL
    // This should succeed because the InternedString ID exists in the interner
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config).unwrap();
    let recovery_result = manager.recover(&wal);

    match recovery_result {
        Ok((current, _historical, lsn)) => {
            // Recovery succeeded, verify the node was created
            assert!(lsn.0 > 0, "LSN should be > 0 after recovery");
            let node = current.get_node(node_id).unwrap();
            assert_eq!(node.id, node_id);
            assert!(node.has_label_str("TestNode"));
        }
        Err(e) => panic!("Expected recovery to succeed, but got error: {:?}", e),
    }
}

/// Test that corrupted WAL with random bytes for InternedString ID is rejected
#[test]
fn test_wal_replay_rejects_corrupted_interned_string() {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config).unwrap();

    // Create multiple operations with invalid InternedString IDs
    // simulating a corrupted WAL file
    for i in 1..=10 {
        let invalid_label = InternedString::from_raw(1000000 + i);
        let node_id = NodeId::new(i as u64).unwrap();
        let properties = PropertyMapBuilder::new().build();
        let temporal = BiTemporalInterval::current(time::now());

        let op = WalOperation::CreateNode {
            node_id,
            label: invalid_label,
            properties,
            temporal,
        };

        wal.append(op).unwrap();
    }

    wal.flush().unwrap();

    // Recovery should fail on the first corrupted entry
    let config = CheckpointConfig {
        checkpoint_dir: temp_dir.path().join("checkpoints"),
        ..Default::default()
    };
    let mut manager = PersistenceManager::new(config).unwrap();
    let recovery_result = manager.recover(&wal);

    assert!(
        recovery_result.is_err(),
        "Expected recovery to fail with corrupted WAL, but it succeeded"
    );

    if let Err(e) = recovery_result {
        let err_msg = format!("{}", e);
        assert!(
            err_msg.contains("InternedString ID") && err_msg.contains("not found"),
            "Expected error about InternedString not found, got: {}",
            err_msg
        );
    }
}
