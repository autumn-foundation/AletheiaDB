//! Shard coordinator for managing distributed operations.

use super::config::{RebalanceConfig, ShardConfig};
use super::router::{ShardRouter, TraversalPlan};
use super::transaction::{
    DistributedTransaction, DistributedTxError, TransactionPhase, TwoPhaseCommitLog,
};
use super::types::{ShardId, ShardMetrics, ShardState, ShardStatus};
use crate::core::hlc::{
    HybridTimestamp, MAX_FORWARD_JUMP_US, SendWithSelfHealError, evaluate_clock_skew,
    is_clock_skew_self_heal_enabled, send_with_overflow_self_heal,
};
use crate::core::id::{IdGenerator, TxId};
use crate::core::temporal::time;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::core::hlc::MAX_BACKWARD_DRIFT_US;

/// Result of recovery operation.
#[derive(Debug, Clone)]
pub struct RecoveryResult {
    /// Transactions that were successfully recovered.
    pub recovered: Vec<TxId>,
    /// Transactions that failed recovery and were dead-lettered.
    pub dead_lettered: Vec<DeadLetteredTransaction>,
}

impl RecoveryResult {
    /// Check if recovery was fully successful (no dead letters).
    pub fn is_complete(&self) -> bool {
        self.dead_lettered.is_empty()
    }

    /// Get the number of transactions that required manual intervention.
    pub fn dead_letter_count(&self) -> usize {
        self.dead_lettered.len()
    }
}

/// A transaction that failed recovery and requires manual intervention.
#[derive(Debug, Clone)]
pub struct DeadLetteredTransaction {
    /// The transaction ID.
    pub tx_id: TxId,
    /// Reason for dead-lettering.
    pub reason: String,
    /// Time of last recovery attempt.
    pub last_attempt: Instant,
    /// Number of recovery attempts made.
    pub attempt_count: u32,
}

