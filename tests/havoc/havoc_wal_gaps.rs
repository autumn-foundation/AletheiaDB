use aletheiadb::core::id::NodeId;
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::storage::wal::concurrent::{ConcurrentWal, ConcurrentWalConfig};
use aletheiadb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
use aletheiadb::storage::wal::entry::{LSN, MAX_WAL_ENTRY_SIZE, WalOperation};
use tempfile::tempdir;

fn test_operation(id: u64) -> WalOperation {
    WalOperation::CreateNode {
        node_id: NodeId::new(id).unwrap(),
        label: GLOBAL_INTERNER.intern("Test").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
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
        provenance: None,
    }
}

// ---------------------------------------------------------------------------
// DELIBERATE BEHAVIOR CHANGE (Issue #3414): serialize-first batch append.
//
// This test USED TO PIN the buggy partial-append behavior: a batch whose
// middle entry failed to serialize left the entries appended *before* the
// failure sitting in the ring buffer, where a later flush swept them to disk
// and recovery replayed them -- a batch the caller was told FAILED
// resurrected as a prefix. The old assertions literally required LSN 1 to be
// present and panicked ("Boring") if the sequence was contiguous.
//
// #3414 makes `append_batch` serialize EVERY entry up front (the fallible
// phase) and only begins appending once all entries have serialized
// successfully. A serialization failure therefore leaves ZERO WAL residue.
// This test is flipped ON PURPOSE to pin that new zero-residue contract: no
// entry of the failed batch may appear in the ring buffer.
//
// NOTE: the tests in this file pin the SERIALIZE-failure case specifically
// (an oversized entry rejected in phase 1). A phase-2 append failure --
// reachable only if a stripe is closed mid-batch during teardown -- is a
// distinct, narrower case deferred to WAL transaction framing (#3413).
// ---------------------------------------------------------------------------
#[test]
fn test_havoc_wal_batch_gaps() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    // 1. Create a batch: [Valid, Huge (Invalid), Valid]
    let ops = vec![test_operation(1), huge_operation(2), test_operation(3)];

    // 2. Append batch - should fail (oversized middle entry)
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

    // 5. Verify the NEW contract: the failed batch left ZERO residue.
    //
    // `append_batch` reserves the LSN range (1, 2, 3) atomically, then
    // serializes all three entries BEFORE appending any. Serializing the
    // oversized middle entry fails, so the whole batch is abandoned without a
    // single append. The reserved-but-unwritten LSN range is benign -- the
    // single-op `append_async` path already produces the identical hole on a
    // serialize failure, and recovery seeds the allocator from the max
    // *written* LSN. `append_async(op4)` therefore lands at LSN 4.
    assert!(
        !lsns.contains(&1),
        "LSN 1 must be ABSENT: no prefix of the failed batch may be appended (was present pre-#3414)"
    );
    assert!(
        !lsns.contains(&2),
        "LSN 2 must be absent (failed serialization)"
    );
    assert!(!lsns.contains(&3), "LSN 3 must be absent (batch abandoned)");
    assert!(
        lsns.contains(&lsn_after.0),
        "the post-failure valid append must be present"
    );

    // The only drained entry is the post-failure op -- the batch is invisible.
    assert_eq!(
        lsns.len(),
        1,
        "exactly one entry (the post-failure op) should be present; the failed batch left zero residue, got {:?}",
        lsns
    );

    println!("✅ ZERO-RESIDUE: failed batch left no entries in the ring buffer.");
}

