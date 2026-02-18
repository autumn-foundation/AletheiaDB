use aletheiadb::storage::wal::group_commit::{GroupCommitConfig, GroupCommitCoordinator};
use aletheiadb::core::error::Error;
use aletheiadb::core::error::StorageError;
use std::sync::{Arc, Barrier};
use std::thread;

// 👺 Havoc: Proving that Group Commit is fragile.
// These tests are designed to FAIL if the bug exists.
// A failing test proves I have found a weakness.

#[test]
fn havoc_repro_group_commit_false_success() {
    // SCENARIO: Data Loss
    // A transaction thinks it's durable, but it failed.
    //
    // 1. T1 registers for Epoch 1.
    // 2. Flush 1 fails.
    // 3. T2 registers for Epoch 2.
    // 4. Flush 2 succeeds.
    // 5. T1 checks status.
    //
    // EXPECTED: T1 should receive an error.
    // ACTUAL (BUG): T1 receives Ok.

    let coord = Arc::new(GroupCommitCoordinator::new(100, 100));
    let barrier = Arc::new(Barrier::new(2));

    // 1. Register T1 (Epoch 1)
    let (epoch1, _) = coord.register_transaction().unwrap();
    assert_eq!(epoch1, 1);

    // Spawn T1 waiter
    let coord_clone = Arc::clone(&coord);
    let barrier_clone = Arc::clone(&barrier);
    let t1_handle = thread::spawn(move || {
        // Wait for main thread to perform flushes
        barrier_clone.wait();
        // Check status
        coord_clone.wait_for_flush(epoch1)
    });

    // 2. Mark Epoch 1 as FAILED
    // Use start/finish explicitly to be safe, though mark_flushed is available
    let f1 = coord.start_flush().unwrap();
    coord
        .finish_flush(
            f1,
            Err(Error::Storage(StorageError::WalError {
                reason: "Disk Full (Epoch 1)".to_string(),
            })),
        )
        .unwrap();

    // 3. Register T2 (Epoch 2)
    // We register to ensure the coordinator moves to Epoch 2 state fully
    let (epoch2, _) = coord.register_transaction().unwrap();
    assert_eq!(epoch2, 2);

    // 4. Mark Epoch 2 as SUCCESS
    let f2 = coord.start_flush().unwrap();
    coord.finish_flush(f2, Ok(())).unwrap();

    // Signal T1 to proceed
    barrier.wait();

    // 5. Join T1 and Verify
    let result = t1_handle.join().unwrap();

    // THIS ASSERTION SHOULD FAIL if the bug exists.
    assert!(
        result.is_err(),
        "👺 Havoc: Data Loss Detected! Transaction returned Ok, but Epoch 1 flush failed."
    );
}

#[test]
fn havoc_repro_group_commit_false_failure() {
    // SCENARIO: Ghost Error
    // A successful transaction reports failure because a LATER transaction failed.
    //
    // 1. T1 registers for Epoch 1.
    // 2. Flush 1 succeeds.
    // 3. T2 registers for Epoch 2.
    // 4. Flush 2 fails.
    // 5. T1 calls wait_for_flush(1).
    //
    // EXPECTED: T1 should receive Ok.
    // ACTUAL (BUG): T1 receives Err.

    let config = GroupCommitConfig {
        recent_errors_capacity: 100,
        ..GroupCommitConfig::default()
    };
    // We need to use with_config to set the capacity properly if we want to rely on history
    let coord = Arc::new(GroupCommitCoordinator::with_config(config));

    // 1. Register T1 (Epoch 1)
    let (epoch1, _) = coord.register_transaction().unwrap();
    assert_eq!(epoch1, 1);

    // 2. Mark Epoch 1 as SUCCESS
    // Use start/finish explicitly
    let f1 = coord.start_flush().unwrap();
    coord.finish_flush(f1, Ok(())).unwrap();

    // 3. Register T2 (Epoch 2)
    let (epoch2, _) = coord.register_transaction().unwrap();
    assert_eq!(epoch2, 2);

    // 4. Mark Epoch 2 as FAILED
    let f2 = coord.start_flush().unwrap();
    coord
        .finish_flush(
            f2,
            Err(Error::Storage(StorageError::WalError {
                reason: "Disk Full (Epoch 2)".to_string(),
            })),
        )
        .unwrap();

    // 5. T1 checks status
    let result = coord.wait_for_flush(epoch1);

    // THIS ASSERTION SHOULD FAIL if the bug exists.
    // The previous implementation returned Err because T1's epoch (1) was evicted.
    // With recent_errors_capacity=100, epoch 1 is still in history (we only flushed epoch 2).
    // Wait, in this scenario:
    // Epoch 1: OK
    // Epoch 2: Fail
    // Since Epoch 1 succeeded, it is NOT in recent_errors.
    // However, if we don't have oldest_error_epoch tracking, we might think it was evicted if history is empty or starts later.
    // BUT here, history has [ (2, "Disk Full") ].
    // Oldest error is 2.
    // We are looking for 1.
    // 1 < 2.
    // So if we just check `epoch < oldest_in_history`, we return Err.
    // This is CORRECT behavior if we assume strict ordering and eviction.
    // BUT wait, Epoch 1 succeeded. It was never an error.
    // The issue is distinguishing "Succeeded and not in error list" from "Failed and evicted from error list".
    //
    // If we maintain `oldest_error_epoch` (which defaults to 0), and we haven't evicted anything:
    // oldest_error_epoch = 0.
    // recent_errors = [(2, "Fail")].
    // We check epoch 1.
    // 1 < 2 (oldest in list).
    // This triggers the "evicted" check if we only look at the list front.
    //
    // CORRECTION in logic:
    // We only lose information if we *popped* from the deque.
    // `oldest_error_epoch` tracks the highest popped epoch + 1.
    // Initial state: oldest_error_epoch = 0.
    // After Epoch 2 fail: recent_errors = [(2, "Fail")]. Nothing popped. oldest_error_epoch = 0.
    // Check Epoch 1:
    // 1. Is 1 in recent_errors? No.
    // 2. Is 1 < oldest_error_epoch (0)? No.
    // 3. Is 1 < recent_errors.front().0 (2)? Yes.
    //
    // PROBLEM: If 1 < 2, does it mean 1 succeeded or 1 failed and was evicted?
    // If we haven't evicted anything, then 1 must have succeeded (or be pending).
    // We know we haven't evicted anything because `state.oldest_error_epoch` is 0.
    // So we should ONLY check against `state.oldest_error_epoch`.
    // Checking against `recent_errors.front()` is aggressive and incorrect if we have sparse errors.
    //
    // The code I wrote in `group_commit.rs` was:
    // if !state.recent_errors.is_empty() {
    //    if let Some((first_failed_epoch, _)) = state.recent_errors.front() {
    //        if epoch < *first_failed_epoch { return Err(...) }
    //    }
    // }
    //
    // This logic assumes that if we have an error for epoch N, we have "forgotten" everything before N.
    // That assumption is WRONG for a sparse error list.
    // We should ONLY check `epoch < state.oldest_error_epoch`.
    //
    // I need to fix `src/storage/wal/group_commit.rs` first.
    // But for this test, I will temporarily comment out this assertion logic to fix the file in next step.
    // Actually, I should just fix the file now.

    assert!(
        result.is_ok(),
        "👺 Havoc: False Failure Detected! Successful transaction returned Error: {:?}",
        result.err()
    );
}
