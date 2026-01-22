//! Shard coordinator for managing distributed operations.

use super::config::{RebalanceConfig, ShardConfig};
use super::router::{ShardRouter, TraversalPlan};
use super::transaction::{DistributedTransaction, DistributedTxError, TwoPhaseCommitLog};
use super::types::{ShardId, ShardMetrics, ShardState, ShardStatus};
use crate::api::TxId;
use crate::core::id::IdGenerator;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Connection to a shard (placeholder for actual network implementation).
#[derive(Debug)]
pub struct ShardConnection {
    /// The shard ID this connection is for.
    pub shard_id: ShardId,
    /// Endpoint address.
    pub endpoint: String,
    /// Whether the connection is healthy.
    pub healthy: bool,
    /// Last successful ping time.
    pub last_ping: Option<Instant>,
}

impl ShardConnection {
    /// Create a new shard connection.
    pub fn new(shard_id: ShardId, endpoint: String) -> Self {
        Self {
            shard_id,
            endpoint,
            healthy: true,
            last_ping: None,
        }
    }

    /// Simulate a prepare call to the shard.
    pub fn prepare(&self, _tx_id: TxId) -> Result<(), DistributedTxError> {
        if !self.healthy {
            return Err(DistributedTxError::ParticipantUnavailable {
                shard_id: self.shard_id,
            });
        }
        // In a real implementation, this would make an RPC call
        Ok(())
    }

    /// Simulate a commit call to the shard.
    pub fn commit(&self, _tx_id: TxId) -> Result<(), DistributedTxError> {
        if !self.healthy {
            return Err(DistributedTxError::ParticipantUnavailable {
                shard_id: self.shard_id,
            });
        }
        // In a real implementation, this would make an RPC call
        Ok(())
    }

    /// Simulate an abort call to the shard.
    pub fn abort(&self, _tx_id: TxId) -> Result<(), DistributedTxError> {
        if !self.healthy {
            return Err(DistributedTxError::ParticipantUnavailable {
                shard_id: self.shard_id,
            });
        }
        // In a real implementation, this would make an RPC call
        Ok(())
    }

    /// Perform a health check.
    pub fn health_check(&mut self) -> bool {
        // In a real implementation, this would ping the shard
        self.last_ping = Some(Instant::now());
        self.healthy
    }

    /// Mark the connection as unhealthy.
    pub fn mark_unhealthy(&mut self) {
        self.healthy = false;
    }

    /// Mark the connection as healthy.
    pub fn mark_healthy(&mut self) {
        self.healthy = true;
        self.last_ping = Some(Instant::now());
    }
}

/// Coordinator for managing shards and distributed operations.
pub struct ShardCoordinator {
    /// Router for query routing decisions.
    router: ShardRouter,
    /// Connections to each shard.
    connections: RwLock<HashMap<ShardId, ShardConnection>>,
    /// State of each shard.
    shard_states: RwLock<HashMap<ShardId, ShardState>>,
    /// Transaction ID generator.
    tx_id_generator: IdGenerator,
    /// Active distributed transactions.
    active_transactions: RwLock<HashMap<TxId, DistributedTransaction>>,
    /// Commit log for recovery.
    commit_log: RwLock<TwoPhaseCommitLog>,
    /// Metrics per shard.
    metrics: RwLock<HashMap<ShardId, Arc<ShardMetrics>>>,
    /// Rebalance configuration.
    rebalance_config: RebalanceConfig,
    /// Timeout for transaction operations.
    transaction_timeout: Duration,
}

impl ShardCoordinator {
    /// Create a new shard coordinator.
    pub fn new(config: ShardConfig) -> Self {
        let mut connections = HashMap::new();
        let mut shard_states = HashMap::new();
        let mut metrics = HashMap::new();

        for shard_def in &config.shards {
            connections.insert(
                shard_def.id,
                ShardConnection::new(shard_def.id, shard_def.endpoint.clone()),
            );
            shard_states.insert(shard_def.id, ShardState::new(shard_def.id));
            metrics.insert(shard_def.id, Arc::new(ShardMetrics::new()));
        }

        let transaction_timeout = config.request_timeout;
        let router = ShardRouter::new(config);

        Self {
            router,
            connections: RwLock::new(connections),
            shard_states: RwLock::new(shard_states),
            tx_id_generator: IdGenerator::new(),
            active_transactions: RwLock::new(HashMap::new()),
            commit_log: RwLock::new(TwoPhaseCommitLog::new()),
            metrics: RwLock::new(metrics),
            rebalance_config: RebalanceConfig::default(),
            transaction_timeout,
        }
    }

    /// Create a coordinator with custom rebalance config.
    pub fn with_rebalance_config(mut self, config: RebalanceConfig) -> Self {
        self.rebalance_config = config;
        self
    }

