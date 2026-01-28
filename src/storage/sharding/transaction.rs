//! Distributed transaction support using Two-Phase Commit (2PC).
//!
//! This module implements distributed transactions for writes that span multiple
//! shards, ensuring ACID properties across shard boundaries.

use super::types::ShardId;
use crate::core::id::TxId;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Phase of the distributed transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionPhase {
    /// Initial phase - collecting operations.
    Pending,
    /// Phase 1 - preparing participants.
    Preparing,
    /// All participants prepared successfully.
    Prepared,
    /// Phase 2 - committing to participants.
    Committing,
    /// Transaction committed successfully.
    Committed,
    /// Transaction aborted.
    Aborted,
    /// Transaction failed and requires recovery.
    Failed,
}

impl fmt::Display for TransactionPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransactionPhase::Pending => write!(f, "Pending"),
            TransactionPhase::Preparing => write!(f, "Preparing"),
            TransactionPhase::Prepared => write!(f, "Prepared"),
            TransactionPhase::Committing => write!(f, "Committing"),
            TransactionPhase::Committed => write!(f, "Committed"),
            TransactionPhase::Aborted => write!(f, "Aborted"),
            TransactionPhase::Failed => write!(f, "Failed"),
        }
    }
}

/// State of a participant (shard) in a distributed transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantState {
    /// Participant has not yet responded to prepare.
    Unknown,
    /// Participant is ready to commit.
    Prepared,
    /// Participant has committed.
    Committed,
    /// Participant has aborted.
    Aborted,
    /// Participant is unreachable.
    Unreachable,
}

impl fmt::Display for ParticipantState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParticipantState::Unknown => write!(f, "Unknown"),
            ParticipantState::Prepared => write!(f, "Prepared"),
            ParticipantState::Committed => write!(f, "Committed"),
            ParticipantState::Aborted => write!(f, "Aborted"),
            ParticipantState::Unreachable => write!(f, "Unreachable"),
        }
    }
}

/// Error types for distributed transactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DistributedTxError {
    /// One or more participants failed to prepare.
    PrepareFailed {
        /// Participants that failed.
        failed_participants: Vec<ShardId>,
    },
    /// Commit failed on one or more participants.
    CommitFailed {
        /// Transaction ID.
        tx_id: TxId,
        /// Participants that failed to commit.
        failed_participants: Vec<ShardId>,
    },
    /// Transaction timed out.
    Timeout {
        /// The phase that timed out.
        phase: TransactionPhase,
        /// Duration before timeout.
        duration: Duration,
    },
    /// Participant is unavailable.
    ParticipantUnavailable {
        /// The unavailable shard.
        shard_id: ShardId,
    },
    /// Transaction was aborted.
    Aborted {
        /// Reason for abortion.
        reason: String,
    },
    /// Deadlock detected.
    Deadlock {
        /// Transactions involved in the deadlock.
        involved_transactions: Vec<TxId>,
    },
    /// Invalid state transition.
    InvalidStateTransition {
        /// Current phase.
        from: TransactionPhase,
        /// Attempted transition.
        to: TransactionPhase,
    },
}

impl fmt::Display for DistributedTxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DistributedTxError::PrepareFailed {
                failed_participants,
            } => {
                write!(
                    f,
                    "Prepare failed for participants: {:?}",
                    failed_participants
                )
            }
            DistributedTxError::CommitFailed {
                tx_id,
                failed_participants,
            } => {
                write!(
                    f,
                    "Commit failed for transaction {} on participants: {:?}",
                    tx_id, failed_participants
                )
            }
            DistributedTxError::Timeout { phase, duration } => {
                write!(
                    f,
                    "Transaction timeout in {} phase after {:?}",
                    phase, duration
                )
            }
            DistributedTxError::ParticipantUnavailable { shard_id } => {
                write!(f, "Participant {} is unavailable", shard_id)
            }
            DistributedTxError::Aborted { reason } => {
                write!(f, "Transaction aborted: {}", reason)
            }
            DistributedTxError::Deadlock {
                involved_transactions,
            } => {
                write!(
                    f,
                    "Deadlock detected involving transactions: {:?}",
                    involved_transactions
                )
            }
            DistributedTxError::InvalidStateTransition { from, to } => {
                write!(f, "Invalid state transition from {} to {}", from, to)
            }
        }
    }
}

