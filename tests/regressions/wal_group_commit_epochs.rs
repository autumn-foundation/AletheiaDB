use aletheiadb::core::error::Error;
use aletheiadb::core::error::StorageError;
use aletheiadb::storage::wal::group_commit::{GroupCommitConfig, GroupCommitCoordinator};
use std::sync::{Arc, Barrier};
use std::thread;

// Regression tests for WAL Group Commit issues.
// These tests verify fixes for Data Loss and Ghost Error scenarios.

#[test]
fn regression_group_commit_data_loss_prevention() {
    // SCENARIO: Data Loss Prevention
    // Verifies that if a flush fails, the waiting transaction receives an error.
    //
    // 1. T1 registers for Epoch 1.
    // 2. Flush 1 fails.
    // 3. T2 registers for Epoch 2.
    // 4. Flush 2 succeeds.
    // 5. T1 checks status.
    //
    // EXPECTED: T1 should receive an error (not Ok).

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

    assert!(
        result.is_err(),
        "Regression: Data Loss Detected! Transaction returned Ok, but Epoch 1 flush failed."
    );
}

#[test]
fn regression_group_commit_ghost_error_prevention() {
    // SCENARIO: Ghost Error Prevention
    // Verifies that a successful transaction does NOT report failure even if a LATER transaction fails.
    //
    // 1. T1 registers for Epoch 1.
    // 2. Flush 1 succeeds.
    // 3. T2 registers for Epoch 2.
    // 4. Flush 2 fails.
    // 5. T1 calls wait_for_flush(1).
    //
    // EXPECTED: T1 should receive Ok.

    let config = GroupCommitConfig {
        recent_errors_capacity: 100,
        ..GroupCommitConfig::default()
    };
    let coord = Arc::new(GroupCommitCoordinator::with_config(config));

    // 1. Register T1 (Epoch 1)
    let (epoch1, _) = coord.register_transaction().unwrap();
    assert_eq!(epoch1, 1);

    // 2. Mark Epoch 1 as SUCCESS
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

    // This ensures that we correctly distinguish between "failed and evicted" vs "succeeded and not in error list".
    // Since we maintain `oldest_error_epoch` and haven't evicted anything (history is short),
    // checking epoch 1 should correctly determine it succeeded.

    assert!(
        result.is_ok(),
        "Regression: False Failure (Ghost Error) Detected! Successful transaction returned Error: {:?}",
        result.err()
    );
}
