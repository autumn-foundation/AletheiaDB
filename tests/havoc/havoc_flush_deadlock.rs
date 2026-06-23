#[test]
#[cfg(unix)]
fn test_flush_deadlock_on_io_error() {
    use aletheiadb::storage::wal::entry::LSN;
    use aletheiadb::storage::wal::flush_coordinator::{FlushCoordinator, FlushCoordinatorConfig};
    use aletheiadb::storage::wal::ring_buffer::PendingEntry;
    use std::sync::Arc;
    use std::thread;

    // 1. Create temp dir
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();

    // 2. Create FlushCoordinator
    let config = FlushCoordinatorConfig::new(dir_path.clone());
    let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

    // 3. Replace the WAL directory with a regular file to force ENOTDIR on segment creation.
    // We cannot use chmod 0o444 because root can still create files in read-only directories.
    // ENOTDIR, however, is enforced by the kernel regardless of privilege level: no process
    // can open a path whose component is a file rather than a directory.
    std::fs::remove_dir(&dir_path).unwrap(); // dir is empty after FlushCoordinator::new()
    std::fs::File::create(&dir_path).unwrap(); // replace with a regular file

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

    // Replace the file back with an empty directory so TempDir can clean up.
    std::fs::remove_file(&dir_path).unwrap();
    std::fs::create_dir(&dir_path).unwrap();

    // Verify waiter finished
    assert!(join_result.is_ok(), "Waiter thread panicked or hung");

    // Verify the result from wait()
    let wait_result = join_result.unwrap();
    // It should be an error because flush failed
    assert!(
        wait_result.is_err(),
        "Waiter should receive error notification"
    );
}
