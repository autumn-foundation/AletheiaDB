use aletheiadb::core::id::NodeId;
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::wal::WalOperation;
use aletheiadb::storage::wal::concurrent::{ConcurrentWal, ConcurrentWalConfig};
use aletheiadb::storage::wal::flush_coordinator::{FlushCoordinator, FlushCoordinatorConfig};
use std::sync::Arc;
use tempfile::tempdir;

// RAII guard to reset permissions on drop
#[cfg(unix)]
struct PermissionsGuard<'a> {
    path: &'a std::path::Path,
    original_mode: u32,
}

#[cfg(unix)]
impl<'a> Drop for PermissionsGuard<'a> {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(self.path) {
            let mut perms = metadata.permissions();
            perms.set_mode(self.original_mode);
            // Ignore result as we can't do much about it in drop
            let _ = std::fs::set_permissions(self.path, perms);
        }
    }
}

#[test]
#[cfg(unix)]
fn test_havoc_flush_failure_deadlock() {
    use std::os::unix::fs::PermissionsExt;

    // Skip if running as root, as root bypasses permission checks
    // We check this by trying to create a file in a read-only directory
    // or simply checking the UID if available. Since we don't want to add `libc`
    // dependency just for this, we use a behavioral check.
    // Note: behavioral check is safer than UID check anyway (e.g. capabilities).

    // For now, we assume standard environment. If this fails in root CI, we can refine.
    // Actually, let's just warn if we suspect root privileges might interfere.

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
    let original_mode = std::fs::metadata(wal_dir).unwrap().permissions().mode();
    let _guard = {
        let mut perms = std::fs::metadata(wal_dir).unwrap().permissions();
        perms.set_mode(0o500); // Read/Execute, no Write
        std::fs::set_permissions(wal_dir, perms).unwrap();
        PermissionsGuard {
            path: wal_dir,
            original_mode,
        }
    };

    // Verify fault injection actually works (skip test if running as root/privileged)
    let probe_file = wal_dir.join("probe_write_permission");
    if std::fs::File::create(&probe_file).is_ok() {
        println!("SKIPPING: Running with privileges that bypass read-only check (e.g. root)");
        return;
    }

    // Append operation that will need to be flushed
    let (_lsn2, handle2) = wal.append_with_handle(op.clone()).unwrap();

    // Drain
    let entries = wal.drain_all();
    assert!(!entries.is_empty());

    // Flush. This should try to rotate/create new segment and fail due to permissions.
    let result = coordinator.flush(entries, true);

    assert!(
        result.is_err(),
        "Flush should fail due to read-only directory"
    );

    // DEADLOCK CHECK:
    // If the bug exists, handle2 is NOT complete, and wait() would block forever.
    // With the fix, handle2 should be complete (with error).

    if !handle2.is_complete() {
        panic!("DEADLOCK DETECTED: Flush failed but waiting threads were not notified!");
    }

    assert!(
        handle2.wait().is_err(),
        "Wait should return the flush error"
    );
}
