use super::*;

use super::*;
    use crate::storage::sharding::config::ShardDefinition;

    fn test_config() -> ShardConfig {
        ShardConfig::new(vec![
            ShardDefinition::new(0, "shard0:9000", vec!["Person"]),
            ShardDefinition::new(1, "shard1:9000", vec!["Place"]),
        ])
    }

    fn run_distributed_tx(
        coordinator: &ShardCoordinator,
        shards: &[ShardId],
    ) -> Result<HybridTimestamp, DistributedTxError> {
        let tx_id = coordinator.begin_distributed_transaction(shards.to_vec())?;
        coordinator.prepare_distributed_transaction(tx_id)?;
        let commit_timestamp = coordinator
            .get_transaction(tx_id)
            .and_then(|tx| tx.commit_timestamp)
            .ok_or_else(|| DistributedTxError::Aborted {
                reason: "Missing commit timestamp after prepare".to_string(),
            })?;
        coordinator.commit_distributed_transaction(tx_id)?;
        Ok(commit_timestamp)
    }

    #[test]
    fn test_coordinator_creation() {
        let config = test_config();
        let coordinator = ShardCoordinator::new(config);

        assert_eq!(coordinator.router().config().num_shards(), 2);
    }

    #[test]
    fn test_coordinator_routing() {
        let coordinator = ShardCoordinator::new(test_config());

        assert_eq!(coordinator.route_node("Person").as_u16(), 0);
        assert_eq!(coordinator.route_node("Place").as_u16(), 1);
    }

    #[test]
    fn test_coordinator_shard_state() {
        let coordinator = ShardCoordinator::new(test_config());
        let shard_id = ShardId::new(0).unwrap();

        let state = coordinator.get_shard_state(shard_id);
        assert!(state.is_some());
        assert_eq!(state.unwrap().status, ShardStatus::Healthy);
    }

    #[test]
    fn test_coordinator_mark_unavailable() {
        let coordinator = ShardCoordinator::new(test_config());
        let shard_id = ShardId::new(0).unwrap();

        coordinator.mark_shard_unavailable(shard_id);

        let state = coordinator.get_shard_state(shard_id);
        assert_eq!(state.unwrap().status, ShardStatus::Unavailable);
    }

    #[test]
    fn test_coordinator_mark_available() {
        let coordinator = ShardCoordinator::new(test_config());
        let shard_id = ShardId::new(0).unwrap();

        coordinator.mark_shard_unavailable(shard_id);
        coordinator.mark_shard_available(shard_id);

        let state = coordinator.get_shard_state(shard_id);
        assert_eq!(state.unwrap().status, ShardStatus::Healthy);
    }

    #[test]
    fn test_coordinator_begin_distributed_transaction() {
        let coordinator = ShardCoordinator::new(test_config());
        let shards = vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()];

        let tx_id = coordinator.begin_distributed_transaction(shards).unwrap();
        assert_eq!(coordinator.active_transaction_count(), 1);

        let tx = coordinator.get_transaction(tx_id);
        assert!(tx.is_some());
        assert_eq!(tx.unwrap().participants.len(), 2);
    }

    #[test]
    fn test_coordinator_prepare_commit_flow() {
        let coordinator = ShardCoordinator::new(test_config());
        let shards = vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()];

        // Begin transaction
        let tx_id = coordinator.begin_distributed_transaction(shards).unwrap();

        // Prepare
        let result = coordinator.prepare_distributed_transaction(tx_id);
        assert!(result.is_ok());

        // Commit
        let result = coordinator.commit_distributed_transaction(tx_id);
        assert!(result.is_ok());
    }

    #[test]
    fn test_coordinator_prepare_sets_commit_timestamp() {
        let coordinator = ShardCoordinator::new(test_config());
        let shards = vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()];

        let tx_id = coordinator.begin_distributed_transaction(shards).unwrap();
        assert!(coordinator.prepare_distributed_transaction(tx_id).is_ok());

        let tx = coordinator.get_transaction(tx_id).unwrap();
        assert!(tx.commit_timestamp.is_some());

        let result = coordinator.commit_distributed_transaction(tx_id);
        assert!(result.is_ok());
    }

    #[test]
    fn test_coordinator_prepare_with_unavailable_shard() {
        let coordinator = ShardCoordinator::new(test_config());
        let shard0 = ShardId::new(0).unwrap();
        let shard1 = ShardId::new(1).unwrap();

        // Mark one shard as unavailable
        coordinator.mark_shard_unavailable(shard1);

        // Begin transaction
        let tx_id = coordinator
            .begin_distributed_transaction(vec![shard0, shard1])
            .unwrap();

        // Prepare should fail
        let result = coordinator.prepare_distributed_transaction(tx_id);
        assert!(result.is_err());
    }

    #[test]
    fn test_coordinator_abort_transaction() {
        let coordinator = ShardCoordinator::new(test_config());
        let shards = vec![ShardId::new(0).unwrap()];

        let tx_id = coordinator.begin_distributed_transaction(shards).unwrap();
        let result = coordinator.abort_distributed_transaction(tx_id, "test abort");
        assert!(result.is_ok());
    }

    #[test]
    fn test_coordinator_calculate_imbalance() {
        let coordinator = ShardCoordinator::new(test_config());

        // Initially all shards are empty, so no imbalance
        assert_eq!(coordinator.calculate_imbalance(), 0.0);

        // Update one shard to have more nodes
        let mut state = coordinator
            .get_shard_state(ShardId::new(0).unwrap())
            .unwrap();
        state.node_count = 1000;
        coordinator.update_shard_state(ShardId::new(0).unwrap(), state);

        // Now there should be imbalance
        let imbalance = coordinator.calculate_imbalance();
        assert!(imbalance > 0.0);
    }

    #[test]
    fn test_coordinator_needs_rebalancing() {
        let coordinator = ShardCoordinator::new(test_config());

        // Initially no rebalancing needed
        assert!(!coordinator.needs_rebalancing());

        // Create significant imbalance
        let mut state0 = coordinator
            .get_shard_state(ShardId::new(0).unwrap())
            .unwrap();
        state0.node_count = 1000;
        coordinator.update_shard_state(ShardId::new(0).unwrap(), state0);

        let mut state1 = coordinator
            .get_shard_state(ShardId::new(1).unwrap())
            .unwrap();
        state1.node_count = 100;
        coordinator.update_shard_state(ShardId::new(1).unwrap(), state1);

        // Now rebalancing should be needed (>30% imbalance)
        assert!(coordinator.needs_rebalancing());
    }

    #[test]
    fn test_shard_connection() {
        let shard_id = ShardId::new(0).unwrap();
        let mut conn = ShardConnection::new(shard_id, "localhost:9000".to_string());

        assert!(conn.healthy);
        assert!(conn.prepare(TxId::new(1), None).is_ok());
        assert!(conn.commit(TxId::new(1), None).is_ok());
        assert!(conn.abort(TxId::new(1)).is_ok());

        conn.mark_unhealthy();
        assert!(!conn.healthy);
        assert!(conn.prepare(TxId::new(2), None).is_err());

        conn.mark_healthy();
        assert!(conn.healthy);
    }

    #[test]
    fn test_coordinator_debug() {
        let coordinator = ShardCoordinator::new(test_config());
        let debug = format!("{:?}", coordinator);
        assert!(debug.contains("ShardCoordinator"));
        assert!(debug.contains("num_shards"));
    }

    #[test]
    fn test_coordinator_get_all_shard_states() {
        let coordinator = ShardCoordinator::new(test_config());
        let states = coordinator.get_all_shard_states();
        assert_eq!(states.len(), 2);
    }

    #[test]
    fn test_coordinator_get_metrics() {
        let coordinator = ShardCoordinator::new(test_config());
        let shard_id = ShardId::new(0).unwrap();

        let metrics = coordinator.get_metrics(shard_id);
        assert!(metrics.is_some());
    }

    // ==================== RecoveryResult Tests ====================

    #[test]
    fn test_recovery_result_is_complete() {
        let result = RecoveryResult {
            recovered: vec![TxId::new(1), TxId::new(2)],
            dead_lettered: vec![],
        };
        assert!(result.is_complete());
        assert_eq!(result.dead_letter_count(), 0);

        let result_with_dead = RecoveryResult {
            recovered: vec![TxId::new(1)],
            dead_lettered: vec![DeadLetteredTransaction {
                tx_id: TxId::new(2),
                reason: "Test failure".to_string(),
                last_attempt: Instant::now(),
                attempt_count: 3,
            }],
        };
        assert!(!result_with_dead.is_complete());
        assert_eq!(result_with_dead.dead_letter_count(), 1);
    }

    // ==================== ShardConnection Tests ====================

    #[test]
    fn test_shard_connection_health_check() {
        let shard_id = ShardId::new(0).unwrap();
        let mut conn = ShardConnection::new(shard_id, "localhost:9000".to_string());

        assert!(conn.last_ping.is_none());

        let result = conn.health_check();
        assert!(result);
        assert!(conn.last_ping.is_some());
    }

    #[test]
    fn test_shard_connection_unhealthy_operations() {
        let shard_id = ShardId::new(0).unwrap();
        let mut conn = ShardConnection::new(shard_id, "localhost:9000".to_string());

        conn.mark_unhealthy();

        // All operations should fail when unhealthy
        assert!(conn.prepare(TxId::new(1), None).is_err());
        assert!(conn.commit(TxId::new(1), None).is_err());
        assert!(conn.abort(TxId::new(1)).is_err());
    }

    // ==================== Extended Coordinator Tests ====================

    #[test]
    fn test_coordinator_with_rebalance_config() {
        let config = test_config();
        let rebalance_config = RebalanceConfig {
            imbalance_threshold: 0.5,
            batch_size: 500,
            max_concurrent_migrations: 2,
            ..Default::default()
        };

        let coordinator = ShardCoordinator::new(config).with_rebalance_config(rebalance_config);
        assert_eq!(coordinator.router().config().num_shards(), 2);
    }

    #[test]
    fn test_coordinator_route_traversal() {
        let coordinator = ShardCoordinator::new(test_config());

        let plan = coordinator.route_traversal("Person", &["Place"]);
        assert!(!plan.involved_shards.is_empty());
    }

    #[test]
    fn test_coordinator_active_transaction_count() {
        let coordinator = ShardCoordinator::new(test_config());

        assert_eq!(coordinator.active_transaction_count(), 0);

        let shards = vec![ShardId::new(0).unwrap()];
        coordinator.begin_distributed_transaction(shards).unwrap();

        assert_eq!(coordinator.active_transaction_count(), 1);
    }

    #[test]
    fn test_coordinator_get_nonexistent_transaction() {
        let coordinator = ShardCoordinator::new(test_config());

        let tx = coordinator.get_transaction(TxId::new(99999));
        assert!(tx.is_none());
    }

    #[test]
    fn test_coordinator_prepare_nonexistent_transaction() {
        let coordinator = ShardCoordinator::new(test_config());

        let result = coordinator.prepare_distributed_transaction(TxId::new(99999));
        assert!(result.is_err());
    }

    #[test]
    fn test_coordinator_commit_nonexistent_transaction() {
        let coordinator = ShardCoordinator::new(test_config());

        let result = coordinator.commit_distributed_transaction(TxId::new(99999));
        assert!(result.is_err());
    }

    #[test]
    fn test_coordinator_abort_nonexistent_transaction() {
        let coordinator = ShardCoordinator::new(test_config());

        let result = coordinator.abort_distributed_transaction(TxId::new(99999), "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_coordinator_get_shard_state_nonexistent() {
        let coordinator = ShardCoordinator::new(test_config());

        let state = coordinator.get_shard_state(ShardId::new(99).unwrap());
        assert!(state.is_none());
    }

    #[test]
    fn test_coordinator_get_metrics_nonexistent() {
        let coordinator = ShardCoordinator::new(test_config());

        let metrics = coordinator.get_metrics(ShardId::new(99).unwrap());
        assert!(metrics.is_none());
    }

    #[test]
    fn test_coordinator_dead_letter_queue() {
        let coordinator = ShardCoordinator::new(test_config());

        // Initially empty
        let dead = coordinator.get_dead_lettered_transactions();
        assert!(dead.is_empty());
    }

    #[test]
    fn test_coordinator_retry_existing_dead_letter() {
        let coordinator = ShardCoordinator::new(test_config());
        let tx_id = TxId::new(42);

        // Add to commit log
        {
            let log = coordinator.commit_log.read().unwrap();
            let _ = log.log_commit(tx_id, vec![ShardId::new(1).unwrap()], None);
        }

        // Add to dead letter queue
        {
            let mut dlq = coordinator.dead_letter_queue.write().unwrap();
            dlq.insert(
                tx_id,
                DeadLetteredTransaction {
                    tx_id,
                    reason: "Test".to_string(),
                    last_attempt: std::time::Instant::now(),
                    attempt_count: 1,
                },
            );
        }

        let result = coordinator.retry_dead_lettered_transaction(tx_id);
        // It succeeds in preparing and marking as committing in this mocked case
        assert!(result.is_ok());
    }

    #[test]
    fn test_coordinator_retry_nonexistent_dead_letter() {
        let coordinator = ShardCoordinator::new(test_config());

        let result = coordinator.retry_dead_lettered_transaction(TxId::new(99999));
        assert!(result.is_err());
    }

    #[test]
    fn test_dead_lettered_transaction_debug() {
        let tx = DeadLetteredTransaction {
            tx_id: TxId::new(1),
            reason: "Test failure".to_string(),
            last_attempt: Instant::now(),
            attempt_count: 3,
        };

        let debug = format!("{:?}", tx);
        assert!(debug.contains("tx_id"));
        assert!(debug.contains("reason"));
        assert!(debug.contains("attempt_count"));
    }

    #[test]
    fn test_recovery_result_debug() {
        let result = RecoveryResult {
            recovered: vec![TxId::new(1)],
            dead_lettered: vec![],
        };

        let debug = format!("{:?}", result);
        assert!(debug.contains("recovered"));
        assert!(debug.contains("dead_lettered"));
    }

    #[test]
    fn test_next_commit_timestamp_allows_idle_forward_drift() {
        let coordinator = ShardCoordinator::new(test_config());
        let idle_gap_us = MAX_FORWARD_JUMP_US + 2_000_000;
        let old_wallclock = time::now().wallclock() - idle_gap_us;

        {
            let mut frontier = coordinator
                .commit_clock
                .lock()
                .expect("commit_clock lock should be available");
            *frontier = crate::core::hlc::HybridTimestamp::new(old_wallclock, 0).unwrap();
        }

        let now = Instant::now();
        {
            let mut observed_at = coordinator
                .commit_clock_observed_at
                .lock()
                .expect("commit_clock_observed_at lock should be available");
            *observed_at = now;
        }

        let result = coordinator
            .next_commit_timestamp_internal(now + Duration::from_micros(idle_gap_us as u64));
        assert!(
            result.is_ok(),
            "normal idle time should not be treated as forward clock skew"
        );
    }

    #[test]
    fn test_prepare_reinserts_transaction_on_timestamp_failure() {
        let coordinator = ShardCoordinator::new(test_config());

        {
            let mut frontier = coordinator
                .commit_clock
                .lock()
                .expect("commit_clock lock should be available");
            *frontier = crate::core::hlc::HybridTimestamp::new(
                crate::core::temporal::MAX_VALID_TIMESTAMP,
                u32::MAX,
            )
            .unwrap();
        }

        let tx_id = coordinator
            .begin_distributed_transaction(vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()])
            .unwrap();

        let result = coordinator.prepare_distributed_transaction(tx_id);
        assert!(result.is_err());

        let transaction = coordinator
            .get_transaction(tx_id)
            .expect("transaction should be reinserted after prepare timestamp failure");
        assert_eq!(transaction.phase, TransactionPhase::Pending);
        assert!(transaction.commit_timestamp.is_none());
    }

    #[test]
    fn test_commit_reinserts_transaction_on_timestamp_failure() {
        let coordinator = ShardCoordinator::new(test_config());

        {
            let mut frontier = coordinator
                .commit_clock
                .lock()
                .expect("commit_clock lock should be available");
            *frontier = crate::core::hlc::HybridTimestamp::new(
                crate::core::temporal::MAX_VALID_TIMESTAMP,
                u32::MAX,
            )
            .unwrap();
        }

        let tx_id = coordinator
            .begin_distributed_transaction(vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()])
            .unwrap();

        let result = coordinator.commit_distributed_transaction(tx_id);
        assert!(result.is_err());

        let transaction = coordinator
            .get_transaction(tx_id)
            .expect("transaction should be reinserted after commit timestamp failure");
        assert_eq!(transaction.phase, TransactionPhase::Pending);
        assert!(transaction.commit_timestamp.is_none());
    }

    #[test]
    fn test_next_commit_timestamp_backward_skew() {
        let coordinator = ShardCoordinator::new(test_config());
        let now = time::now().wallclock();
        let self_heal = is_clock_skew_self_heal_enabled();
        let skewed_frontier = now + (MAX_BACKWARD_DRIFT_US * 2);

        {
            let mut frontier = coordinator
                .commit_clock
                .lock()
                .expect("commit_clock lock should be available");
            *frontier = crate::core::hlc::HybridTimestamp::new(skewed_frontier, 0).unwrap();
        }

        let result = coordinator.next_commit_timestamp();

        if self_heal {
            assert!(result.is_ok());
            let committed = result.unwrap();
            assert_eq!(committed.wallclock(), skewed_frontier);
            assert_eq!(committed.logical(), 1);
        } else {
            let error =
                result.expect_err("expected backward skew to abort when self-heal is disabled");
            let reason = match error {
                DistributedTxError::Aborted { reason } => reason,
                _ => panic!("unexpected error variant: {error:?}"),
            };
            assert!(reason.contains("backward"));
        }
    }

    #[test]
    fn test_next_commit_timestamp_forward_skew() {
        let coordinator = ShardCoordinator::new(test_config());
        let now = time::now().wallclock();
        let self_heal = is_clock_skew_self_heal_enabled();
        let skewed_frontier = now - (MAX_FORWARD_JUMP_US * 2);

        {
            let mut frontier = coordinator
                .commit_clock
                .lock()
                .expect("commit_clock lock should be available");
            *frontier = crate::core::hlc::HybridTimestamp::new(skewed_frontier, 0).unwrap();
        }

        let result = coordinator.next_commit_timestamp();

        if self_heal {
            assert!(result.is_ok());
            let committed = result.unwrap();
            assert_eq!(committed.wallclock(), skewed_frontier);
            assert_eq!(committed.logical(), 1);
        } else {
            let error =
                result.expect_err("expected forward skew to abort when self-heal is disabled");
            let reason = match error {
                DistributedTxError::Aborted { reason } => reason,
                _ => panic!("unexpected error variant: {error:?}"),
            };
            assert!(reason.contains("forward"));
        }
    }

    #[test]
    fn test_prepare_with_backward_skew_distributed_tx() {
        let coordinator = ShardCoordinator::new(test_config());
        let now = time::now().wallclock();
        let skewed_frontier = now + (MAX_BACKWARD_DRIFT_US * 2);

        {
            let mut frontier = coordinator
                .commit_clock
                .lock()
                .expect("commit_clock lock should be available");
            *frontier = crate::core::hlc::HybridTimestamp::new(skewed_frontier, 0).unwrap();
        }

        let tx_id = coordinator
            .begin_distributed_transaction(vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()])
            .unwrap();

        let self_heal = is_clock_skew_self_heal_enabled();
        let result = coordinator.prepare_distributed_transaction(tx_id);

        if self_heal {
            assert!(result.is_ok());
            let tx = coordinator.get_transaction(tx_id).unwrap();
            assert!(tx.commit_timestamp.is_some());
            assert!(coordinator.commit_distributed_transaction(tx_id).is_ok());
        } else {
            assert!(result.is_err());
            let error =
                result.expect_err("expected backward skew to abort when self-heal is disabled");
            let reason = match error {
                DistributedTxError::Aborted { reason } => reason,
                _ => panic!("unexpected error variant: {error:?}"),
            };
            assert!(reason.contains("backward"));
        }
    }

    #[test]
    fn test_prepare_with_forward_skew_distributed_tx() {
        let coordinator = ShardCoordinator::new(test_config());
        let now = time::now().wallclock();
        let skewed_frontier = now - (MAX_FORWARD_JUMP_US * 2);

        {
            let mut frontier = coordinator
                .commit_clock
                .lock()
                .expect("commit_clock lock should be available");
            *frontier = crate::core::hlc::HybridTimestamp::new(skewed_frontier, 0).unwrap();
        }

        let tx_id = coordinator
            .begin_distributed_transaction(vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()])
            .unwrap();

        let self_heal = is_clock_skew_self_heal_enabled();
        let result = coordinator.prepare_distributed_transaction(tx_id);

        if self_heal {
            assert!(result.is_ok());
            let tx = coordinator.get_transaction(tx_id).unwrap();
            assert!(tx.commit_timestamp.is_some());
            assert!(coordinator.commit_distributed_transaction(tx_id).is_ok());
        } else {
            assert!(result.is_err());
            let error =
                result.expect_err("expected forward skew to abort when self-heal is disabled");
            let reason = match error {
                DistributedTxError::Aborted { reason } => reason,
                _ => panic!("unexpected error variant: {error:?}"),
            };
            assert!(reason.contains("forward"));
        }
    }

    #[test]
    fn test_repeated_backward_skew_prepare_commit_flow() {
        let coordinator = ShardCoordinator::new(test_config());
        let shards = vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()];
        let self_heal = is_clock_skew_self_heal_enabled();

        // First prepare/commit starts with a heavily backward-skewed frontier.
        let first_frontier = time::now().wallclock() + (MAX_BACKWARD_DRIFT_US * 2);
        {
            let mut frontier = coordinator
                .commit_clock
                .lock()
                .expect("commit_clock lock should be available");
            *frontier = crate::core::hlc::HybridTimestamp::new(first_frontier, 0).unwrap();
        }

        let first = run_distributed_tx(&coordinator, &shards);

        if !self_heal {
            assert!(first.is_err());
            let error = first.expect_err("expected backward skew abort when self-heal is disabled");
            let reason = match error {
                DistributedTxError::Aborted { reason } => reason,
                _ => panic!("unexpected error variant: {error:?}"),
            };
            assert!(reason.contains("backward"));
            return;
        }

        let first = first.unwrap();

        // Second prepare/commit hits backward skew again with a slightly newer frontier.
        let second_frontier = first.wallclock() + 1;
        {
            let mut frontier = coordinator
                .commit_clock
                .lock()
                .expect("commit_clock lock should be available");
            *frontier = crate::core::hlc::HybridTimestamp::new(second_frontier, 0).unwrap();
        }

        let second = run_distributed_tx(&coordinator, &shards).unwrap();
        assert!(second > first);
    }

    #[test]
    fn test_repeated_forward_skew_prepare_commit_flow() {
        let coordinator = ShardCoordinator::new(test_config());
        let shards = vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()];
        let self_heal = is_clock_skew_self_heal_enabled();

        // First prepare/commit with an aggressively forward-skewed frontier.
        let first_frontier = time::now().wallclock() - (MAX_FORWARD_JUMP_US * 2);
        {
            let mut frontier = coordinator
                .commit_clock
                .lock()
                .expect("commit_clock lock should be available");
            *frontier = crate::core::hlc::HybridTimestamp::new(first_frontier, 0).unwrap();
        }

        let first = run_distributed_tx(&coordinator, &shards);
        if !self_heal {
            assert!(first.is_err());
            let error =
                first.expect_err("expected forward skew to abort when self-heal is disabled");
            let reason = match error {
                DistributedTxError::Aborted { reason } => reason,
                _ => panic!("unexpected error variant: {error:?}"),
            };
            assert!(reason.contains("forward"));
            return;
        }

        let first = first.unwrap();

        // Second prepare/commit again under forward-skew pressure.
        let second_frontier = first.wallclock() + (MAX_FORWARD_JUMP_US * 2);
        {
            let mut frontier = coordinator
                .commit_clock
                .lock()
                .expect("commit_clock lock should be available");
            *frontier = crate::core::hlc::HybridTimestamp::new(second_frontier, 0).unwrap();
        }

        let second = run_distributed_tx(&coordinator, &shards).unwrap();
        assert!(second > first);
    }

    #[test]
    fn test_havoc_deadlock() {
        use std::sync::{Arc, RwLock};
        use std::thread;
        use std::time::Duration;

        let active_transactions = Arc::new(RwLock::new(()));
        let connections = Arc::new(RwLock::new(()));

        let tx1 = active_transactions.clone();
        let conn1 = connections.clone();
        let t1 = thread::spawn(move || {
            let _c = conn1.read().unwrap();
            thread::sleep(Duration::from_millis(50));
            // This simulates the missing drop(connections) before active_transactions.write()
            let _t = tx1.write().unwrap();
        });

        let tx2 = active_transactions.clone();
        let conn2 = connections.clone();
        let t2 = thread::spawn(move || {
            let _t = tx2.write().unwrap();
            thread::sleep(Duration::from_millis(50));
            // This simulates an operation that holds active_transactions and requests connections
            let _c = conn2.read().unwrap();
        });

        // Use a timeout to detect deadlock
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            t1.join().unwrap();
            t2.join().unwrap();
            tx.send(()).unwrap();
        });

        assert!(
            rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "Deadlock detected!"
        );
    }