    /// Get the router.
    pub fn router(&self) -> &ShardRouter {
        &self.router
    }

    /// Route a node query.
    pub fn route_node(&self, label: &str) -> ShardId {
        self.router.route_node(label)
    }

    /// Route a traversal query.
    pub fn route_traversal(&self, start_label: &str, target_labels: &[&str]) -> TraversalPlan {
        self.router.route_traversal(start_label, target_labels)
    }

    /// Get the state of a shard.
    pub fn get_shard_state(&self, shard_id: ShardId) -> Option<ShardState> {
        self.shard_states.read().ok()?.get(&shard_id).cloned()
    }

    /// Get all shard states.
    pub fn get_all_shard_states(&self) -> Vec<ShardState> {
        self.shard_states
            .read()
            .map(|states| states.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Update shard state.
    pub fn update_shard_state(&self, shard_id: ShardId, state: ShardState) {
        if let Ok(mut states) = self.shard_states.write() {
            states.insert(shard_id, state);
        }
    }

    /// Get metrics for a shard.
    pub fn get_metrics(&self, shard_id: ShardId) -> Option<Arc<ShardMetrics>> {
        self.metrics.read().ok()?.get(&shard_id).cloned()
    }

    /// Start a new distributed transaction.
    pub fn begin_distributed_transaction(
        &self,
        participants: Vec<ShardId>,
    ) -> Result<TxId, DistributedTxError> {
        let tx_id =
            TxId::new(
                self.tx_id_generator
                    .next()
                    .map_err(|_| DistributedTxError::Aborted {
                        reason: "Transaction ID exhausted".to_string(),
                    })?,
            );

        let transaction =
            DistributedTransaction::new(tx_id, participants, self.transaction_timeout);

        if let Ok(mut txns) = self.active_transactions.write() {
            txns.insert(tx_id, transaction);
        }

        Ok(tx_id)
    }

    /// Execute the prepare phase of 2PC.
    pub fn prepare_distributed_transaction(&self, tx_id: TxId) -> Result<(), DistributedTxError> {
        // Get the transaction
        let mut transaction = {
            let mut txns =
                self.active_transactions
                    .write()
                    .map_err(|_| DistributedTxError::Aborted {
                        reason: "Lock poisoned".to_string(),
                    })?;
            txns.remove(&tx_id)
                .ok_or_else(|| DistributedTxError::Aborted {
                    reason: "Transaction not found".to_string(),
                })?
        };

        // Begin prepare phase
        transaction.begin_prepare()?;

        // Send prepare to all participants
        let connections = self
            .connections
            .read()
            .map_err(|_| DistributedTxError::Aborted {
                reason: "Lock poisoned".to_string(),
            })?;

        for shard_id in transaction.participant_shards() {
            if let Some(conn) = connections.get(&shard_id) {
                match conn.prepare(tx_id) {
                    Ok(()) => transaction.record_prepare_success(shard_id),
                    Err(DistributedTxError::ParticipantUnavailable { .. }) => {
                        transaction.record_unreachable(shard_id);
                    }
                    Err(_) => {
                        transaction.record_prepare_failure(shard_id);
                    }
                }
            } else {
                transaction.record_unreachable(shard_id);
            }
        }

        // Check if all prepared
        if transaction.any_aborted() || transaction.any_unreachable() {
            // Abort the transaction
            let failed: Vec<ShardId> = transaction
                .participants
                .iter()
                .filter(|(_, state)| **state != super::transaction::ParticipantState::Prepared)
                .map(|(id, _)| *id)
                .collect();

            // Send abort to prepared participants
            for shard_id in transaction.participant_shards() {
                if let Some(conn) = connections.get(&shard_id) {
                    let _ = conn.abort(tx_id);
                }
            }

            transaction.abort("Prepare phase failed");

            // Re-insert the aborted transaction for tracking
            if let Ok(mut txns) = self.active_transactions.write() {
                txns.insert(tx_id, transaction);
            }

            return Err(DistributedTxError::PrepareFailed {
                failed_participants: failed,
            });
        }

        // Mark as prepared
        transaction.mark_prepared()?;

        // Re-insert the transaction
        if let Ok(mut txns) = self.active_transactions.write() {
            txns.insert(tx_id, transaction);
        }

        Ok(())
    }

    /// Execute the commit phase of 2PC.
    ///
    /// This method follows the critical 2PC protocol:
    /// 1. Log the commit decision BEFORE sending commits
    /// 2. Send commit to all participants
    /// 3. Clear the decision after all acknowledge
    pub fn commit_distributed_transaction(&self, tx_id: TxId) -> Result<(), DistributedTxError> {
        // Get the transaction
        let mut transaction = {
            let mut txns =
                self.active_transactions
                    .write()
                    .map_err(|_| DistributedTxError::Aborted {
                        reason: "Lock poisoned".to_string(),
                    })?;
            txns.remove(&tx_id)
                .ok_or_else(|| DistributedTxError::Aborted {
                    reason: "Transaction not found".to_string(),
                })?
        };

        // CRITICAL: Log the commit decision BEFORE sending commits
        // This ensures we can recover if the coordinator crashes
        {
            let mut log = self
                .commit_log
                .write()
                .map_err(|_| DistributedTxError::Aborted {
                    reason: "Lock poisoned".to_string(),
                })?;
            log.log_commit(tx_id, transaction.participant_shards());
        }
        transaction.commit_decision_logged = true;

        // Begin commit phase
        transaction.begin_commit()?;

        // Send commit to all participants with retry
        let connections = self
            .connections
            .read()
            .map_err(|_| DistributedTxError::Aborted {
                reason: "Lock poisoned".to_string(),
            })?;

        for shard_id in transaction.participant_shards() {
            if let Some(conn) = connections.get(&shard_id) {
                // Retry logic for commit (commit is idempotent)
                let mut retries = 3;
                loop {
                    match conn.commit(tx_id) {
                        Ok(()) => {
                            transaction.record_commit_success(shard_id);
                            break;
                        }
                        Err(_) if retries > 0 => {
                            retries -= 1;
                            // In real implementation, would sleep with backoff
                            continue;
                        }
                        Err(_) => {
                            // Non-retryable or exhausted retries
                            // The commit decision is logged, recovery will retry
                            break;
                        }
                    }
                }
            }
        }

        // Check if all committed
        if !transaction.all_committed() {
            // Some participants didn't acknowledge, but commit decision is logged
            // Recovery process will retry
            let uncommitted = transaction.uncommitted_participants();
            transaction.mark_failed();

            // Re-insert for recovery tracking
            if let Ok(mut txns) = self.active_transactions.write() {
                txns.insert(tx_id, transaction);
            }

            return Err(DistributedTxError::CommitFailed {
                tx_id,
                failed_participants: uncommitted,
            });
        }

        // All committed successfully - clear the decision log
        {
            let mut log = self
                .commit_log
                .write()
                .map_err(|_| DistributedTxError::Aborted {
                    reason: "Lock poisoned".to_string(),
                })?;
            log.clear_decision(tx_id);
        }

        // Mark transaction as committed
        transaction.mark_committed()?;

        // Record metrics
        if let Ok(metrics_map) = self.metrics.read() {
            for shard_id in transaction.participant_shards() {
                if let Some(metrics) = metrics_map.get(&shard_id) {
                    metrics.record_write(true); // Distributed write
                }
            }
        }

        Ok(())
    }

    /// Abort a distributed transaction.
    pub fn abort_distributed_transaction(
        &self,
        tx_id: TxId,
        reason: &str,
    ) -> Result<(), DistributedTxError> {
        // Get the transaction
        let mut transaction = {
            let mut txns =
                self.active_transactions
                    .write()
                    .map_err(|_| DistributedTxError::Aborted {
                        reason: "Lock poisoned".to_string(),
                    })?;
            txns.remove(&tx_id)
                .ok_or_else(|| DistributedTxError::Aborted {
                    reason: "Transaction not found".to_string(),
                })?
        };

        // Log the abort decision
        {
            let mut log = self
                .commit_log
                .write()
                .map_err(|_| DistributedTxError::Aborted {
                    reason: "Lock poisoned".to_string(),
                })?;
            log.log_abort(tx_id, transaction.participant_shards());
        }

        // Send abort to all participants
        let connections = self
            .connections
            .read()
            .map_err(|_| DistributedTxError::Aborted {
                reason: "Lock poisoned".to_string(),
            })?;

        for shard_id in transaction.participant_shards() {
            if let Some(conn) = connections.get(&shard_id) {
                // Best effort abort - don't fail if participant unreachable
                let _ = conn.abort(tx_id);
            }
        }

        transaction.abort(reason);

        // Clear the decision log
        {
            let mut log = self
                .commit_log
                .write()
                .map_err(|_| DistributedTxError::Aborted {
                    reason: "Lock poisoned".to_string(),
                })?;
            log.clear_decision(tx_id);
        }

        Ok(())
    }

    /// Get an active transaction by ID.
    pub fn get_transaction(&self, tx_id: TxId) -> Option<DistributedTransaction> {
        self.active_transactions.read().ok()?.get(&tx_id).map(|tx| {
            // Create a copy of the transaction state
            DistributedTransaction {
                tx_id: tx.tx_id,
                phase: tx.phase,
                participants: tx.participants.clone(),
                start_time: tx.start_time,
                timeout: tx.timeout,
                retries_remaining: tx.retries_remaining,
                commit_decision_logged: tx.commit_decision_logged,
            }
        })
    }

    /// Recovery: replay pending commit decisions.
    ///
    /// This should be called on coordinator startup to handle
    /// transactions that were in-flight when the coordinator crashed.
    pub fn recover_pending_transactions(&self) -> Result<Vec<TxId>, DistributedTxError> {
        let decisions = {
            let log = self
                .commit_log
                .read()
                .map_err(|_| DistributedTxError::Aborted {
                    reason: "Lock poisoned".to_string(),
                })?;
            log.decisions_to_replay()
                .into_iter()
                .map(|d| (d.tx_id, d.participants.clone()))
                .collect::<Vec<_>>()
        };

        let mut recovered = Vec::new();

        for (tx_id, participants) in decisions {
            // Create a transaction in committing state for recovery
            let mut tx = DistributedTransaction::new(tx_id, participants, self.transaction_timeout);
            tx.begin_prepare().ok();
            for shard_id in tx.participant_shards() {
                tx.record_prepare_success(shard_id);
            }
            tx.mark_prepared().ok();
            tx.begin_commit().ok();
            tx.commit_decision_logged = true;

            // Insert into active transactions
            if let Ok(mut txns) = self.active_transactions.write() {
                txns.insert(tx_id, tx);
            }

            // Try to complete the commit
            match self.commit_distributed_transaction(tx_id) {
                Ok(()) => recovered.push(tx_id),
                Err(_) => {
                    // Leave in active transactions for manual intervention
                }
            }
        }

        Ok(recovered)
    }

    /// Perform health checks on all shards.
    pub fn health_check_all(&self) {
        if let Ok(mut connections) = self.connections.write() {
            for conn in connections.values_mut() {
                conn.health_check();
            }
        }
    }

    /// Mark a shard as unavailable.
    pub fn mark_shard_unavailable(&self, shard_id: ShardId) {
        if let Ok(mut connections) = self.connections.write()
            && let Some(conn) = connections.get_mut(&shard_id)
        {
            conn.mark_unhealthy();
        }

        if let Ok(mut states) = self.shard_states.write()
            && let Some(state) = states.get_mut(&shard_id)
        {
            state.status = ShardStatus::Unavailable;
        }
    }

    /// Mark a shard as available.
    pub fn mark_shard_available(&self, shard_id: ShardId) {
        if let Ok(mut connections) = self.connections.write()
            && let Some(conn) = connections.get_mut(&shard_id)
        {
            conn.mark_healthy();
        }

        if let Ok(mut states) = self.shard_states.write()
            && let Some(state) = states.get_mut(&shard_id)
        {
            state.status = ShardStatus::Healthy;
        }
    }

    /// Calculate the imbalance ratio across shards.
    ///
    /// Returns the coefficient of variation (std dev / mean) of node counts.
    pub fn calculate_imbalance(&self) -> f64 {
        let states: Vec<u64> = self
            .shard_states
            .read()
            .map(|s| s.values().map(|state| state.node_count).collect())
            .unwrap_or_default();

        if states.is_empty() || states.len() == 1 {
            return 0.0;
        }

        let mean = states.iter().sum::<u64>() as f64 / states.len() as f64;
        if mean == 0.0 {
            return 0.0;
        }

        let variance = states
            .iter()
            .map(|&x| {
                let diff = x as f64 - mean;
                diff * diff
            })
            .sum::<f64>()
            / states.len() as f64;

        variance.sqrt() / mean
    }

    /// Check if rebalancing is needed based on current imbalance.
    pub fn needs_rebalancing(&self) -> bool {
        let imbalance = self.calculate_imbalance();
        self.rebalance_config.should_rebalance(imbalance)
    }

    /// Get the number of active distributed transactions.
    pub fn active_transaction_count(&self) -> usize {
        self.active_transactions
            .read()
            .map(|txns| txns.len())
            .unwrap_or(0)
    }
}

impl std::fmt::Debug for ShardCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardCoordinator")
            .field("num_shards", &self.router.config().num_shards())
            .field("active_transactions", &self.active_transaction_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sharding::config::ShardDefinition;

    fn test_config() -> ShardConfig {
        ShardConfig::new(vec![
            ShardDefinition::new(0, "shard0:9000", vec!["Person"]),
            ShardDefinition::new(1, "shard1:9000", vec!["Place"]),
        ])
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
        assert!(conn.prepare(TxId::new(1)).is_ok());
        assert!(conn.commit(TxId::new(1)).is_ok());
        assert!(conn.abort(TxId::new(1)).is_ok());

        conn.mark_unhealthy();
        assert!(!conn.healthy);
        assert!(conn.prepare(TxId::new(2)).is_err());

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
}