impl std::error::Error for DistributedTxError {}

/// A distributed transaction spanning multiple shards.
#[derive(Debug)]
pub struct DistributedTransaction {
    /// Unique transaction ID.
    pub tx_id: TxId,
    /// Current phase of the transaction.
    pub phase: TransactionPhase,
    /// Participating shards and their states.
    pub participants: HashMap<ShardId, ParticipantState>,
    /// When the transaction started.
    pub start_time: Instant,
    /// Timeout for the entire transaction.
    pub timeout: Duration,
    /// Number of retry attempts remaining.
    pub retries_remaining: u32,
    /// Whether the commit decision has been logged.
    pub commit_decision_logged: bool,
}

impl DistributedTransaction {
    /// Create a new distributed transaction.
    pub fn new(tx_id: TxId, participants: Vec<ShardId>, timeout: Duration) -> Self {
        let mut participant_map = HashMap::new();
        for shard in participants {
            participant_map.insert(shard, ParticipantState::Unknown);
        }

        Self {
            tx_id,
            phase: TransactionPhase::Pending,
            participants: participant_map,
            start_time: Instant::now(),
            timeout,
            retries_remaining: 3,
            commit_decision_logged: false,
        }
    }

    /// Check if the transaction has timed out.
    pub fn is_timed_out(&self) -> bool {
        self.start_time.elapsed() > self.timeout
    }

    /// Get the elapsed time since transaction start.
    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Transition to the preparing phase.
    pub fn begin_prepare(&mut self) -> Result<(), DistributedTxError> {
        if self.phase != TransactionPhase::Pending {
            return Err(DistributedTxError::InvalidStateTransition {
                from: self.phase,
                to: TransactionPhase::Preparing,
            });
        }
        self.phase = TransactionPhase::Preparing;
        Ok(())
    }

    /// Record a successful prepare from a participant.
    pub fn record_prepare_success(&mut self, shard_id: ShardId) {
        if let Some(state) = self.participants.get_mut(&shard_id) {
            *state = ParticipantState::Prepared;
        }
    }

    /// Record a failed prepare from a participant.
    pub fn record_prepare_failure(&mut self, shard_id: ShardId) {
        if let Some(state) = self.participants.get_mut(&shard_id) {
            *state = ParticipantState::Aborted;
        }
    }

    /// Record that a participant is unreachable.
    pub fn record_unreachable(&mut self, shard_id: ShardId) {
        if let Some(state) = self.participants.get_mut(&shard_id) {
            *state = ParticipantState::Unreachable;
        }
    }

    /// Check if all participants have prepared successfully.
    pub fn all_prepared(&self) -> bool {
        self.participants
            .values()
            .all(|s| *s == ParticipantState::Prepared)
    }

    /// Check if any participant has aborted.
    pub fn any_aborted(&self) -> bool {
        self.participants
            .values()
            .any(|s| *s == ParticipantState::Aborted)
    }

    /// Check if any participant is unreachable.
    pub fn any_unreachable(&self) -> bool {
        self.participants
            .values()
            .any(|s| *s == ParticipantState::Unreachable)
    }

    /// Transition to the prepared state (ready for commit).
    pub fn mark_prepared(&mut self) -> Result<(), DistributedTxError> {
        if self.phase != TransactionPhase::Preparing {
            return Err(DistributedTxError::InvalidStateTransition {
                from: self.phase,
                to: TransactionPhase::Prepared,
            });
        }

        if !self.all_prepared() {
            let failed: Vec<ShardId> = self
                .participants
                .iter()
                .filter(|(_, s)| **s != ParticipantState::Prepared)
                .map(|(id, _)| *id)
                .collect();
            return Err(DistributedTxError::PrepareFailed {
                failed_participants: failed,
            });
        }

        self.phase = TransactionPhase::Prepared;
        Ok(())
    }

    /// Transition to the committing phase.
    ///
    /// CRITICAL: The commit decision MUST be logged before calling this method.
    pub fn begin_commit(&mut self) -> Result<(), DistributedTxError> {
        if self.phase != TransactionPhase::Prepared {
            return Err(DistributedTxError::InvalidStateTransition {
                from: self.phase,
                to: TransactionPhase::Committing,
            });
        }
        self.phase = TransactionPhase::Committing;
        Ok(())
    }

