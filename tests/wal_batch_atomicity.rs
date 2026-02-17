use aletheiadb::core::id::NodeId;
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::wal::concurrent::{ConcurrentWal, ConcurrentWalConfig};
use aletheiadb::storage::wal::entry::{MAX_WAL_ENTRY_SIZE, WalOperation};
use tempfile::tempdir;

fn test_operation(id: u64, label: &str) -> WalOperation {
    WalOperation::CreateNode {
        node_id: NodeId::new(id).unwrap(),
        label: GLOBAL_INTERNER.intern(label).unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
    }
}

// Create a huge operation that exceeds MAX_WAL_ENTRY_SIZE
fn huge_operation(id: u64) -> WalOperation {
    // Create a byte array slightly larger than MAX_WAL_ENTRY_SIZE
    let size = MAX_WAL_ENTRY_SIZE + 1024;
    let bytes = vec![0u8; size];

    let props = aletheiadb::core::property::PropertyMapBuilder::new()
        .insert("huge", bytes)
        .build();

    WalOperation::CreateNode {
        node_id: NodeId::new(id).unwrap(),
        label: GLOBAL_INTERNER.intern("Huge").unwrap(),
        properties: props,
        valid_from: time::now(),
    }
}

#[test]
fn test_wal_batch_atomicity() {
    // 🎯 Target: ConcurrentWal::append_batch atomicity
    // 💣 Risk: Partial writes or "holes" in LSN sequence if validation fails mid-batch.
    // 🧪 Strategy: Submit a batch with a mix of valid and invalid entries.
    // 🔬 Verification: Ensure rejection is atomic (no LSNs consumed, no entries written).

    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    // 1. Create a batch: [Op1, Op2(Huge), Op3]
    let ops = vec![
        test_operation(1, "Op1"),
        huge_operation(2),
        test_operation(3, "Op3"),
    ];

    // 2. Append batch - should fail atomically due to size check
    let result = wal.append_batch(ops);
    assert!(
        result.is_err(),
        "Batch append should fail due to size limit"
    );

    let err = result.unwrap_err();
    println!("Append batch error (expected): {}", err);
    assert!(
        err.to_string().contains("CapacityExceeded") || err.to_string().contains("WAL entry size")
    );

    // 3. Append another valid operation successfully
    // If atomicity works, the failed batch consumed 0 LSNs.
    // So this op should get LSN 1.
    let lsn_after = wal.append_async(test_operation(4, "Op4")).unwrap();
    println!("Appended valid op after failure, LSN: {:?}", lsn_after);

    assert_eq!(
        lsn_after.0, 1,
        "LSN should be 1, verifying no LSNs were wasted by failed batch"
    );

    // 4. Drain and inspect
    let entries = wal.drain_all();
    let lsns: Vec<u64> = entries.iter().map(|e| e.lsn.0).collect();

    println!("Drained LSNs: {:?}", lsns);

    // 5. Verify consistency
    assert_eq!(lsns, vec![1], "Should have exactly one entry at LSN 1");

    // 6. Verify the content is Op4, not Op1
    // Since LSNs are unique and sequential, and we asserted LSN 1 was assigned to Op4 (returned by append_async),
    // and Op1 would have claimed LSN 1 if it were written, the fact that lsn_after is 1 proves Op1 was not written.
    // If Op1 was written (partial write), lsn_after would be > 1.
}
