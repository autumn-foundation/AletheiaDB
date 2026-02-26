use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::id::TxId;
use aletheiadb::storage::sharding::{
    CommitLogConfig, PersistentCommitLog, ShardConfig, ShardCoordinator, ShardDefinition, ShardId,
};
use tempfile::tempdir;

#[test]
fn test_shard_coordinator_durability_repro() {
    let dir = tempdir().unwrap();
    let wal_path = dir.path().join("commit.log");

    let shard0 = ShardId::new(0).unwrap();
    let shard1 = ShardId::new(1).unwrap();
    let shards = vec![shard0, shard1];
    let tx_id = TxId::new(100);

    // 1. Inject a pending commit into the log (Simulating a crash state)
    {
        let log = PersistentCommitLog::new(&wal_path, CommitLogConfig::default()).unwrap();
        let commit_ts = HybridTimestamp::new(100, 0).unwrap();
        log.log_commit(tx_id, shards.clone(), Some(commit_ts))
            .unwrap();
        assert!(log.has_pending_decision(tx_id));
    }

    // 2. Restart Coordinator
    {
        let mut config = ShardConfig::new(vec![
            ShardDefinition::new(0, "shard0:9000", vec!["Person"]),
            ShardDefinition::new(1, "shard1:9000", vec!["Place"]),
        ]);
        config.wal_path = Some(wal_path.clone());

        // This panics if recovery fails, ensuring we test the fix (which adds recovery call in new)
        let coordinator = ShardCoordinator::new(config);

        // 3. Verification
        // In this test environment, ShardConnection is a stub that returns `Ok(())` immediately.
        // Therefore, `recover_pending_transactions` (called in `new`) will successfully "commit"
        // the transaction to all (mock) shards.
        // Once all shards acknowledge, the coordinator logs completion and removes the transaction
        // from active memory.

        let tx = coordinator.get_transaction(tx_id);
        assert!(
            tx.is_none(),
            "Transaction should be completed and removed from active list after successful recovery"
        );
    }

    // 4. Verify log state
    // If recovery ran successfully, it MUST have logged the completion to the persistent log.
    // This confirms that the pending transaction was indeed processed.
    {
        let log = PersistentCommitLog::new(&wal_path, CommitLogConfig::default()).unwrap();
        assert!(
            !log.has_pending_decision(tx_id),
            "Transaction should have been completed and removed from pending log during recovery"
        );
    }
}
