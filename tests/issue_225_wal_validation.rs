//! Test for issue #225: Validation of InternedString IDs during WAL operations
//!
//! This test verifies that invalid InternedString IDs are detected and rejected
//! during WAL serialization, preventing the creation of corrupted WAL files.

use aletheiadb::core::id::NodeId;
use aletheiadb::core::interning::{GLOBAL_INTERNER, InternedString};
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::persistence::{CheckpointConfig, PersistenceManager};
use aletheiadb::storage::wal::WalOperation;
use aletheiadb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
use aletheiadb::utils::error::Error;
use tempfile::TempDir;

/// Test that WAL append rejects invalid InternedString IDs at write time.
///
/// In WAL V2, we persist the string content, so we must resolve the ID to a string
/// during serialization. If the ID is invalid (not in the interner), serialization
/// must fail.
#[test]
fn test_wal_append_rejects_invalid_interned_string() {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
    let wal = ConcurrentWalSystem::new(wal_config).unwrap();

    // Create a WalOperation with an INVALID InternedString ID
    // Use a very high ID that is unlikely to exist in the interner
    let invalid_label = InternedString::from_raw(999999);
    let node_id = NodeId::new(1).unwrap();
    let properties = PropertyMapBuilder::new().build();

    let op = WalOperation::CreateNode {
        node_id,
        label: invalid_label,
        properties,
        valid_from: time::now(),
    };

    // Attempt to write the operation to WAL
    // This should fail immediately during serialization because the string cannot be found
    let result = wal.append(op);

    assert!(
        result.is_err(),
        "Expected wal.append to fail with invalid InternedString"
    );

    match result {
        Err(Error::Storage(storage_err)) => {
            let err_msg = format!("{}", storage_err);
            assert!(
                err_msg.contains("InternedString 999999 not found"),
                "Expected error message about InternedString not found, got: {}",
                err_msg
            );
        }
        Err(other) => panic!("Expected StorageError::InconsistentState, got: {:?}", other),
        Ok(_) => panic!("Expected failure"),
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

    let op = WalOperation::CreateNode {
        node_id,
        label: valid_label,
        properties,
        valid_from: time::now(),
    };

    // Write the operation to WAL
    wal.append(op).unwrap();
    wal.flush().unwrap();

    // Now try to recover from this WAL
    // This should succeed because the InternedString ID exists in the interner
    // and was correctly serialized with its content
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

/// Test that multiple operations with invalid IDs are all rejected
#[test]
fn test_wal_append_rejects_corrupted_interned_string_loop() {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir);
    let wal = ConcurrentWalSystem::new(wal_config).unwrap();

    // Create multiple operations with invalid InternedString IDs
    for i in 1..=10 {
        let invalid_label = InternedString::from_raw(1000000 + i);
        let node_id = NodeId::new(i as u64).unwrap();
        let properties = PropertyMapBuilder::new().build();

        let op = WalOperation::CreateNode {
            node_id,
            label: invalid_label,
            properties,
            valid_from: time::now(),
        };

        let result = wal.append(op);
        assert!(
            result.is_err(),
            "Expected append to fail for invalid ID {}",
            1000000 + i
        );

        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("InternedString"),
            "Unexpected error: {}",
            err_msg
        );
    }
}
