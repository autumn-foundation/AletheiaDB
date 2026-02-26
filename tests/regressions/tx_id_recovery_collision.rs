use aletheiadb::core::id::TxId;
use aletheiadb::storage::sharding::config::{ShardConfig, ShardDefinition};
use aletheiadb::storage::sharding::coordinator::ShardCoordinator;
use aletheiadb::storage::sharding::types::ShardId;
use tempfile::TempDir;

#[test]
fn test_coordinator_tx_id_collision_on_restart() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().to_path_buf();
    let wal_path = db_path.join("commit.log");

    let shard_config = ShardConfig::new(vec![ShardDefinition::new(
        0,
        "shard0:9000",
        vec!["Person"],
    )])
    .with_wal_path(wal_path.clone());

    // 1. Start Coordinator and generate some transactions
    let mut last_tx_id_run1 = TxId::new(0);
    {
        let coordinator = ShardCoordinator::new(shard_config.clone());

        // Generate 10 transactions
        for _ in 0..10 {
            let tx_id = coordinator
                .begin_distributed_transaction(vec![ShardId::new(0).unwrap()])
                .unwrap();
            last_tx_id_run1 = tx_id;

            // Log them as committed so they persist in WAL
            // Note: In real life, we'd go through prepare/commit, but we can cheat by accessing log if needed,
            // but going through public API is better.
            coordinator
                .prepare_distributed_transaction(tx_id)
                .unwrap();
            coordinator
                .commit_distributed_transaction(tx_id)
                .unwrap();
        }
    }

    println!("Last TxId from Run 1: {}", last_tx_id_run1);

    // 2. Restart Coordinator
    {
        let coordinator = ShardCoordinator::new(shard_config.clone());

        // 3. Generate a NEW transaction
        let new_tx_id = coordinator
            .begin_distributed_transaction(vec![ShardId::new(0).unwrap()])
            .unwrap();
        println!("First TxId from Run 2: {}", new_tx_id);

        // 4. Verify Monotonicity
        // The new TxId MUST be greater than the last one from Run 1.
        // If it reset to 0 (or 1), this assertion will fail.
        assert!(
            new_tx_id > last_tx_id_run1,
            "TxId regression detected! Run 1 ended at {}, Run 2 started at {}",
            last_tx_id_run1,
            new_tx_id
        );
    }
}
