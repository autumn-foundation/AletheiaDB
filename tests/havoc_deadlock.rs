use aletheiadb::storage::wal::ring_buffer::PendingEntry;
use aletheiadb::storage::wal::LSN;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn test_deadlock_on_dropped_pending_entry() {
    // Create a synchronous entry (returns handle)
    // The handle allows waiting for completion.
    let (entry, handle) = PendingEntry::new_sync(LSN(1), vec![]);

    let (tx, rx) = mpsc::channel();

    // Spawn a thread that waits on the handle
    thread::spawn(move || {
        // This wait() calls Condvar::wait.
        // If the entry is dropped without notifying, this thread hangs forever.
        let result = handle.wait();
        // Send the result back
        let _ = tx.send(result);
    });

    // Give the thread a moment to block on the Condvar
    thread::sleep(Duration::from_millis(50));

    // Drop the entry without notifying completion/error.
    // This simulates a scenario where the WAL buffer is dropped (e.g. shutdown/panic)
    // while entries are still pending.
    drop(entry);

    // Wait for the thread to return.
    // Current behavior (Bug): It hangs forever (deadlock).
    // Expected behavior (Fix): It returns Err("Entry dropped").

    // We set a short timeout to detect the deadlock quickly.
    let result = rx.recv_timeout(Duration::from_millis(500));

    match result {
        Ok(res) => {
            // If we get a result, verify it's the expected error
            assert!(res.is_err(), "Expected error from dropped entry, got Ok");
            let err = res.unwrap_err();
            assert!(err.contains("dropped"), "Expected 'dropped' error, got: {}", err);
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Timeout occurred! This confirms the deadlock.
            panic!("Deadlock detected! handle.wait() did not return after drop(entry).");
        }
        Err(e) => panic!("Channel error: {:?}", e),
    }
}
