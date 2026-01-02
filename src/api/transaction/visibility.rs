//! Transaction visibility management for Snapshot Isolation.
//!
//! This module implements the visibility rules for Snapshot Isolation (SI),
//! which ensures that each transaction sees a consistent snapshot of the database
//! from the time it started.

use crate::api::transaction::types::TxId;
use crate::core::temporal::Timestamp;
use crate::utils::lock::MutexExt;
use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;

/// Snapshot of transaction visibility at a point in time.
///
/// A snapshot captures the set of transactions that were active when the
/// snapshot was taken. This is used to determine which versions are visible
/// to a transaction using Snapshot Isolation.
#[derive(Debug, Clone)]
pub struct TransactionSnapshot {
    /// Timestamp when snapshot was taken
    pub snapshot_timestamp: Timestamp,

    /// Transactions that were active when snapshot was taken
    /// (not yet committed or aborted)
    pub active_transactions: HashSet<TxId>,
}

impl TransactionSnapshot {
    /// Check if a version is visible in this snapshot.
    ///
    /// A version is visible if:
    /// 1. It was committed before the snapshot timestamp, AND
    /// 2. It was not created by a transaction that was active at snapshot time
    ///
    /// # Arguments
    /// * `created_by_tx` - The transaction that created this version
    /// * `commit_timestamp` - When the version was committed (None if uncommitted)
    ///
    /// # Returns
    /// `true` if the version is visible in this snapshot, `false` otherwise
    pub fn is_visible(&self, created_by_tx: TxId, commit_timestamp: Option<Timestamp>) -> bool {
        match commit_timestamp {
            None => false, // Uncommitted version - not visible
            Some(ts) => {
                // Visible if:
                // 1. Committed strictly before our snapshot (not at the same time) AND
                // 2. Not created by a transaction that was active at snapshot time
                ts < self.snapshot_timestamp && !self.active_transactions.contains(&created_by_tx)
            }
        }
    }
}

/// Manages transaction visibility for Snapshot Isolation.
///
/// The visibility manager tracks which transactions are currently active
/// and which have committed, allowing transactions to capture a consistent
/// snapshot of the database state.
///
/// # Snapshot Isolation Semantics
///
/// - Each transaction sees a consistent snapshot from its start time
/// - Transactions don't see uncommitted changes from other transactions
/// - Transactions don't see changes committed after their snapshot time
/// - Write-write conflicts are detected at commit time
pub struct TxVisibilityManager {
    /// Currently active transactions
    active: Mutex<HashSet<TxId>>,

    /// Committed transactions: TxId → commit_timestamp
    committed: Mutex<BTreeMap<TxId, Timestamp>>,
}

impl TxVisibilityManager {
    /// Create a new visibility manager.
    pub fn new() -> Self {
        TxVisibilityManager {
            active: Mutex::new(HashSet::new()),
            committed: Mutex::new(BTreeMap::new()),
        }
    }

    /// Register a new active transaction.
    ///
    /// This should be called when a transaction begins.
    ///
    /// # Arguments
    /// * `tx_id` - The ID of the transaction being started
    ///
    /// # Note
    ///
    /// Uses lock recovery to prevent cascade panics if the lock was poisoned
    /// by a panicking thread. The transaction set can safely be used after recovery.
    pub fn register_active(&self, tx_id: TxId) {
        let mut active = self.active.lock_or_recover();
        active.insert(tx_id);
    }

    /// Capture a snapshot for a transaction.
    ///
    /// The snapshot records which transactions are currently active.
    /// This snapshot will be used to determine visibility of versions
    /// throughout the transaction's lifetime.
    ///
    /// # Arguments
    /// * `snapshot_timestamp` - The timestamp for this snapshot
    ///
    /// # Returns
    /// A `TransactionSnapshot` capturing the current visibility state
    pub fn capture_snapshot(&self, snapshot_timestamp: Timestamp) -> TransactionSnapshot {
        let active = self.active.lock_or_recover();
        TransactionSnapshot {
            snapshot_timestamp,
            active_transactions: active.clone(),
        }
    }

    /// Register a transaction commit.
    ///
    /// This should be called when a transaction successfully commits.
    /// It removes the transaction from the active set and records its
    /// commit timestamp.
    ///
    /// # Arguments
    /// * `tx_id` - The ID of the committing transaction
    /// * `commit_timestamp` - When the transaction committed
    pub fn register_commit(&self, tx_id: TxId, commit_timestamp: Timestamp) {
        let mut active = self.active.lock_or_recover();
        active.remove(&tx_id);

        let mut committed = self.committed.lock_or_recover();
        committed.insert(tx_id, commit_timestamp);
    }

    /// Register a transaction abort.
    ///
    /// This should be called when a transaction aborts (rolls back).
    /// It simply removes the transaction from the active set.
    ///
    /// # Arguments
    /// * `tx_id` - The ID of the aborting transaction
    pub fn register_abort(&self, tx_id: TxId) {
        let mut active = self.active.lock_or_recover();
        active.remove(&tx_id);
    }

    /// Check if a version is visible in a snapshot.
    ///
    /// This applies the Snapshot Isolation visibility rules to determine
    /// if a version created by `created_by_tx` is visible in the given snapshot.
    ///
    /// # Arguments
    /// * `snapshot` - The transaction's snapshot
    /// * `created_by_tx` - The transaction that created the version
    ///
    /// # Returns
    /// `true` if the version is visible, `false` otherwise
    pub fn is_visible(&self, snapshot: &TransactionSnapshot, created_by_tx: TxId) -> bool {
        // Special case: TxId(0) is for pre-existing data (e.g., test fixtures, migrations)
        // Always treat it as visible to ensure backward compatibility
        if created_by_tx.as_u64() == 0 {
            return true;
        }

        // Check if transaction committed
        let committed = self.committed.lock_or_recover();

        match committed.get(&created_by_tx) {
            None => false, // Not committed - not visible
            Some(&commit_ts) => {
                // Apply visibility rules from snapshot
                snapshot.is_visible(created_by_tx, Some(commit_ts))
            }
        }
    }

