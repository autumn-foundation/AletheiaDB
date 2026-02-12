use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::temporal::time;
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::storage::wal::concurrent::{ConcurrentWal, ConcurrentWalConfig};
use aletheiadb::storage::wal::flush_coordinator::{FlushCoordinator, FlushCoordinatorConfig};
use aletheiadb::storage::wal::WalOperation;
use std::sync::Arc;
use tempfile::tempdir;

#[test]
#[cfg(unix)]
fn test_havoc_flush_failure_deadlock() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let wal_dir = dir.path();

    // Configure WAL with small segments to force frequent file operations
    let config = ConcurrentWalConfig::new(wal_dir)
        .with_segment_size(1024)
        .with_num_stripes(4);

    // Create WAL
    let wal = Arc::new(ConcurrentWal::new(config).unwrap());

    // Create FlushCoordinator
    // Use small segment size to force rotation
    let mut flush_config = FlushCoordinatorConfig::new(wal_dir);
    flush_config.segment_size = 1024;
    let coordinator = Arc::new(FlushCoordinator::new(flush_config).unwrap());

    // Write some successful data first
    let op = WalOperation::CreateNode {
        node_id: NodeId::new(1).unwrap(),
        label: GLOBAL_INTERNER.intern("Test").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
    };

    let (_lsn, handle) = wal.append_with_handle(op.clone()).unwrap();

    // Manually drain and flush
    let entries = wal.drain_all();
    coordinator.flush(entries, true).unwrap();
    assert!(handle.is_complete());

    // Append enough data to fill the current segment (>1024 bytes)
    // WalEntry overhead is ~50 bytes. 100 entries should be enough.
    for _ in 0..100 {
        wal.append_async(op.clone()).unwrap();
    }

    let entries = wal.drain_all();
    let stats = coordinator.flush(entries, true).unwrap();

    // Check if we wrote enough to trigger rotation next time
    // segment_rotated might be true if it rotated *after* writing this batch
    println!("Flush stats: {:?}", stats);

    // Induce failure: Make directory read-only
    let mut perms = std::fs::metadata(wal_dir).unwrap().permissions();
    perms.set_mode(0o500); // Read/Execute, no Write
    std::fs::set_permissions(wal_dir, perms).unwrap();

    // Append operation that will need to be flushed
    let (_lsn2, handle2) = wal.append_with_handle(op.clone()).unwrap();

    // Drain
    let entries = wal.drain_all();
    assert!(!entries.is_empty());

    // Flush. This should try to rotate/create new segment and fail due to permissions.
    // Note: If previous flush didn't rotate, this might succeed if it appends to open file.
    // But we wrote >1KB, so it should have marked for rotation.
    // The `maybe_rotate_segment` logic: if size >= limit, rotate.
    // Rotation involves opening new file.
    // If it rotated in previous flush, new file is open? No, rotation closes old and prepares state.
    // `ensure_segment_open` is called at start of `flush`.
    // It should try to open new segment.

    let result = coordinator.flush(entries, true);

    // Reset permissions so cleanup works
    let mut perms = std::fs::metadata(wal_dir).unwrap().permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(wal_dir, perms).unwrap();

    assert!(result.is_err(), "Flush should fail due to read-only directory");

    // DEADLOCK CHECK:
    // If the bug exists, handle2 is NOT complete, and wait() would block forever.
    // With the fix, handle2 should be complete (with error).

    if !handle2.is_complete() {
        panic!("DEADLOCK DETECTED: Flush failed but waiting threads were not notified!");
    }

    assert!(handle2.wait().is_err(), "Wait should return the flush error");
}
