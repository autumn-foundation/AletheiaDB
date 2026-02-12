use aletheiadb::storage::wal::entry::LSN;
use aletheiadb::storage::wal::flush_coordinator::{FlushCoordinator, FlushCoordinatorConfig};
use aletheiadb::storage::wal::ring_buffer::PendingEntry;
use std::sync::Arc;
use std::thread;

#[test]
#[cfg(unix)]
fn test_flush_deadlock_on_io_error() {
    use std::os::unix::fs::PermissionsExt;

    // 1. Create temp dir
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();

    // 2. Create FlushCoordinator
    let config = FlushCoordinatorConfig::new(dir_path.clone());
    let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

    // 3. Make directory read-only to force I/O error on file creation
    let mut perms = std::fs::metadata(&dir_path).unwrap().permissions();
    perms.set_mode(0o444); // Read-only
    std::fs::set_permissions(&dir_path, perms).unwrap();

    // 4. Create a PendingEntry with completion handle
    let (entry, handle) = PendingEntry::new_sync(
        LSN(1),
        vec![1, 2, 3], // Dummy data
    );

    // 5. Spawn a thread to wait for completion (to detect hang)
    let handle_clone = handle.clone();
    let waiter = thread::spawn(move || {
        // This should return Err (due to I/O failure) or Ok (if flush somehow succeeded)
        // But it MUST NOT hang.
        handle_clone.wait()
    });

    // 6. Call flush (this will fail)
    let result = coordinator.flush(vec![entry], true);

    // Assert that flush failed
    assert!(result.is_err(), "Flush should fail due to I/O error");

    // 7. Wait for waiter with timeout
    // Join with timeout not supported directly on JoinHandle, so we use a channel or just rely on test timeout?
    // Let's assume the test runner has a timeout. But to be safe and fast:

    // We can just join the thread. If it hangs, the test times out.
    // Ideally we'd use a channel to signal completion.

    let join_result = waiter.join();

    // Restore permissions so tempdir can be cleaned up
    let mut perms = std::fs::metadata(&dir_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&dir_path, perms).unwrap();

    // Verify waiter finished
    assert!(join_result.is_ok(), "Waiter thread panicked or hung");

    // Verify the result from wait()
    let wait_result = join_result.unwrap();
    // It should be an error because flush failed
    assert!(wait_result.is_err(), "Waiter should receive error notification");
}
