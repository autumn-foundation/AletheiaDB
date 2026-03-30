#![cfg(loom)]

use std::time::Duration;
use loom::sync::Arc;
use loom::thread;
use aletheiadb::storage::wal::group_commit::{GroupCommitCoordinator, GroupCommitConfig};

#[test]
fn test_group_commit_coordinator_deadlock() {
    loom::model(|| {
        let config = GroupCommitConfig {
            max_delay_ms: 5,
            max_batch_size: 2,
            timeout_multiplier: 1,
            timeout_base_ms: 1,
            timeout_min_ms: 1,
            timeout_max_ms: 5,
            recent_errors_capacity: 10,
        };

        let coord = Arc::new(GroupCommitCoordinator::with_config(config));

        let coord1 = Arc::clone(&coord);
        let t1 = thread::spawn(move || {
            let (epoch, should_flush) = coord1.register_transaction().unwrap();

            if should_flush {
                let flush_epoch = coord1.start_flush().unwrap();
                coord1.finish_flush(flush_epoch, Ok(())).unwrap();
            } else {
                 // Simulate timeout
                 let _ = coord1.wait_for_flush(epoch);
            }
        });

        let coord2 = Arc::clone(&coord);
        let t2 = thread::spawn(move || {
            let (epoch, should_flush) = coord2.register_transaction().unwrap();

            if should_flush {
                let flush_epoch = coord2.start_flush().unwrap();
                coord2.finish_flush(flush_epoch, Ok(())).unwrap();
            } else {
                let _ = coord2.wait_for_flush(epoch);
            }
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}
