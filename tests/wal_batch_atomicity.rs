use aletheiadb::core::id::NodeId;
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::wal::concurrent::{ConcurrentWal, ConcurrentWalConfig};
use aletheiadb::storage::wal::entry::{MAX_WAL_ENTRY_SIZE, WalOperation};

#[test]
fn test_batch_atomicity_failure() {
    let dir = tempfile::tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    let node_id = NodeId::new(1).unwrap();
    let label = GLOBAL_INTERNER.intern("test").unwrap();

    // 1. Valid Operation
    let op1 = WalOperation::CreateNode {
        node_id,
        label,
        properties: PropertyMapBuilder::new().insert("key", "val").build(),
        valid_from: time::now(),
    };

    // 2. Invalid Operation (Too Big)
    // MAX_WAL_ENTRY_SIZE is 64MB. We add a bit more to ensure it fails serialization/capacity check.
    // Note: This allocates ~64MB memory, which is acceptable for unit tests on modern machines.
    let huge_string = "a".repeat(MAX_WAL_ENTRY_SIZE + 1024);
    let op2 = WalOperation::CreateNode {
        node_id,
        label,
        properties: PropertyMapBuilder::new()
            .insert("huge", huge_string)
            .build(),
        valid_from: time::now(),
    };

    // 3. Valid Operation
    let op3 = WalOperation::CreateNode {
        node_id,
        label,
        properties: PropertyMapBuilder::new().insert("key", "val2").build(),
        valid_from: time::now(),
    };

    let batch = vec![op1, op2, op3];

    // Capture state before
    let initial_lsn = wal.current_lsn();
    assert_eq!(initial_lsn.0, 1, "Initial LSN should be 1");

    // This should fail due to size limit in the middle operation
    let result = wal.append_batch(batch);
    assert!(result.is_err(), "Batch should fail due to size limit");

    // ATOMICITY CHECK:
    // If atomic, no LSNs should have been allocated (or at least no side effects visible).
    // In the flawed implementation, LSNs are allocated in a batch at start, so current_lsn advances by 3.
    // In the fixed implementation, validation happens before allocation.
    let current = wal.current_lsn();
    assert_eq!(
        current.0, 1,
        "LSN should not advance on atomic batch failure. Current: {}",
        current.0
    );

    // GAP/PARTIAL WRITE CHECK:
    // In flawed implementation, op1 is written, op2 fails, op3 skipped.
    // So drain_all() returns 1 entry.
    // In fixed implementation, nothing is written.
    let entries = wal.drain_all();
    assert!(
        entries.is_empty(),
        "WAL should be empty on atomic batch failure. Found {} entries.",
        entries.len()
    );
}
