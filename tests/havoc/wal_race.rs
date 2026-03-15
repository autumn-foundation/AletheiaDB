#[cfg(test)]
mod tests {
    use aletheiadb::core::error::{Error, StorageError};
    use aletheiadb::storage::wal::group_commit::{GroupCommitConfig, GroupCommitCoordinator};
    use std::sync::Arc;

    #[test]
    fn test_havoc_group_commit_false_failure() {
        let config = GroupCommitConfig {
            max_delay_ms: 10,
            max_batch_size: 10,
            timeout_multiplier: 2,
            timeout_base_ms: 10,
            timeout_min_ms: 5,
            timeout_max_ms: 100,
            recent_errors_capacity: 2,
        };
        let coord = Arc::new(GroupCommitCoordinator::with_config(config));

        // 1. Thread A registers Epoch 1
        let (epoch1, _) = coord.register_transaction().unwrap();

        // 2. Epoch 1 is flushed successfully
        let flush_ep = coord.start_flush().unwrap();
        coord.finish_flush(flush_ep, Ok(())).unwrap();

        // 3. A bunch of OTHER epochs fail, pushing out the history
        for _ in 0..5 {
            coord.register_transaction().unwrap();
            let ep = coord.start_flush().unwrap();
            coord
                .finish_flush(
                    ep,
                    Err(Error::Storage(StorageError::WalError {
                        reason: "Fail".to_string(),
                    })),
                )
                .unwrap();
        }

        // 4. Thread A now calls wait_for_flush(epoch1).
        // It SHOULD succeed, because epoch 1 was successfully flushed!
        let result = coord.wait_for_flush(epoch1);

        // This fails if the false failure bug exists.
        assert!(
            result.is_ok(),
            "Epoch 1 succeeded, but wait_for_flush returned an error: {:?}",
            result.err()
        );
    }
}