// ---------------------------------------------------------------------------
// Max-residue variant (Issue #3414): the OVERSIZED entry is LAST.
//
// `test_havoc_wal_batch_gaps` fails the MIDDLE entry, so buggy pre-#3414 code
// would have leaked a single-entry prefix (LSN 1). This test fails the LAST
// entry of a three-entry batch -- the worst case for the old serialize-in-loop
// bug, which would have appended the entire N-1 prefix (LSNs 1 AND 2) before
// hitting the oversized entry. Pinning the last-entry position proves the
// serialize-first fix abandons the WHOLE batch, not just a trailing suffix:
// zero residue regardless of how far into the batch serialization fails.
// ---------------------------------------------------------------------------
#[test]
fn test_havoc_wal_batch_last_entry_failure_no_residue() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    // Batch: [Valid(1), Valid(2), Huge(3) - invalid LAST entry].
    // On buggy serialize-in-loop code this would append LSN 1 and LSN 2 (the
    // full N-1 prefix) before the oversized entry aborts the loop.
    let ops = vec![test_operation(1), test_operation(2), huge_operation(3)];

    let result = wal.append_batch(ops);
    assert!(
        result.is_err(),
        "batch with an oversized LAST entry must fail"
    );

    let err = result.unwrap_err();
    println!("Append batch error (expected): {}", err);
    assert!(
        err.to_string().contains("CapacityExceeded") || err.to_string().contains("WAL entry size")
    );

    // A later valid op must still succeed and be the ONLY visible entry.
    let lsn_after = wal.append_async(test_operation(4)).unwrap();
    println!("Appended valid op after failure, LSN: {:?}", lsn_after);

    let entries = wal.drain_all();
    let lsns: Vec<u64> = entries.iter().map(|e| e.lsn.0).collect();
    println!("Drained LSNs: {:?}", lsns);

    // The max-residue assertion: NEITHER LSN 1 NOR LSN 2 (the full N-1 prefix
    // the buggy code would have leaked) may be present.
    assert!(
        !lsns.contains(&1),
        "LSN 1 must be ABSENT: no prefix of the failed batch may be appended (buggy code leaked LSNs 1,2)"
    );
    assert!(
        !lsns.contains(&2),
        "LSN 2 must be ABSENT: the full N-1 prefix must be abandoned, not just the failed suffix"
    );
    assert!(
        !lsns.contains(&3),
        "LSN 3 must be absent (failed serialization)"
    );
    assert!(
        lsns.contains(&lsn_after.0),
        "the post-failure valid append must be present"
    );
    assert_eq!(
        lsns.len(),
        1,
        "exactly one entry (the post-failure op) should be present; the failed batch left zero residue, got {:?}",
        lsns
    );

    println!("✅ ZERO-RESIDUE (last-entry failure): full N-1 prefix abandoned.");
}

// ---------------------------------------------------------------------------
// Real on-disk replay regression test (Issue #3414).
//
// This is the crash-recovery-faithful version of the bug: it flushes to real
// on-disk WAL segments and then reads them back through the same segment
// reader that crash recovery uses (`read_from`). On the buggy (pre-#3414)
// code the prefix entry (node 1) that the failed batch orphaned in the ring
// buffer gets swept to disk by the *next* unrelated flush and reappears on
// replay. After #3414 the failed batch never appends anything, so replay sees
// only writes that actually succeeded.
// ---------------------------------------------------------------------------
#[test]
fn test_havoc_wal_batch_no_residue_on_replay() {
    let dir = tempdir().unwrap();
    let wal_path = dir.path().join("wal");
    std::fs::create_dir_all(&wal_path).unwrap();

    // Synchronous durability: a successful write is flushed to on-disk
    // segments immediately, so `read_from` observes exactly what crash
    // recovery would replay.
    let config =
        ConcurrentWalSystemConfig::new(&wal_path).with_durability_mode(DurabilityMode::Synchronous);
    let system = ConcurrentWalSystem::new(config).unwrap();

    // Batch: [Valid(1), Huge(2) - invalid, Valid(3)]
    let ops = vec![test_operation(1), huge_operation(2), test_operation(3)];
    let result = system.append_batch(ops);
    assert!(
        result.is_err(),
        "batch containing an oversized entry must fail"
    );

    // A later, genuinely valid write that MUST persist and replay. On the
    // buggy code this flush is what sweeps the orphaned prefix (node 1) to
    // disk.
    let lsn_after = system.append_sync(test_operation(4)).unwrap();
    println!("post-failure durable write at LSN {:?}", lsn_after);

    // Force REAL replay: read entries back from the on-disk WAL segments,
    // exactly as crash recovery does (no index snapshot exists at this layer
    // to short-circuit it).
    let replayed = system.read_from(LSN(1)).unwrap();
    let replayed_ids: Vec<u64> = replayed
        .iter()
        .filter_map(|e| match &e.operation {
            WalOperation::CreateNode { node_id, .. } => Some(node_id.as_u64()),
            _ => None,
        })
        .collect();
    println!("replayed node ids: {:?}", replayed_ids);

    // No entry of the FAILED batch may survive replay.
    for failed_id in [1u64, 2, 3] {
        assert!(
            !replayed_ids.contains(&failed_id),
            "node {} from the FAILED batch must NOT be replayed (found {:?})",
            failed_id,
            replayed_ids
        );
    }

    // The post-failure write is durable and replayed.
    assert!(
        replayed_ids.contains(&4),
        "the post-failure durable write (node 4) must be replayed, found {:?}",
        replayed_ids
    );

    println!("✅ ZERO-RESIDUE ON REPLAY: failed batch left no on-disk entries.");
}
