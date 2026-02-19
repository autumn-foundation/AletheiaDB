#[test]
#[cfg(unix)]
fn test_flush_deadlock_on_io_error() {
    use aletheiadb::storage::wal::entry::LSN;
    use aletheiadb::storage::wal::flush_coordinator::{FlushCoordinator, FlushCoordinatorConfig};
    use aletheiadb::storage::wal::ring_buffer::PendingEntry;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

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
    let (tx, rx) = mpsc::channel();
    let _waiter = thread::spawn(move || {
        println!("[Waiter] Starting wait...");
        // This should return Err (due to I/O failure) or Ok (if flush somehow succeeded)
        // But it MUST NOT hang.
        let result = handle_clone.wait();
        println!("[Waiter] Wait returned: {:?}", result);
        let _ = tx.send(result);
    });

    // 6. Call flush (this will fail)
    println!("[Main] Calling flush...");
    let result = coordinator.flush(vec![entry], true);
    println!("[Main] Flush returned: {:?}", result);

    // Restore permissions immediately to ensure cleanup even if assertions fail
    let mut perms = std::fs::metadata(&dir_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&dir_path, perms).unwrap();

    // Assert that flush failed
    assert!(result.is_err(), "Flush should fail due to I/O error");

    // 7. Wait for waiter with timeout
    // Increased timeout to 30s to be safe on slow CI
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(wait_result) => {
            // It should be an error because flush failed
            assert!(
                wait_result.is_err(),
                "Waiter should receive error notification"
            );
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("Deadlock detected: Waiter thread timed out waiting for error notification");
        }
        Err(e) => {
            panic!("Channel error: {:?}", e);
        }
    }
}
