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
    let tx_id = coordinator.begin_distributed_transaction(participants).unwrap();

    // 4. Move to Commit phase (this logs the decision)
    coordinator.prepare_distributed_transaction(tx_id).expect("Prepare should succeed");

    // Mark shards unavailable to cause commit failure (but after logging!)
    coordinator.mark_shard_unavailable(ShardId::new(0).unwrap());
    coordinator.mark_shard_unavailable(ShardId::new(1).unwrap());

    // This should fail to commit (network error), but the decision SHOULD be logged.
    let result = coordinator.commit_distributed_transaction(tx_id);
    assert!(result.is_err(), "Commit should fail due to network error");

    // Verify transaction is in Failed state in active map
    let tx = coordinator.get_transaction(tx_id).unwrap();
    assert!(tx.commit_decision_logged, "Commit decision should be logged");

    // 5. "Crash" and Restart
    drop(coordinator);

    // Create new coordinator with SAME config (pointing to same WAL)
    let coordinator_recovered = ShardCoordinator::new(config);

    // 6. Attempt Recovery
    let result = coordinator_recovered.recover_pending_transactions().unwrap();

    // 7. Verify Data Recovery (Fix Verification)
    // The transaction should be in the 'recovered' list (or 'dead_lettered' if retry fails, but recovery logic might retry commit).
    // Wait, recover_pending_transactions retries commit.
    // But the shards are still unavailable (new coordinator creates new connections, but they point to "localhost:9000" which is not a real server).
    // The mock ShardConnection is "healthy" by default.
    // So recovery should SUCCEED because the new coordinator thinks shards are healthy!
    // (Unless the mock connection actually tries network).
    // ShardConnection logic:
    // "Simulate a prepare call... In a real implementation, this would make an RPC call"
    // It returns Ok if healthy.

    // New coordinator -> New connections -> Default Healthy.
    // So recover_pending_transactions -> commit_distributed_transaction -> conn.commit() -> Ok.
    // So transaction should be in `recovered`.

    assert!(!result.recovered.is_empty(), "Transaction should be recovered from WAL");
    assert!(result.recovered.contains(&tx_id));
    assert!(result.dead_lettered.is_empty());

    // Cleanup
    // temp_dir drops automatically
}
