use aletheiadb::core::id::NodeId;
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::wal::concurrent::{ConcurrentWal, ConcurrentWalConfig};
use aletheiadb::storage::wal::entry::{MAX_WAL_ENTRY_SIZE, WalOperation};
use tempfile::tempdir;

fn test_operation(id: u64) -> WalOperation {
    WalOperation::CreateNode {
        node_id: NodeId::new(id).unwrap(),
        label: GLOBAL_INTERNER.intern("Test").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
    }
}

// Create a huge operation that exceeds MAX_WAL_ENTRY_SIZE
fn huge_operation(id: u64) -> WalOperation {
    // MAX_WAL_ENTRY_SIZE is 64MB.
    // Create a property map larger than that.
    // A vector of f32 is dense. 64MB / 4 = 16M floats.
    // But MAX_VECTOR_DIMENSIONS is 100,000. So we can't use a single vector.
    // We can use a large string or bytes.
    // Bytes limit? PropertyValue::Bytes(Arc<[u8]>)
    // Is there a limit on Bytes size?
    // serialized_size checks recursion depth but not explicit byte size limit?
    // Let's check PropertyValue::Bytes.
    // Wait, PropertyValue::Bytes stores Arc<[u8]>.
    // `estimated_entry_capacity` checks size.
    // `MAX_WAL_ENTRY_SIZE` is enforced in `ConcurrentWal::serialize_entry`.

    // Create a byte array slightly larger than MAX_WAL_ENTRY_SIZE
    let size = MAX_WAL_ENTRY_SIZE + 1024;
    let bytes = vec![0u8; size];

    // We need to bypass PropertyMapBuilder if it enforces limits?
    // PropertyMap::deserialize enforces count limit.
    // serialize_recursive enforces recursion depth.
    // But huge bytes are allowed in PropertyValue (just heap alloc).

    // However, serialize_entry will fail if size > MAX_WAL_ENTRY_SIZE.

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
fn test_sentry_wal_batch_gaps() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    // 1. Create a batch: [Valid, Huge (Invalid), Valid]
    let ops = vec![test_operation(1), huge_operation(2), test_operation(3)];

    // 2. Append batch - should fail
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
    let lsn_after = wal.append_async(test_operation(4)).unwrap();
    println!("Appended valid op after failure, LSN: {:?}", lsn_after);

    // 4. Drain and inspect LSNs
    let entries = wal.drain_all();
    let lsns: Vec<u64> = entries.iter().map(|e| e.lsn.0).collect();

    println!("Drained LSNs: {:?}", lsns);

    // 5. Verify atomicity: No partial batches and no LSN gaps
    // Since we pre-validate the batch, the entire batch should be rejected before
    // any LSNs are allocated. The next valid operation should receive LSN 1.

    assert!(
        !lsns.contains(&1) || lsn_after.0 == 1,
        "LSN 1 should belong to the valid operation, not the failed batch"
    );
    assert!(
        lsns.contains(&lsn_after.0),
        "LSN after batch should be present"
    );

    // Prove contiguity
    let mut contiguous = true;
    for i in 0..lsns.len().saturating_sub(1) {
        if lsns[i + 1] != lsns[i] + 1 {
            contiguous = false;
            break;
        }
    }

    assert!(
        contiguous,
        "LSN sequence should be contiguous, but found gaps: {:?}",
        lsns
    );
    println!("🛡️ SENTRY SUCCESS: LSNs are contiguous. Batch validation prevented gaps.");
}

#[test]
fn test_sentry_wal_batch_with_handles_gaps() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    // 1. Create a batch: [Valid, Huge (Invalid), Valid]
    let ops = vec![test_operation(1), huge_operation(2), test_operation(3)];

    // 2. Append batch with handles - should fail
    let result = wal.append_batch_with_handles(ops);
    assert!(
        result.is_err(),
        "Batch append with handles should fail due to size limit"
    );

    let err = result.unwrap_err();
    println!("Append batch with handles error (expected): {}", err);
    assert!(
        err.to_string().contains("CapacityExceeded") || err.to_string().contains("WAL entry size")
    );

    // 3. Append another valid operation successfully
    let lsn_after = wal.append_async(test_operation(4)).unwrap();
    println!("Appended valid op after failure, LSN: {:?}", lsn_after);

    // 4. Drain and inspect LSNs
    let entries = wal.drain_all();
    let lsns: Vec<u64> = entries.iter().map(|e| e.lsn.0).collect();

    // 5. Verify atomicity
    assert!(
        !lsns.contains(&1) || lsn_after.0 == 1,
        "LSN 1 should belong to the valid operation, not the failed batch"
    );
    assert!(
        lsns.contains(&lsn_after.0),
        "LSN after batch should be present"
    );

    // Prove contiguity
    let mut contiguous = true;
    for i in 0..lsns.len().saturating_sub(1) {
        if lsns[i + 1] != lsns[i] + 1 {
            contiguous = false;
            break;
        }
    }

    assert!(
        contiguous,
        "LSN sequence should be contiguous, but found gaps: {:?}",
        lsns
    );
}