    /// Record a successful commit from a participant.
    pub fn record_commit_success(&mut self, shard_id: ShardId) {
        if let Some(state) = self.participants.get_mut(&shard_id) {
            *state = ParticipantState::Committed;
        }
    }

    /// Check if all participants have committed.
    pub fn all_committed(&self) -> bool {
        self.participants
            .values()
            .all(|s| *s == ParticipantState::Committed)
    }

    /// Get participants that haven't committed yet.
    pub fn uncommitted_participants(&self) -> Vec<ShardId> {
        self.participants
            .iter()
            .filter(|(_, s)| **s != ParticipantState::Committed)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Mark the transaction as committed.
    pub fn mark_committed(&mut self) -> Result<(), DistributedTxError> {
        if self.phase != TransactionPhase::Committing {
            return Err(DistributedTxError::InvalidStateTransition {
                from: self.phase,
                to: TransactionPhase::Committed,
            });
        }

        if !self.all_committed() {
            let failed = self.uncommitted_participants();
            return Err(DistributedTxError::CommitFailed {
                tx_id: self.tx_id,
                failed_participants: failed,
            });
        }

        self.phase = TransactionPhase::Committed;
        Ok(())
    }

    /// Abort the transaction.
    pub fn abort(&mut self, reason: &str) {
        self.phase = TransactionPhase::Aborted;
        for state in self.participants.values_mut() {
            if *state != ParticipantState::Committed {
                *state = ParticipantState::Aborted;
            }
        }
        // Log the abort reason
        let _ = reason; // Will be logged by the coordinator
    }

    /// Mark the transaction as failed (requires recovery).
    pub fn mark_failed(&mut self) {
        self.phase = TransactionPhase::Failed;
    }

    /// Check if the transaction can be retried.
    pub fn can_retry(&self) -> bool {
        self.retries_remaining > 0
            && !self.is_timed_out()
            && self.phase != TransactionPhase::Committed
            && self.phase != TransactionPhase::Aborted
    }

    /// Decrement the retry counter.
    pub fn decrement_retries(&mut self) {
        if self.retries_remaining > 0 {
            self.retries_remaining -= 1;
        }
    }

    /// Get the participant shards.
    pub fn participant_shards(&self) -> Vec<ShardId> {
        self.participants.keys().copied().collect()
    }
}

/// Log for recording commit decisions for crash recovery.
///
/// The commit log ensures that after a coordinator crash, we can
/// determine which transactions should be committed vs. aborted.
#[derive(Debug)]
pub struct TwoPhaseCommitLog {
    /// Pending commit decisions.
    pending_decisions: HashMap<TxId, CommitDecision>,
    /// ID generator for log sequence numbers.
    lsn_generator: AtomicU64,
}

/// A commit decision recorded in the log.
#[derive(Debug, Clone)]
pub struct CommitDecision {
    /// Transaction ID.
    pub tx_id: TxId,
    /// Log sequence number.
    pub lsn: u64,
    /// Participating shards.
    pub participants: Vec<ShardId>,
    /// Decision: true = commit, false = abort.
    pub decision: bool,
    /// When the decision was made.
    pub timestamp: Instant,
}

impl TwoPhaseCommitLog {
    /// Create a new commit log.
    pub fn new() -> Self {
        Self {
            pending_decisions: HashMap::new(),
            lsn_generator: AtomicU64::new(0),
        }
    }

    /// Log a commit decision.
    ///
    /// This MUST be called before sending commit messages to participants.
    pub fn log_commit(&mut self, tx_id: TxId, participants: Vec<ShardId>) -> u64 {
        let lsn = self.lsn_generator.fetch_add(1, Ordering::SeqCst);
        let decision = CommitDecision {
            tx_id,
            lsn,
            participants,
            decision: true,
            timestamp: Instant::now(),
        };
        self.pending_decisions.insert(tx_id, decision);
        lsn
    }

    /// Log an abort decision.
    pub fn log_abort(&mut self, tx_id: TxId, participants: Vec<ShardId>) -> u64 {
        let lsn = self.lsn_generator.fetch_add(1, Ordering::SeqCst);
        let decision = CommitDecision {
            tx_id,
            lsn,
            participants,
            decision: false,
            timestamp: Instant::now(),
        };
        self.pending_decisions.insert(tx_id, decision);
        lsn
    }