    /// Get the number of active transactions.
    ///
    /// This is primarily useful for testing and monitoring.
    #[allow(dead_code)]
    pub fn active_count(&self) -> usize {
        let active = self.active.lock_or_recover();
        active.len()
    }

    /// Get the number of committed transactions tracked.
    ///
    /// This is primarily useful for testing and monitoring.
    #[allow(dead_code)]
    pub fn committed_count(&self) -> usize {
        let committed = self.committed.lock_or_recover();
        committed.len()
    }
}

impl Default for TxVisibilityManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_visibility_committed_before() {
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100,
            active_transactions: HashSet::new(),
        };

        // Version committed before snapshot - visible
        assert!(snapshot.is_visible(TxId::new(1), Some(50)));
    }

    #[test]
    fn test_snapshot_visibility_committed_after() {
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100,
            active_transactions: HashSet::new(),
        };

        // Version committed after snapshot - not visible
        assert!(!snapshot.is_visible(TxId::new(1), Some(150)));
    }

    #[test]
    fn test_snapshot_visibility_uncommitted() {
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100,
            active_transactions: HashSet::new(),
        };

        // Uncommitted version - not visible
        assert!(!snapshot.is_visible(TxId::new(1), None));
    }

    #[test]
    fn test_snapshot_visibility_active_transaction() {
        let mut active = HashSet::new();
        active.insert(TxId::new(1));

        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100,
            active_transactions: active,
        };

        // Version from active transaction - not visible even if committed before snapshot
        assert!(!snapshot.is_visible(TxId::new(1), Some(50)));
    }

    #[test]
    fn test_visibility_manager_creation() {
        let manager = TxVisibilityManager::new();
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.committed_count(), 0);
    }

    #[test]
    fn test_register_active() {
        let manager = TxVisibilityManager::new();
        manager.register_active(TxId::new(1));
        manager.register_active(TxId::new(2));

        assert_eq!(manager.active_count(), 2);
    }

    #[test]
    fn test_capture_snapshot() {
        let manager = TxVisibilityManager::new();
        manager.register_active(TxId::new(1));
        manager.register_active(TxId::new(2));

        let snapshot = manager.capture_snapshot(100);

        assert_eq!(snapshot.snapshot_timestamp, 100);
        assert_eq!(snapshot.active_transactions.len(), 2);
        assert!(snapshot.active_transactions.contains(&TxId::new(1)));
        assert!(snapshot.active_transactions.contains(&TxId::new(2)));
    }

    #[test]
    fn test_register_commit() {
        let manager = TxVisibilityManager::new();
        manager.register_active(TxId::new(1));

        assert_eq!(manager.active_count(), 1);
        assert_eq!(manager.committed_count(), 0);

        manager.register_commit(TxId::new(1), 100);

        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.committed_count(), 1);
    }

    #[test]
    fn test_register_abort() {
        let manager = TxVisibilityManager::new();
        manager.register_active(TxId::new(1));

        assert_eq!(manager.active_count(), 1);

        manager.register_abort(TxId::new(1));

        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.committed_count(), 0);
    }

    #[test]
    fn test_is_visible_committed_transaction() {
        let manager = TxVisibilityManager::new();

        // Start and commit transaction 1
        manager.register_active(TxId::new(1));
        manager.register_commit(TxId::new(1), 50);

        // Take snapshot after commit
        let snapshot = manager.capture_snapshot(100);

        // Version from tx1 should be visible
        assert!(manager.is_visible(&snapshot, TxId::new(1)));
    }

    #[test]
    fn test_is_visible_uncommitted_transaction() {
        let manager = TxVisibilityManager::new();

        // Start transaction 1 but don't commit
        manager.register_active(TxId::new(1));

        let snapshot = manager.capture_snapshot(100);

        // Version from uncommitted tx1 should not be visible
        assert!(!manager.is_visible(&snapshot, TxId::new(1)));
    }

    #[test]
    fn test_is_visible_concurrent_transaction() {
        let manager = TxVisibilityManager::new();

        // Start tx1
        manager.register_active(TxId::new(1));

        // Take snapshot (tx1 is active)
        let snapshot = manager.capture_snapshot(100);

        // Commit tx1 after snapshot
        manager.register_commit(TxId::new(1), 90);

        // Even though tx1 committed before snapshot timestamp,
        // it was active at snapshot time, so not visible
        assert!(!manager.is_visible(&snapshot, TxId::new(1)));
    }

    #[test]
    fn test_concurrent_snapshots() {
        let manager = TxVisibilityManager::new();

        // Start tx1
        manager.register_active(TxId::new(1));

        // Snapshot 1 - sees tx1 as active
        let snapshot1 = manager.capture_snapshot(100);
        assert_eq!(snapshot1.active_transactions.len(), 1);

        // Commit tx1, start tx2
        manager.register_commit(TxId::new(1), 110);
        manager.register_active(TxId::new(2));

        // Snapshot 2 - sees tx2 as active, tx1 committed
        let snapshot2 = manager.capture_snapshot(120);
        assert_eq!(snapshot2.active_transactions.len(), 1);
        assert!(snapshot2.active_transactions.contains(&TxId::new(2)));

        // Original snapshot1 unchanged
        assert_eq!(snapshot1.active_transactions.len(), 1);
        assert!(snapshot1.active_transactions.contains(&TxId::new(1)));
    }
}
