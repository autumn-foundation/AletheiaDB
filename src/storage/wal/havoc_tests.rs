#[cfg(test)]
mod tests {
    use crate::storage::wal::group_commit::{GroupCommitCoordinator, GroupCommitConfig};

    #[test]
    fn test_havoc_group_commit_timeout_overflow() {
        // 👺 Havoc: User can pass huge timeouts which will panic std::time::Instant addition!
        let config = GroupCommitConfig {
            max_delay_ms: 10,
            max_batch_size: 100,
            timeout_multiplier: 1,
            timeout_base_ms: 0,
            timeout_min_ms: 0,
            timeout_max_ms: u64::MAX, // Boom
            recent_errors_capacity: 10,
        };

        let coord = GroupCommitCoordinator::with_config(config);

        let (epoch, _) = coord.register_transaction().unwrap();

        // This will panic when adding to Instant::now()
        let _ = coord.wait_for_flush(epoch);
    }
}
