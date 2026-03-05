use aletheiadb::core::id::TxId;
use aletheiadb::api::transaction::visibility::TxVisibilityManager;
use std::sync::{Arc, Barrier};
use std::thread;

/// 👺 HAVOC: NON-REPEATABLE READ RACE
///
/// This test proves a critical flaw in Snapshot Isolation visibility rules.
///
/// In `TxVisibilityManager::register_commit`:
/// 1. `active.lock()` is acquired.
/// 2. The tx is removed from the active set.
/// 3. `active.lock()` is RELEASED.
/// 4. `committed.write()` is acquired.
/// 5. The tx is added to the committed set.
///
/// Between steps 3 and 4, there is a race condition window:
/// - The transaction is NOT in `active`.
/// - The transaction is NOT in `committed`.
///
/// If a reader calls `capture_snapshot` during this window, the snapshot
/// does NOT contain the tx in its `active_transactions`.
///
/// If the reader then calls `is_visible(snapshot, tx)`:
/// - First read: `committed.get(tx)` is None. Returns `false`.
/// - (Context switch: Writer thread completes step 5)
/// - Second read: `committed.get(tx)` is Some. The snapshot says the tx was
///   not active. Returns `true`.
///
/// Result: A single snapshot returns `false` then `true` for the exact same
/// data point! This violates Snapshot Isolation. The data should be consistently
/// visible or consistently invisible.
#[test]
fn havoc_visibility_non_repeatable_read() {
    let manager = Arc::new(TxVisibilityManager::new());

    let iterations = 100000;
    let mut race_detected = false;

    for i in 0..iterations {
        let mgr1 = manager.clone();
        let mgr2 = manager.clone();

        // Reset state
        let tx = TxId::new(i as u64 + 1);
        mgr1.register_active(tx);

        let barrier = Arc::new(Barrier::new(2));
        let b1 = barrier.clone();
        let b2 = barrier.clone();

        let t1 = thread::spawn(move || {
            b1.wait();
            // This drops active lock then acquires committed lock
            mgr1.register_commit(tx, (i as i64 * 10).into());
        });

        let t2 = thread::spawn(move || {
            b2.wait();
            // Try to hit the window where tx is removed from active but not in committed
            for _ in 0..10 {
                let snap = mgr2.capture_snapshot((i as i64 * 10 + 1).into());

                // First read
                let first_read = mgr2.is_visible(&snap, tx);

                // Second read using the SAME snapshot
                let second_read = mgr2.is_visible(&snap, tx);

                // If Snapshot Isolation holds, a snapshot should be immutable.
                // If it changes from false to true, we have a non-repeatable read!
                if !first_read && second_read {
                    return true;
                }
            }
            false
        });

        t1.join().unwrap();
        if t2.join().unwrap() {
            race_detected = true;
            break;
        }
    }

    if race_detected {
        panic!(
            "👺 HAVOC: Non-repeatable read detected!\n\
            \n\
            🧨 Trigger: Concurrently checking visibility while another thread commits.\n\
            \n\
            📉 The Flaw:\n\
            Snapshot Isolation is broken. A single snapshot just changed its mind about visibility.\n\
            1. First check: is_visible(tx) = false (Tx not in committed yet)\n\
            2. Second check: is_visible(tx) = true (Tx appeared in committed, snapshot says it wasn't active)\n\
            \n\
            😈 Comment:\n\
            You tried to optimize lock contention by dropping the active lock before acquiring the committed lock in register_commit.\n\
            Congratulations, you created a race window where a transaction ceases to exist. A true phantom.\n\
            Lock both, or use an atomic state transition."
        );
    }
}
