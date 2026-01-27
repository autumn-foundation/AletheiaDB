//! Tests for HLC Phase 3: Distributed Coordination and Network Synchronization.
//!
//! These tests verify HLC integration with:
//! - Distributed transaction coordination (2PC)
//! - Network message timestamp synchronization
//! - Cross-shard causality preservation

use gallifreydb::api::TxId;
use gallifreydb::core::hlc::HybridTimestamp;
use gallifreydb::storage::sharding::transaction::{DistributedTransaction, TwoPhaseCommitLog};
use gallifreydb::storage::sharding::types::ShardId;
use std::time::Duration;

/// RED PHASE TEST 1: Distributed transactions should use HybridTimestamp instead of Instant
#[test]
fn test_distributed_tx_uses_hybrid_timestamp() {
    let tx_id = TxId::new(1);
    let shard0 = ShardId::new(0).unwrap();
    let shard1 = ShardId::new(1).unwrap();
    let participants = vec![shard0, shard1];

    // Create transaction with a specific HLC timestamp
    let start_ts = HybridTimestamp::new(1_000_000, 0).unwrap();
    let tx = DistributedTransaction::new_with_timestamp(
        tx_id,
        participants,
        Duration::from_secs(30),
        start_ts,
    );

    // Verify transaction has HLC timestamp
    assert_eq!(tx.start_timestamp(), start_ts);
    assert_eq!(tx.start_timestamp().wallclock(), 1_000_000);
    assert_eq!(tx.start_timestamp().logical(), 0);
}

/// RED PHASE TEST 2: Transaction commit should update HLC using send()
#[test]
fn test_distributed_tx_commit_advances_hlc() {
    let tx_id = TxId::new(1);
    let shard0 = ShardId::new(0).unwrap();
    let start_ts = HybridTimestamp::new(1_000_000, 5).unwrap();

    let mut tx = DistributedTransaction::new_with_timestamp(
        tx_id,
        vec![shard0],
        Duration::from_secs(30),
        start_ts,
    );

    // Prepare and commit
    tx.begin_prepare().unwrap();
    tx.record_prepare_success(shard0);
    tx.mark_prepared().unwrap();
    tx.begin_commit().unwrap();

    // Get current physical time (simulate clock advancement)
    let physical_time = 1_001_000;

    // Record commit with HLC advancement
    tx.record_commit_with_timestamp(shard0, physical_time)
        .unwrap();

    // Verify HLC advanced using send() semantics
    let commit_ts = tx.commit_timestamp().unwrap();
    assert!(
        commit_ts > start_ts,
        "Commit timestamp should be greater than start"
    );
    assert_eq!(commit_ts.wallclock(), physical_time);
    assert_eq!(commit_ts.logical(), 0); // Should reset since wallclock advanced
}

/// RED PHASE TEST 3: Receiving prepare responses should use HLC receive()
#[test]
fn test_distributed_tx_prepare_response_synchronizes_hlc() {
    let tx_id = TxId::new(1);
    let shard0 = ShardId::new(0).unwrap();
    let shard1 = ShardId::new(1).unwrap();

    let local_ts = HybridTimestamp::new(1_000_000, 5).unwrap();
    let mut tx = DistributedTransaction::new_with_timestamp(
        tx_id,
        vec![shard0, shard1],
        Duration::from_secs(30),
        local_ts,
    );

    tx.begin_prepare().unwrap();

    // Shard0 responds with timestamp ahead of coordinator
    let shard0_ts = HybridTimestamp::new(1_002_000, 10).unwrap();
    let physical_time = 1_001_000;

    tx.record_prepare_success_with_timestamp(shard0, shard0_ts, physical_time)
        .unwrap();

    // Coordinator's HLC should advance using receive() semantics
    let current_ts = tx.current_timestamp();
    assert!(current_ts > local_ts, "Coordinator HLC should advance");
    assert!(
        current_ts > shard0_ts,
        "Coordinator HLC should be > shard response"
    );

    // Verify causality: current_ts = max(local, shard0, physical) with logical+1
    assert_eq!(current_ts.wallclock(), 1_002_000); // max wallclock
    assert_eq!(current_ts.logical(), 11); // shard0.logical + 1
}

/// RED PHASE TEST 4: 2PC log should record HLC timestamps
#[test]
fn test_two_phase_commit_log_uses_hlc() {
    let mut log = TwoPhaseCommitLog::new();
    let tx_id = TxId::new(1);
    let shards = vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()];

    // Log commit decision with HLC timestamp
    let commit_ts = HybridTimestamp::new(1_500_000, 42).unwrap();
    let lsn = log.log_commit_with_timestamp(tx_id, shards.clone(), commit_ts);

    assert_eq!(lsn, 0);
    assert!(log.has_pending_decision(tx_id));

    // Retrieve decision and verify HLC timestamp
    let decision = log.get_decision(tx_id).unwrap();
    assert_eq!(decision.tx_id, tx_id);
    assert!(decision.decision);
    assert_eq!(decision.timestamp, commit_ts);
    assert_eq!(decision.timestamp.wallclock(), 1_500_000);
    assert_eq!(decision.timestamp.logical(), 42);
}