    /// Clear a decision after all participants have acknowledged.
    pub fn clear_decision(&mut self, tx_id: TxId) -> Option<CommitDecision> {
        self.pending_decisions.remove(&tx_id)
    }

    /// Get a pending decision.
    pub fn get_decision(&self, tx_id: TxId) -> Option<&CommitDecision> {
        self.pending_decisions.get(&tx_id)
    }

    /// Get all pending decisions (for recovery).
    pub fn pending_decisions(&self) -> Vec<&CommitDecision> {
        self.pending_decisions.values().collect()
    }

    /// Get commit decisions that should be replayed during recovery.
    pub fn decisions_to_replay(&self) -> Vec<&CommitDecision> {
        self.pending_decisions
            .values()
            .filter(|d| d.decision)
            .collect()
    }

    /// Get abort decisions that should be processed during recovery.
    pub fn aborts_to_process(&self) -> Vec<&CommitDecision> {
        self.pending_decisions
            .values()
            .filter(|d| !d.decision)
            .collect()
    }

    /// Check if a transaction has a pending decision.
    pub fn has_pending_decision(&self, tx_id: TxId) -> bool {
        self.pending_decisions.contains_key(&tx_id)
    }

    /// Get the current LSN (for checkpointing).
    pub fn current_lsn(&self) -> u64 {
        self.lsn_generator.load(Ordering::SeqCst)
    }
}

impl Default for TwoPhaseCommitLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tx_id(id: u64) -> TxId {
        TxId::new(id)
    }

    #[test]
    fn test_transaction_phase_display() {
        assert_eq!(format!("{}", TransactionPhase::Pending), "Pending");
        assert_eq!(format!("{}", TransactionPhase::Preparing), "Preparing");
        assert_eq!(format!("{}", TransactionPhase::Prepared), "Prepared");
        assert_eq!(format!("{}", TransactionPhase::Committing), "Committing");
        assert_eq!(format!("{}", TransactionPhase::Committed), "Committed");
        assert_eq!(format!("{}", TransactionPhase::Aborted), "Aborted");
        assert_eq!(format!("{}", TransactionPhase::Failed), "Failed");
    }

    #[test]
    fn test_participant_state_display() {
        assert_eq!(format!("{}", ParticipantState::Unknown), "Unknown");
        assert_eq!(format!("{}", ParticipantState::Prepared), "Prepared");
        assert_eq!(format!("{}", ParticipantState::Committed), "Committed");
        assert_eq!(format!("{}", ParticipantState::Aborted), "Aborted");
        assert_eq!(format!("{}", ParticipantState::Unreachable), "Unreachable");
    }

    #[test]
    fn test_distributed_tx_creation() {
        let tx_id = make_tx_id(1);
        let shards = vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()];
        let tx = DistributedTransaction::new(tx_id, shards, Duration::from_secs(30));

