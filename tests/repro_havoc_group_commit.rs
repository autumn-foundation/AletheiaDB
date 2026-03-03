use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
use aletheiadb::utils::Error;
use aletheiadb::utils::StorageError;
use std::sync::{Arc, Barrier};
use std::thread;

// 👺 Havoc: Proving that Group Commit is fragile.
// These tests are designed to FAIL if the bug exists.
// A failing test proves I have found a weakness.

#[test]
#[ignore = "Reproduces known 'False Success' data loss bug in GroupCommitCoordinator"]
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
    coord
        .mark_flushed(Err(Error::Storage(StorageError::WalError {
            reason: "Disk Full (Epoch 1)".to_string(),
        })))
        .unwrap();

    // 3. Register T2 (Epoch 2)
    // We register to ensure the coordinator moves to Epoch 2 state fully
    let (epoch2, _) = coord.register_transaction().unwrap();
    assert_eq!(epoch2, 2);

    // 4. Mark Epoch 2 as SUCCESS
    coord.mark_flushed(Ok(())).unwrap();

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
#[ignore = "Reproduces known 'False Failure' ghost error bug in GroupCommitCoordinator"]
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

    let coord = Arc::new(GroupCommitCoordinator::new(100, 100));

    // 1. Register T1 (Epoch 1)
    let (epoch1, _) = coord.register_transaction().unwrap();
    assert_eq!(epoch1, 1);

    // 2. Mark Epoch 1 as SUCCESS
    coord.mark_flushed(Ok(())).unwrap();

    // 3. Register T2 (Epoch 2)
    let (epoch2, _) = coord.register_transaction().unwrap();
    assert_eq!(epoch2, 2);

    // 4. Mark Epoch 2 as FAILED
    coord
        .mark_flushed(Err(Error::Storage(StorageError::WalError {
            reason: "Disk Full (Epoch 2)".to_string(),
        })))
        .unwrap();

    // 5. T1 checks status
    let result = coord.wait_for_flush(epoch1);

    // THIS ASSERTION SHOULD FAIL if the bug exists.
    assert!(
        result.is_ok(),
        "👺 Havoc: False Failure Detected! Successful transaction returned Error: {:?}",
        result.err()
    );
}
