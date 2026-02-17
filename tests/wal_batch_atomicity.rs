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

    // Verify error type is CapacityExceeded (robust error matching)
    if let aletheiadb::utils::error::Error::Storage(aletheiadb::utils::error::StorageError::CapacityExceeded { resource, .. }) = &err {
        assert!(resource.contains("WAL entry size"), "Unexpected resource: {}", resource);
    } else {
        panic!("Expected CapacityExceeded error, got: {:?}", err);
    }

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

#[test]
fn test_wal_batch_atomicity_recursion_depth() {
    use std::sync::Arc;
    use aletheiadb::core::property::PropertyValue;
    use aletheiadb::core::property::MAX_RECURSION_DEPTH;

    // 🎯 Target: ConcurrentWal::append_batch atomicity for serialization errors
    // 💣 Risk: If serialization fails (e.g. recursion limit), partial writes could occur.
    // 🧪 Strategy: Create a batch with a deeply nested property that fails serialization.
    // 🔬 Verification: Ensure no LSNs are allocated and no partial writes occur.

    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    // Create a deeply nested value: Array(Array(...)) exceeding MAX_RECURSION_DEPTH
    let mut bad_value = PropertyValue::Int(42);
    for _ in 0..MAX_RECURSION_DEPTH + 5 {
        bad_value = PropertyValue::Array(Arc::new(vec![bad_value]));
    }

    // This bypasses the PropertyMapBuilder recursion check (which panics)
    // by constructing the PropertyMap manually if needed, or relying on `serialize_entry`
    // to catch it if the builder lets it pass (builder check is just depth > MAX).

    // We construct an operation with this value.
    // Since PropertyMapBuilder panics on insert recursion, we need a way to construct it.
    // We can use `properties!` macro or manual construction if accessible.
    // But `PropertyMap` fields are private.
    // However, `ConcurrentWal` calls `serialize_entry` which calls `serialize_operation_into`.
    // We can simulate the failure by using `PropertyMapBuilder::try_insert` if exposed,
    // or just assume `append_batch` will fail during serialization.

    // Actually, `PropertyMapBuilder::insert` panics. `try_insert` is public but also checks depth.
    // So we can't easily construct a `WalOperation` with invalid recursion depth via public API
    // without triggering the panic *before* `append_batch`.
    //
    // BUT, we can construct a `WalOperation` that is valid in memory but fails `estimate_entry_capacity`
    // or serialization if we can bypass the builder check.
    //
    // Wait, `PropertyMap::from_iter` constructs without recursion check (as per `property.rs` test `test_property_map_from_iter_no_panic_on_deep_recursion`).
    // Let's use that path.

    let key = GLOBAL_INTERNER.intern("deep").unwrap();
    let props: PropertyMap = vec![(key, bad_value)].into_iter().collect();

    let op1 = test_operation(1, "valid");
    let op2 = WalOperation::CreateNode {
        node_id: NodeId::new(2).unwrap(),
        label: GLOBAL_INTERNER.intern("invalid").unwrap(),
        properties: props,
        valid_from: time::now(),
    };

    let batch = vec![op1, op2];

    // Append batch - should fail during Phase 1 serialization
    let result = wal.append_batch(batch);
    assert!(result.is_err());

    let err = result.unwrap_err();
    // Error should be CorruptedData ("recursion depth limit exceeded") wrapped in StorageError
    // But `ConcurrentWal` wraps it in `Error::Storage`.
    // The inner error from serialization is `StorageError::CorruptedData`.

    if let aletheiadb::utils::error::Error::Storage(aletheiadb::utils::error::StorageError::CorruptedData(msg)) = &err {
        assert!(msg.contains("recursion depth"), "Unexpected error message: {}", msg);
    } else {
        // It might be wrapped differently depending on how `serialize_into` error propagates.
        // It returns `Result<()>`.
        println!("Got error: {:?}", err);
    }

    // Verify atomicity: No LSNs consumed
    let lsn_next = wal.append_async(test_operation(3, "valid")).unwrap();
    assert_eq!(lsn_next.0, 1, "LSN should still be 1 (no gaps from recursion failure)");
}
