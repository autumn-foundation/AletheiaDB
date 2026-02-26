use aletheiadb::storage::sharding::config::{ShardConfig, ShardDefinition};
use aletheiadb::storage::sharding::coordinator::ShardCoordinator;
use aletheiadb::storage::sharding::types::ShardId;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn test_shard_recovery_data_loss_repro() {
    // 1. Setup configuration
    // We use a temp dir for WAL
    let temp_dir = TempDir::new().unwrap();
    let wal_path = temp_dir.path().join("coordinator.wal");

    let shard0 = ShardDefinition::new(0, "localhost:9000", vec!["A"]);
    let shard1 = ShardDefinition::new(1, "localhost:9001", vec!["B"]);

    // Config WITH WAL path
    let config = ShardConfig::new(vec![shard0, shard1])
        .with_request_timeout(Duration::from_secs(5))
        .with_wal_path(wal_path.clone());

    // 2. Create Coordinator
    let coordinator = ShardCoordinator::new(config.clone());

    // 3. Start a transaction
    let participants = vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()];
    let tx_id = coordinator
        .begin_distributed_transaction(participants)
        .unwrap();

    // 4. Move to Commit phase (this logs the decision)
    coordinator
        .prepare_distributed_transaction(tx_id)
        .expect("Prepare should succeed");

    // Mark shards unavailable to cause commit failure (but after logging!)
    coordinator.mark_shard_unavailable(ShardId::new(0).unwrap());
    coordinator.mark_shard_unavailable(ShardId::new(1).unwrap());

    // This should fail to commit (network error), but the decision SHOULD be logged.
    let result = coordinator.commit_distributed_transaction(tx_id);
    assert!(result.is_err(), "Commit should fail due to network error");

    // Verify transaction is in Failed state in active map
    let tx = coordinator.get_transaction(tx_id).unwrap();
    assert!(
        tx.commit_decision_logged,
        "Commit decision should be logged"
    );

    // 5. "Crash" and Restart
    drop(coordinator);

    // Create new coordinator with SAME config (pointing to same WAL)
    let coordinator_recovered = ShardCoordinator::new(config);

    // 6. Recovery happens automatically in new()
    // The shards are "healthy" by default in the new coordinator (mock connection).
    // So recovery should SUCCEED immediately.

    // 7. Verify Data Recovery
    // Since recovery succeeded, the transaction should be completed and removed from active tracking.
    let tx = coordinator_recovered.get_transaction(tx_id);
    assert!(
        tx.is_none(),
        "Transaction should be recovered and completed during startup"
    );

    // Cleanup
    // temp_dir drops automatically
}
