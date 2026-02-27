use aletheiadb::core::id::TxId;
use aletheiadb::storage::sharding::config::{ShardConfig, ShardDefinition};
use aletheiadb::storage::sharding::coordinator::ShardCoordinator;
use tempfile::TempDir;

#[test]
fn test_repro_tx_id_reuse_on_recovery() {
    let dir = TempDir::new().unwrap();
    let wal_path = dir.path().join("coordinator.wal");

    let config = ShardConfig::new(vec![ShardDefinition::new(0, "shard0:9000", vec!["Person"])])
        .with_wal_path(wal_path.clone());

    // 1. Start coordinator and run a transaction
    {
        let coordinator = ShardCoordinator::new(config.clone());
        let tx_id = coordinator
            .begin_distributed_transaction(vec![
                aletheiadb::storage::sharding::types::ShardId::new(0).unwrap(),
            ])
            .unwrap();

        println!("First transaction ID: {}", tx_id);

        // Prepare and Commit to ensure it's logged
        coordinator.prepare_distributed_transaction(tx_id).unwrap();
        coordinator.commit_distributed_transaction(tx_id).unwrap();

        // IdGenerator normally starts at 0, but PersistentCommitLog forces a reset to at least 1
        // to avoid ambiguity with "empty log" state.
        assert_eq!(tx_id, TxId::new(1));
    }

    // 2. Restart coordinator
    {
        let coordinator = ShardCoordinator::new(config);

        // 3. Start a new transaction
        let new_tx_id = coordinator
            .begin_distributed_transaction(vec![
                aletheiadb::storage::sharding::types::ShardId::new(0).unwrap(),
            ])
            .unwrap();

        println!("Second transaction ID (after restart): {}", new_tx_id);

        // 4. Verify ID uniqueness
        // If ID generation reset, new_tx_id will be 0, which duplicates the first transaction
        assert!(
            new_tx_id > TxId::new(0),
            "Transaction ID reused after restart! Got {}, expected > 0",
            new_tx_id
        );
    }
}
