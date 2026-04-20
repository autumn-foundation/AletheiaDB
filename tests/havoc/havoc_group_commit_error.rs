use aletheiadb::core::error::Error;
use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
use std::time::{Duration, Instant};

#[test]
fn havoc_group_commit_cascading_failure() {
    let coord = GroupCommitCoordinator::new(10, 200);

    // Register epoch 1
    let (_epoch1, _) = coord.register_transaction().unwrap();
    let epoch1_flush = coord.start_flush().unwrap();

    // Register epoch 2
    let (epoch2, _) = coord.register_transaction().unwrap();

    // Finish flush for epoch 1 with ERROR
    coord
        .finish_flush(epoch1_flush, Err(Error::other("Fatal IO Error")))
        .unwrap();

    // Wait for epoch 2. Since epoch 1 failed, the WAL is broken.
    // Epoch 2 should fail immediately.
    let start = Instant::now();
    let _res = coord.wait_for_flush(epoch2);
    let elapsed = start.elapsed();

    // If it waits for more than 500ms, it means it hit the timeout!
    if elapsed > Duration::from_millis(100) {
        panic!(
            "👺 HAVOC: Cascading Failure Wait Detected!\n\
               \n\
               🧨 Trigger: Epoch 1 failed, but transactions waiting for Epoch 2 didn't get the memo.\n\
               📉 The Flaw: The `wait_for_flush` loop only checks if the EXACT epoch failed.\n\
               If an older epoch fails, the flush thread might die, leaving subsequent epochs to hang until timeout.\n\
               😈 Comment: You built a highly concurrent system that fails with a 30s timeout on disk errors. Nice latency spike."
        );
    }
}