/// Connection to a shard (placeholder for actual network implementation).
#[derive(Debug)]
pub struct ShardConnection {
    /// The shard ID this connection is for.
    pub shard_id: ShardId,
    /// Endpoint address.
    pub endpoint: String,
    /// Whether the connection is healthy.
    pub healthy: bool,
    /// Remote HLC frontier observed from this shard.
    hlc_frontier: Mutex<HybridTimestamp>,
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
            hlc_frontier: Mutex::new(time::now()),
            last_ping: None,
        }
    }

    /// Simulate a prepare call to the shard.
    pub fn prepare(
        &self,
        _tx_id: TxId,
        timestamp: Option<HybridTimestamp>,
    ) -> Result<(), DistributedTxError> {
        self.apply_remote_timestamp(timestamp);
        if !self.healthy {
            return Err(DistributedTxError::ParticipantUnavailable {
                shard_id: self.shard_id,
            });
        }
        // In a real implementation, this would make an RPC call
        Ok(())
    }

    /// Simulate a commit call to the shard.
    pub fn commit(
        &self,
        _tx_id: TxId,
        commit_timestamp: Option<HybridTimestamp>,
    ) -> Result<(), DistributedTxError> {
        self.apply_remote_timestamp(commit_timestamp);
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

    fn apply_remote_timestamp(&self, timestamp: Option<HybridTimestamp>) {
        if let Some(remote_ts) = timestamp
            && let Ok(mut frontier) = self.hlc_frontier.lock()
            && let Ok(updated) = frontier.receive(remote_ts, time::now().wallclock())
        {
            *frontier = updated;
        }
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
    /// Coordinating HLC frontier used to assign cross-shard commit timestamps.
    commit_clock: Mutex<HybridTimestamp>,
    /// Monotonic observation time for commit clock drift checks.
    commit_clock_observed_at: Mutex<Instant>,
    /// Metrics per shard.
    metrics: RwLock<HashMap<ShardId, Arc<ShardMetrics>>>,
    /// Rebalance configuration.
    rebalance_config: RebalanceConfig,
    /// Timeout for transaction operations.
    transaction_timeout: Duration,
    /// Dead letter queue for transactions that failed recovery.
    dead_letter_queue: RwLock<HashMap<TxId, DeadLetteredTransaction>>,
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
            commit_clock: Mutex::new(time::now()),
            commit_clock_observed_at: Mutex::new(Instant::now()),
            metrics: RwLock::new(metrics),
            rebalance_config: RebalanceConfig::default(),
            transaction_timeout,
            dead_letter_queue: RwLock::new(HashMap::new()),
        }
    }

    /// Create a coordinator with custom rebalance config.
    pub fn with_rebalance_config(mut self, config: RebalanceConfig) -> Self {
        self.rebalance_config = config;
        self
    }

    fn reinsert_transaction(&self, tx_id: TxId, transaction: DistributedTransaction) {
        if let Ok(mut txns) = self.active_transactions.write() {
            txns.insert(tx_id, transaction);
        }
    }

    fn adaptive_forward_jump_limit_us(
        &self,
        observed_at: Instant,
    ) -> Result<i64, DistributedTxError> {
        let mut previous_observed_at =
            self.commit_clock_observed_at
                .lock()
                .map_err(|_| DistributedTxError::Aborted {
                    reason: "Clock observation lock poisoned".to_string(),
                })?;
        let elapsed = observed_at.duration_since(*previous_observed_at);
        *previous_observed_at = observed_at;

        let elapsed_us = i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX);
        Ok(MAX_FORWARD_JUMP_US.saturating_add(elapsed_us))
    }

    fn next_commit_timestamp(&self) -> Result<HybridTimestamp, DistributedTxError> {
        let mut frontier = self
            .commit_clock
            .lock()
            .map_err(|_| DistributedTxError::Aborted {
                reason: "Clock frontier lock poisoned".to_string(),
            })?;

        let current_wallclock = time::now();
        let self_heal_clock_skew = is_clock_skew_self_heal_enabled();
        let observed_at = Instant::now();
        let adaptive_forward_limit_us = self.adaptive_forward_jump_limit_us(observed_at)?;
        let skew_decision = evaluate_clock_skew(
            current_wallclock.wallclock(),
            frontier.wallclock(),
            Some(adaptive_forward_limit_us),
            self_heal_clock_skew,
        )
        .map_err(|violation| DistributedTxError::Aborted {
            reason: format!(
                "Clock skew detected: {} drift {}us exceeds max {}us",
                violation.direction.as_str(),
                violation.drift_us,
                violation.max_allowed
            ),
        })?;

        if self_heal_clock_skew && let Some(_direction) = skew_decision.healed_direction {
            #[cfg(feature = "observability")]
            tracing::warn!(
                wallclock_ts = %current_wallclock,
                prev_ts = %frontier,
                drift_us = skew_decision.drift_us,
                reason = _direction.as_str(),
                "Self-healing clock skew by clamping to local HLC frontier"
            );
        }

        let next = send_with_overflow_self_heal(
            &frontier,
            skew_decision.effective_wallclock,
            self_heal_clock_skew,
            |error| match error {
                SendWithSelfHealError::InitialSend(error) => DistributedTxError::Aborted {
                    reason: format!("Failed to advance HLC frontier: {}", error),
                },
                SendWithSelfHealError::FallbackWallclockOverflow {
                    wallclock,
                    current_logical: _,
                } => DistributedTxError::Aborted {
                    reason: format!(
                        "HLC logical counter overflow while self-healing at wallclock={}",
                        wallclock
                    ),
                },
                SendWithSelfHealError::FallbackSend(fallback_error) => {
                    DistributedTxError::Aborted {
                        reason: format!(
                            "HLC timestamp generation failed while self-healing: {}",
                            fallback_error
                        ),
                    }
                }
            },
        )?;

        *frontier = next;
        Ok(next)
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
        if let Err(error) = transaction.begin_prepare() {
            self.reinsert_transaction(tx_id, transaction);
            return Err(error);
        }

        // Allocate a single commit timestamp for prepare + commit.
        // This keeps participant RPCs causally ordered even if wallclock
        // shifts during the two-phase commit sequence.
        if transaction.commit_timestamp.is_none() {
            match self.next_commit_timestamp() {
                Ok(timestamp) => transaction.commit_timestamp = Some(timestamp),
                Err(error) => {
                    // No participant has seen this transaction yet, so reset phase to pending.
                    transaction.phase = TransactionPhase::Pending;
                    self.reinsert_transaction(tx_id, transaction);
                    return Err(error);
                }
            }
        }
        let prepare_timestamp = transaction.commit_timestamp;

        // Send prepare to all participants
        let connections = match self.connections.read() {
            Ok(connections) => connections,
            Err(_) => {
                transaction.phase = TransactionPhase::Pending;
                self.reinsert_transaction(tx_id, transaction);
                return Err(DistributedTxError::Aborted {
                    reason: "Lock poisoned".to_string(),
                });
            }
        };

        for shard_id in transaction.participant_shards() {
            if let Some(conn) = connections.get(&shard_id) {
                match conn.prepare(tx_id, prepare_timestamp) {
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

        let commit_timestamp = if let Some(commit_timestamp) = transaction.commit_timestamp {
            Some(commit_timestamp)
        } else {
            match self.next_commit_timestamp() {
                Ok(timestamp) => {
                    transaction.commit_timestamp = Some(timestamp);
                    Some(timestamp)
                }
                Err(error) => {
                    self.reinsert_transaction(tx_id, transaction);
                    return Err(error);
                }
            }
        };

        // CRITICAL: Log the commit decision BEFORE sending commits
        // This ensures we can recover if the coordinator crashes
        {
            let mut log = match self.commit_log.write() {
                Ok(log) => log,
                Err(_) => {
                    self.reinsert_transaction(tx_id, transaction);
                    return Err(DistributedTxError::Aborted {
                        reason: "Lock poisoned".to_string(),
                    });
                }
            };

            let should_log = match log.get_decision(tx_id) {
                Some(existing) => {
                    !transaction.commit_decision_logged
                        || existing.commit_timestamp != commit_timestamp
                        || !existing.decision
                }
                None => true,
            };

            if should_log {
                log.log_commit(tx_id, transaction.participant_shards(), commit_timestamp);
                transaction.commit_decision_logged = true;
            }
        }

        // Begin commit phase
        match transaction.phase {
            TransactionPhase::Committing => {}
            TransactionPhase::Failed | TransactionPhase::Prepared => {
                transaction.phase = TransactionPhase::Committing;
            }
            _ => {
                if let Err(error) = transaction.begin_commit() {
                    self.reinsert_transaction(tx_id, transaction);
                    return Err(error);
                }
            }
        }

        // Send commit to all participants with retry
        let connections = match self.connections.read() {
            Ok(connections) => connections,
            Err(_) => {
                self.reinsert_transaction(tx_id, transaction);
                return Err(DistributedTxError::Aborted {
                    reason: "Lock poisoned".to_string(),
                });
            }
        };

        for shard_id in transaction.participant_shards() {
            if let Some(conn) = connections.get(&shard_id) {
                // Retry logic for commit with exponential backoff
                let max_retries = 3;
                let mut retry_count = 0;

                loop {
                    match conn.commit(tx_id, commit_timestamp) {
                        Ok(()) => {
                            transaction.record_commit_success(shard_id);
                            break;
                        }
                        Err(_) if retry_count < max_retries => {
                            // Exponential backoff: 100ms, 200ms, 400ms
                            let backoff_ms = 100 * (1 << retry_count);
                            std::thread::sleep(Duration::from_millis(backoff_ms));
                            retry_count += 1;
                            continue;
                        }
                        Err(_) => {
                            // Exhausted retries - commit decision is logged, recovery will retry
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
                commit_timestamp: tx.commit_timestamp,
            }
        })
    }

    /// Recovery: replay pending commit decisions.
    ///
    /// This should be called on coordinator startup to handle
    /// transactions that were in-flight when the coordinator crashed.
    ///
    /// Returns a tuple of (successfully recovered, dead-lettered transactions).
    /// Dead-lettered transactions are those that exceeded max recovery attempts
    /// and require manual intervention.
    pub fn recover_pending_transactions(&self) -> Result<RecoveryResult, DistributedTxError> {
        let decisions = {
            let log = self
                .commit_log
                .read()
                .map_err(|_| DistributedTxError::Aborted {
                    reason: "Lock poisoned".to_string(),
                })?;
            log.decisions_to_replay()
                .into_iter()
                .map(|d| (d.tx_id, d.participants.clone(), d.commit_timestamp))
                .collect::<Vec<_>>()
        };

        let mut recovered = Vec::new();
        let mut dead_lettered = Vec::new();
        let max_recovery_attempts = 5;

        for (tx_id, participants, commit_timestamp) in decisions {
            // Create a transaction in committing state for recovery
            let mut tx = DistributedTransaction::new(tx_id, participants, self.transaction_timeout);
            tx.begin_prepare().ok();
            for shard_id in tx.participant_shards() {
                tx.record_prepare_success(shard_id);
            }
            tx.mark_prepared().ok();
            tx.begin_commit().ok();
            tx.commit_timestamp = commit_timestamp;
            tx.commit_decision_logged = true;

            // Insert into active transactions
            if let Ok(mut txns) = self.active_transactions.write() {
                txns.insert(tx_id, tx);
            }

            // Try to complete the commit with exponential backoff
            let mut attempts = 0;
            let mut success = false;

            while attempts < max_recovery_attempts && !success {
                match self.commit_distributed_transaction(tx_id) {
                    Ok(()) => {
                        recovered.push(tx_id);
                        success = true;
                    }
                    Err(e) => {
                        attempts += 1;
                        if attempts < max_recovery_attempts {
                            // Exponential backoff: 1s, 2s, 4s, 8s, 16s
                            let backoff_secs = 1 << attempts;
                            #[cfg(feature = "observability")]
                            tracing::warn!(
                                tx_id = %tx_id,
                                attempt = attempts,
                                backoff_secs = backoff_secs,
                                error = %e,
                                "Recovery attempt failed, retrying"
                            );
                            std::thread::sleep(Duration::from_secs(backoff_secs));
                        } else {
                            // Max attempts exceeded - dead letter this transaction
                            #[cfg(feature = "observability")]
                            tracing::error!(
                                tx_id = %tx_id,
                                max_attempts = max_recovery_attempts,
                                "Transaction exceeded max recovery attempts, moving to dead letter queue"
                            );
                            dead_lettered.push(DeadLetteredTransaction {
                                tx_id,
                                reason: format!("Exceeded max recovery attempts: {}", e),
                                last_attempt: Instant::now(),
                                attempt_count: attempts,
                            });

                            // Remove from active transactions to prevent unbounded growth
                            if let Ok(mut txns) = self.active_transactions.write() {
                                txns.remove(&tx_id);
                            }
                        }
                    }
                }
            }
        }

        // Store dead-lettered transactions
        if let Ok(mut dlq) = self.dead_letter_queue.write() {
            for tx in &dead_lettered {
                dlq.insert(tx.tx_id, tx.clone());
            }
        }

        Ok(RecoveryResult {
            recovered,
            dead_lettered,
        })
    }

    /// Get transactions in the dead letter queue.
    ///
    /// These are transactions that failed recovery and require manual intervention.
    pub fn get_dead_lettered_transactions(&self) -> Vec<DeadLetteredTransaction> {
        self.dead_letter_queue
            .read()
            .map(|dlq| dlq.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Manually retry a dead-lettered transaction.
    ///
    /// This removes the transaction from the dead letter queue and attempts
    /// recovery again.
    pub fn retry_dead_lettered_transaction(&self, tx_id: TxId) -> Result<(), DistributedTxError> {
        // Remove from dead letter queue
        let dlq_entry = {
            let mut dlq =
                self.dead_letter_queue
                    .write()
                    .map_err(|_| DistributedTxError::Aborted {
                        reason: "Lock poisoned".to_string(),
                    })?;
            dlq.remove(&tx_id)
        };

        if dlq_entry.is_none() {
            return Err(DistributedTxError::Aborted {
                reason: format!("Transaction {} not found in dead letter queue", tx_id),
            });
        }

        // Re-attempt recovery using single-transaction recovery
        let decisions = {
            let log = self
                .commit_log
                .read()
                .map_err(|_| DistributedTxError::Aborted {
                    reason: "Lock poisoned".to_string(),
                })?;
            log.decisions_to_replay()
                .into_iter()
                .filter(|d| d.tx_id == tx_id)
                .map(|d| (d.tx_id, d.participants.clone(), d.commit_timestamp))
                .collect::<Vec<_>>()
        };

        if let Some((found_tx_id, participants, commit_timestamp)) = decisions.into_iter().next() {
            let mut tx =
                DistributedTransaction::new(found_tx_id, participants, self.transaction_timeout);
            tx.begin_prepare().ok();
            for shard_id in tx.participant_shards() {
                tx.record_prepare_success(shard_id);
            }
            tx.mark_prepared().ok();
            tx.begin_commit().ok();
            tx.commit_timestamp = commit_timestamp;
            tx.commit_decision_logged = true;

            if let Ok(mut txns) = self.active_transactions.write() {
                txns.insert(found_tx_id, tx);
            }

            self.commit_distributed_transaction(found_tx_id)
        } else {
            Err(DistributedTxError::Aborted {
                reason: format!("No commit decision found for transaction {}", tx_id),
            })
        }
    }

    /// Clear all dead-lettered transactions (after manual resolution).
    pub fn clear_dead_letter_queue(&self) {
        if let Ok(mut dlq) = self.dead_letter_queue.write() {
            dlq.clear();
        }
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

        {
            let mut observed_at = coordinator
                .commit_clock_observed_at
                .lock()
                .expect("commit_clock_observed_at lock should be available");

            // Handle potential underflow on systems with short uptime (e.g. fresh CI runners)
            // Instant is monotonic from boot time.
            let now = Instant::now();
            let duration = Duration::from_micros(idle_gap_us as u64);
            if let Some(past_time) = now.checked_sub(duration) {
                *observed_at = past_time;
            } else {
                println!(
                    "Skipping test_next_commit_timestamp_allows_idle_forward_drift: system uptime insufficient for simulation"
                );
                return;
            }
        }

        let result = coordinator.next_commit_timestamp();
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
}
