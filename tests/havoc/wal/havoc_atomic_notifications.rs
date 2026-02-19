use aletheiadb::storage::wal::ring_buffer::{CompletionNotifier, PendingEntry};
use aletheiadb::storage::wal::LSN;
use std::sync::Arc;

#[test]
fn test_completion_notifier_atomicity() {
    let notifier = Arc::new(CompletionNotifier::new());

    // 1. Initial State -> Complete (Success)
    // Pending -> Complete is valid
    notifier.notify_success();
    assert!(notifier.is_complete());

    // 2. Complete -> Error (Should Fail)
    // Now that it's complete, notify_error should NOT overwrite the state.
    notifier.notify_error("Should fail to overwrite success");

    // It is not possible to inspect the internal state directly to see if it's "Error" vs "Complete"
    // via public API easily if we don't have a way to distinguish them except by behavior.
    // However, if we wait() on it, we should get Ok(()), not Err.
    assert!(notifier.wait().is_ok(), "State should remain Success");

    // 3. Reset for next test (using a new notifier)
    let notifier = Arc::new(CompletionNotifier::new());

    // 4. Initial State -> Error (Success)
    // Pending -> Error is valid
    notifier.notify_error("First error wins");
    assert!(notifier.is_complete());

    // 5. Error -> Success (Should Fail)
    // Now that it's Error, notify_success should NOT overwrite the state.
    notifier.notify_success();

    // Verify it is still Error
    let result = notifier.wait();
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "First error wins");
}

#[test]
fn test_pending_entry_drop_race_simulation() {
    // This test simulates the race condition where `notify_success` happens
    // concurrently with `drop` (which calls `notify_error`).
    //
    // Scenario:
    // 1. Flush thread calls `notify_success`. State becomes Complete.
    // 2. PendingEntry drops. `drop` calls `notify_error`.
    //
    // Expected: State remains Complete. `notify_error` fails CAS silently.

    let (entry, handle) = PendingEntry::new_sync(LSN(1), vec![]);
    let notifier = entry.completion.as_ref().unwrap().clone(); // Access internal notifier for simulation

    // Simulate concurrent flush success
    notifier.notify_success();

    // Now drop the entry.
    // In the old implementation (non-atomic), this would overwrite Success with Error.
    // In the new implementation (atomic), `notify_error` inside `drop` should fail CAS.
    drop(entry);

    // Verify state
    assert!(handle.wait().is_ok(), "Drop should not overwrite successful completion");
}