        assert_eq!(tx.tx_id, tx_id);
        assert_eq!(tx.phase, TransactionPhase::Pending);
        assert_eq!(tx.participants.len(), 2);
        assert!(!tx.all_prepared());
    }

    #[test]
    fn test_distributed_tx_prepare_flow() {
        let tx_id = make_tx_id(1);
        let shard0 = ShardId::new(0).unwrap();
        let shard1 = ShardId::new(1).unwrap();
        let mut tx =
            DistributedTransaction::new(tx_id, vec![shard0, shard1], Duration::from_secs(30));

        // Begin prepare
        assert!(tx.begin_prepare().is_ok());
        assert_eq!(tx.phase, TransactionPhase::Preparing);

        // Record prepare responses
        assert!(!tx.all_prepared());
        tx.record_prepare_success(shard0);
        assert!(!tx.all_prepared());
        tx.record_prepare_success(shard1);
        assert!(tx.all_prepared());

        // Mark prepared
        assert!(tx.mark_prepared().is_ok());
        assert_eq!(tx.phase, TransactionPhase::Prepared);
    }

    #[test]
    fn test_distributed_tx_commit_flow() {
        let tx_id = make_tx_id(1);
        let shard0 = ShardId::new(0).unwrap();
        let shard1 = ShardId::new(1).unwrap();
        let mut tx =
            DistributedTransaction::new(tx_id, vec![shard0, shard1], Duration::from_secs(30));

        // Prepare
        tx.begin_prepare().unwrap();
        tx.record_prepare_success(shard0);
        tx.record_prepare_success(shard1);
        tx.mark_prepared().unwrap();

        // Begin commit
        assert!(tx.begin_commit().is_ok());
        assert_eq!(tx.phase, TransactionPhase::Committing);

        // Record commit responses
        assert!(!tx.all_committed());
        tx.record_commit_success(shard0);
        assert!(!tx.all_committed());
        tx.record_commit_success(shard1);
        assert!(tx.all_committed());

        // Mark committed
        assert!(tx.mark_committed().is_ok());
        assert_eq!(tx.phase, TransactionPhase::Committed);
    }

    #[test]
    fn test_distributed_tx_prepare_failure() {
        let tx_id = make_tx_id(1);
        let shard0 = ShardId::new(0).unwrap();
        let shard1 = ShardId::new(1).unwrap();
        let mut tx =
            DistributedTransaction::new(tx_id, vec![shard0, shard1], Duration::from_secs(30));

        tx.begin_prepare().unwrap();
        tx.record_prepare_success(shard0);
        tx.record_prepare_failure(shard1);

        assert!(tx.any_aborted());
        assert!(!tx.all_prepared());

        let result = tx.mark_prepared();
        assert!(result.is_err());
        if let Err(DistributedTxError::PrepareFailed {
            failed_participants,
        }) = result
        {
            assert!(failed_participants.contains(&shard1));
        } else {
            panic!("Expected PrepareFailed error");
        }
    }

    #[test]
    fn test_distributed_tx_unreachable() {
        let tx_id = make_tx_id(1);
        let shard0 = ShardId::new(0).unwrap();
        let shard1 = ShardId::new(1).unwrap();
        let mut tx =
            DistributedTransaction::new(tx_id, vec![shard0, shard1], Duration::from_secs(30));

        tx.begin_prepare().unwrap();
        tx.record_prepare_success(shard0);
        tx.record_unreachable(shard1);

        assert!(tx.any_unreachable());
        assert!(!tx.all_prepared());
    }

    #[test]
    fn test_distributed_tx_abort() {
        let tx_id = make_tx_id(1);
        let shard0 = ShardId::new(0).unwrap();
        let shard1 = ShardId::new(1).unwrap();
        let mut tx =
            DistributedTransaction::new(tx_id, vec![shard0, shard1], Duration::from_secs(30));

        tx.begin_prepare().unwrap();
        tx.record_prepare_success(shard0);
        tx.abort("test abort");

        assert_eq!(tx.phase, TransactionPhase::Aborted);
    }

    #[test]
    fn test_distributed_tx_invalid_transitions() {
        let tx_id = make_tx_id(1);
        let mut tx = DistributedTransaction::new(
            tx_id,
            vec![ShardId::new(0).unwrap()],
            Duration::from_secs(30),
        );

        // Can't begin commit from Pending
        assert!(tx.begin_commit().is_err());

        // Can't mark prepared from Pending
        assert!(tx.mark_prepared().is_err());

        // Can't mark committed from Pending
        assert!(tx.mark_committed().is_err());

        // After begin_prepare, can't begin_prepare again
        tx.begin_prepare().unwrap();
        assert!(tx.begin_prepare().is_err());
    }

    #[test]
    fn test_distributed_tx_retry() {
        let tx_id = make_tx_id(1);
        let mut tx = DistributedTransaction::new(
            tx_id,
            vec![ShardId::new(0).unwrap()],
            Duration::from_secs(30),
        );

        assert_eq!(tx.retries_remaining, 3);
        assert!(tx.can_retry());

        tx.decrement_retries();
        assert_eq!(tx.retries_remaining, 2);
        assert!(tx.can_retry());

        tx.decrement_retries();
        tx.decrement_retries();
        assert_eq!(tx.retries_remaining, 0);
        assert!(!tx.can_retry());
    }

    #[test]
    fn test_distributed_tx_timeout() {
        let tx_id = make_tx_id(1);
        let tx = DistributedTransaction::new(
            tx_id,
            vec![ShardId::new(0).unwrap()],
            Duration::from_millis(1),
        );

        // Sleep briefly to trigger timeout
        std::thread::sleep(Duration::from_millis(10));
        assert!(tx.is_timed_out());
        assert!(!tx.can_retry());
    }

    #[test]
    fn test_two_phase_commit_log() {
        let mut log = TwoPhaseCommitLog::new();
        let tx_id = make_tx_id(1);
        let shards = vec![ShardId::new(0).unwrap(), ShardId::new(1).unwrap()];

        // Log a commit decision
        let lsn = log.log_commit(tx_id, shards.clone());
        assert_eq!(lsn, 0);
        assert!(log.has_pending_decision(tx_id));

        // Get the decision
        let decision = log.get_decision(tx_id).unwrap();
        assert_eq!(decision.tx_id, tx_id);
        assert!(decision.decision);
        assert_eq!(decision.participants.len(), 2);

        // Clear the decision
        let cleared = log.clear_decision(tx_id);
        assert!(cleared.is_some());
        assert!(!log.has_pending_decision(tx_id));
    }

    #[test]
    fn test_two_phase_commit_log_abort() {
        let mut log = TwoPhaseCommitLog::new();
        let tx_id = make_tx_id(1);
        let shards = vec![ShardId::new(0).unwrap()];

        let lsn = log.log_abort(tx_id, shards);
        assert_eq!(lsn, 0);

        let decision = log.get_decision(tx_id).unwrap();
        assert!(!decision.decision);

        let aborts = log.aborts_to_process();
        assert_eq!(aborts.len(), 1);

        let commits = log.decisions_to_replay();
        assert!(commits.is_empty());
    }

    #[test]
    fn test_two_phase_commit_log_recovery() {
        let mut log = TwoPhaseCommitLog::new();

        // Log multiple decisions
        let tx1 = make_tx_id(1);
        let tx2 = make_tx_id(2);
        let tx3 = make_tx_id(3);
        let shards = vec![ShardId::new(0).unwrap()];

        log.log_commit(tx1, shards.clone());
        log.log_abort(tx2, shards.clone());
        log.log_commit(tx3, shards.clone());

        // Check pending decisions
        let pending = log.pending_decisions();
        assert_eq!(pending.len(), 3);

        // Check commits to replay
        let commits = log.decisions_to_replay();
        assert_eq!(commits.len(), 2);

        // Check aborts to process
        let aborts = log.aborts_to_process();
        assert_eq!(aborts.len(), 1);
    }

    #[test]
    fn test_two_phase_commit_log_lsn_ordering() {
        let mut log = TwoPhaseCommitLog::new();
        let shards = vec![ShardId::new(0).unwrap()];

        let lsn1 = log.log_commit(make_tx_id(1), shards.clone());
        let lsn2 = log.log_commit(make_tx_id(2), shards.clone());
        let lsn3 = log.log_commit(make_tx_id(3), shards.clone());

        assert!(lsn1 < lsn2);
        assert!(lsn2 < lsn3);
        assert_eq!(log.current_lsn(), 3);
    }

    #[test]
    fn test_distributed_tx_uncommitted_participants() {
        let tx_id = make_tx_id(1);
        let shard0 = ShardId::new(0).unwrap();
        let shard1 = ShardId::new(1).unwrap();
        let shard2 = ShardId::new(2).unwrap();
        let mut tx = DistributedTransaction::new(
            tx_id,
            vec![shard0, shard1, shard2],
            Duration::from_secs(30),
        );

        // Prepare all
        tx.begin_prepare().unwrap();
        tx.record_prepare_success(shard0);
        tx.record_prepare_success(shard1);
        tx.record_prepare_success(shard2);
        tx.mark_prepared().unwrap();

        // Begin commit and commit some
        tx.begin_commit().unwrap();
        tx.record_commit_success(shard0);

        // Check uncommitted
        let uncommitted = tx.uncommitted_participants();
        assert_eq!(uncommitted.len(), 2);
        assert!(uncommitted.contains(&shard1));
        assert!(uncommitted.contains(&shard2));
    }

    #[test]
    fn test_distributed_tx_error_display() {
        let err = DistributedTxError::PrepareFailed {
            failed_participants: vec![ShardId::new(0).unwrap()],
        };
        assert!(format!("{}", err).contains("Prepare failed"));

        let err = DistributedTxError::Timeout {
            phase: TransactionPhase::Preparing,
            duration: Duration::from_secs(30),
        };
        assert!(format!("{}", err).contains("timeout"));
        assert!(format!("{}", err).contains("Preparing"));

        let err = DistributedTxError::Deadlock {
            involved_transactions: vec![make_tx_id(1), make_tx_id(2)],
        };
        assert!(format!("{}", err).contains("Deadlock"));
    }
}
