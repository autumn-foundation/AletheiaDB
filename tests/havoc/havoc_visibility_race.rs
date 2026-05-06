use aletheiadb::api::transaction::types::TxId;
use aletheiadb::api::transaction::visibility::TxVisibilityManager;
use std::sync::{Arc, Barrier};
use std::thread;

/// HAVOC: NON-REPEATABLE READ RACE (Issue #238 regression test)
///
/// The OLD design had a race window in `register_commit`:
/// 1. `active.lock()` acquired → tx removed from active set → lock released.
/// 2. `committed.write()` acquired → tx inserted into committed map → lock released.
///
/// Between steps 1 and 2, a reader could snapshot a state where the tx was in
/// neither set. Calling `is_visible` twice on the same snapshot could return
/// `false` then `true` — a non-repeatable read.
///
/// The NEW design (Issue #238) eliminates the committed map entirely. Commit
/// timestamps are embedded in each version struct. `register_commit` only
/// removes the tx from `active` (single lock, atomic transition). Visibility
/// is determined by an immutable `commit_timestamp` carried by the version, so
/// there is no shared mutable state that could produce a non-repeatable read.
///
/// This test verifies that repeated visibility checks on the same snapshot + the
/// same embedded commit timestamp always agree.
#[test]
fn havoc_visibility_non_repeatable_read() {
    let manager = Arc::new(TxVisibilityManager::new());

    let iterations = 100_000;
    let mut race_detected = false;

    for i in 0..iterations {
        let mgr1 = manager.clone();
        let mgr2 = manager.clone();

        let tx = TxId::new(i as u64 + 1);
        mgr1.register_active(tx);

        let barrier = Arc::new(Barrier::new(2));
        let b1 = barrier.clone();
        let b2 = barrier.clone();

        // commit_timestamp is now embedded in the version, not stored in the manager.
        let commit_ts: aletheiadb::core::temporal::Timestamp = (i as i64 * 10).into();

        let t1 = thread::spawn(move || {
            b1.wait();
            mgr1.register_commit(tx);
        });

        let t2 = thread::spawn(move || {
            b2.wait();
            for _ in 0..10 {
                let snap = mgr2.capture_snapshot((i as i64 * 10 + 1).into());

                // With embedded timestamps both calls use the same immutable inputs
                // (the snapshot and the commit_ts from the version struct), so they
                // must always agree.
                let first_read = mgr2.is_visible_with_embedded_ts(&snap, tx, Some(commit_ts));
                let second_read = mgr2.is_visible_with_embedded_ts(&snap, tx, Some(commit_ts));

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
            "HAVOC: Non-repeatable read detected!\n\
            \n\
            Issue #238 should have eliminated this by removing the committed map.\n\
            With embedded commit timestamps both visibility checks use only:\n\
            1. The snapshot (immutable after capture)\n\
            2. The commit_timestamp (immutable after version creation)\n\
            \n\
            If this fires, the embedded-timestamp path has a shared-state bug."
        );
    }
}