/// RED PHASE TEST 5: Multiple transactions should maintain HLC ordering
#[test]
fn test_concurrent_transactions_maintain_hlc_ordering() {
    let tx1_id = TxId::new(1);
    let tx2_id = TxId::new(2);
    let tx3_id = TxId::new(3);
    let shard0 = ShardId::new(0).unwrap();

    // Three transactions at same wallclock but different logical counters
    let ts1 = HybridTimestamp::new(1_000_000, 0).unwrap();
    let ts2 = HybridTimestamp::new(1_000_000, 1).unwrap();
    let ts3 = HybridTimestamp::new(1_000_000, 2).unwrap();

    let tx1 = DistributedTransaction::new_with_timestamp(
        tx1_id,
        vec![shard0],
        Duration::from_secs(30),
        ts1,
    );
    let tx2 = DistributedTransaction::new_with_timestamp(
        tx2_id,
        vec![shard0],
        Duration::from_secs(30),
        ts2,
    );
    let tx3 = DistributedTransaction::new_with_timestamp(
        tx3_id,
        vec![shard0],
        Duration::from_secs(30),
        ts3,
    );

    // Verify total ordering despite same wallclock
    assert!(tx1.start_timestamp() < tx2.start_timestamp());
    assert!(tx2.start_timestamp() < tx3.start_timestamp());
    assert!(tx1.start_timestamp() < tx3.start_timestamp());
}

/// RED PHASE TEST 6: Clock skew handling in distributed prepare
#[test]
fn test_distributed_tx_handles_clock_skew() {
    let tx_id = TxId::new(1);
    let shard0 = ShardId::new(0).unwrap();
    let shard1 = ShardId::new(1).unwrap();

    // Coordinator has local time
    let coordinator_ts = HybridTimestamp::new(2_000_000, 5).unwrap();
    let mut tx = DistributedTransaction::new_with_timestamp(
        tx_id,
        vec![shard0, shard1],
        Duration::from_secs(30),
        coordinator_ts,
    );

    tx.begin_prepare().unwrap();

    // Shard0 has clock ahead (clock skew)
    let shard0_ts = HybridTimestamp::new(2_005_000, 0).unwrap();
    // Shard1 has clock behind (clock skew)
    let shard1_ts = HybridTimestamp::new(1_995_000, 8).unwrap();

    let physical_time = 2_001_000; // Coordinator's current physical clock

    tx.record_prepare_success_with_timestamp(shard0, shard0_ts, physical_time)
        .unwrap();
    tx.record_prepare_success_with_timestamp(shard1, shard1_ts, physical_time)
        .unwrap();

    // Final HLC should be max of all clocks
    let final_ts = tx.current_timestamp();

    // Should use shard0's wallclock (highest)
    assert_eq!(final_ts.wallclock(), 2_005_000);
    // Logical should be 2 because:
    // - First receive: (2_000_000,5) + shard0(2_005_000,0) -> (2_005_000,1)
    // - Second receive: (2_005_000,1) + shard1(1_995_000,8) -> (2_005_000,2)
    assert_eq!(final_ts.logical(), 2);

    // Verify causality preserved
    assert!(final_ts > coordinator_ts);
    assert!(final_ts > shard0_ts);
    assert!(final_ts > shard1_ts);
}

/// RED PHASE TEST 7: Recovery should restore HLC state
#[test]
fn test_recovery_restores_hlc_state() {
    let mut log = TwoPhaseCommitLog::new();

    // Log several commit decisions with HLC timestamps
    let tx1_ts = HybridTimestamp::new(1_000_000, 0).unwrap();
    let tx2_ts = HybridTimestamp::new(1_001_000, 5).unwrap();
    let tx3_ts = HybridTimestamp::new(1_002_000, 2).unwrap();

    log.log_commit_with_timestamp(TxId::new(1), vec![ShardId::new(0).unwrap()], tx1_ts);
    log.log_commit_with_timestamp(TxId::new(2), vec![ShardId::new(0).unwrap()], tx2_ts);
    log.log_commit_with_timestamp(TxId::new(3), vec![ShardId::new(0).unwrap()], tx3_ts);

    // During recovery, should be able to get all HLC timestamps
    let mut decisions = log.decisions_to_replay();
    assert_eq!(decisions.len(), 3);

    // Sort by timestamp to verify ordering is preserved
    decisions.sort_by_key(|d| d.timestamp);

    // Verify timestamps are preserved and ordered
    assert_eq!(decisions[0].timestamp, tx1_ts);
    assert_eq!(decisions[1].timestamp, tx2_ts);
    assert_eq!(decisions[2].timestamp, tx3_ts);

    // Verify strict ordering
    assert!(decisions[0].timestamp < decisions[1].timestamp);
    assert!(decisions[1].timestamp < decisions[2].timestamp);
}
