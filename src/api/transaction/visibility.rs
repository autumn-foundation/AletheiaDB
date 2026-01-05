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
///
/// # Lock Recovery Safety
///
/// This struct uses `lock_or_recover()` for all lock acquisitions. This is safe
/// because the protected data structures (`HashSet<TxId>` and `BTreeMap<TxId, Timestamp>`)
/// have no complex invariants that could be violated by a mid-operation panic:
///
/// - **Worst case**: A transaction ID may be missing from the active/committed sets
/// - **Behavior**: This fails safe by being conservative (treating as uncommitted/not visible)
/// - **No corruption**: Standard library collections remain valid after partial operations
pub struct TxVisibilityManager {
    /// Currently active transactions
    active: Mutex<HashSet<TxId>>,

    /// Committed transactions: TxId → commit_timestamp
    committed: Mutex<BTreeMap<TxId, Timestamp>>,

    /// Active snapshots: TxId → snapshot_timestamp
    /// Tracked to determine safe cleanup threshold (watermark)
    active_snapshots: Mutex<BTreeMap<TxId, Timestamp>>,

    /// Counter for triggering periodic cleanup
    commit_counter: std::sync::atomic::AtomicUsize,

    /// How often to trigger cleanup (every N commits)
    cleanup_interval: usize,
}

impl TxVisibilityManager {
    /// Create a new visibility manager with default cleanup interval.
    pub fn new() -> Self {
        Self::with_cleanup_interval(1000)
    }

    /// Create a new visibility manager with custom cleanup interval.
    ///
    /// # Arguments
    /// * `interval` - Number of commits between cleanup operations
    ///
    /// # Note
    /// A smaller interval means more frequent cleanup (less memory usage, more CPU).
    /// A larger interval means less frequent cleanup (more memory usage, less CPU).
    /// Default is 1000 commits.
    pub fn with_cleanup_interval(interval: usize) -> Self {
        TxVisibilityManager {
            active: Mutex::new(HashSet::new()),
            committed: Mutex::new(BTreeMap::new()),
            active_snapshots: Mutex::new(BTreeMap::new()),
            commit_counter: std::sync::atomic::AtomicUsize::new(0),
            cleanup_interval: interval,
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
    /// The snapshot is also registered for watermark tracking, which enables
    /// cleanup of old committed transaction records.
    ///
    /// # Arguments
    /// * `snapshot_timestamp` - The timestamp for this snapshot
    /// * `tx_id` - The ID of the transaction taking this snapshot
    ///
    /// # Returns
    /// A `TransactionSnapshot` capturing the current visibility state
    pub fn capture_snapshot(&self, snapshot_timestamp: Timestamp, tx_id: TxId) -> TransactionSnapshot {
        let active = self.active.lock_or_recover();

        // Register snapshot for watermark tracking
        let mut active_snapshots = self.active_snapshots.lock_or_recover();
        active_snapshots.insert(tx_id, snapshot_timestamp);
        drop(active_snapshots); // Release lock early

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
    /// This method also unregisters the transaction's snapshot and may
    /// trigger periodic cleanup of old committed records.
    ///
    /// # Arguments
    /// * `tx_id` - The ID of the committing transaction
    /// * `commit_timestamp` - When the transaction committed
    pub fn register_commit(&self, tx_id: TxId, commit_timestamp: Timestamp) {
        let mut active = self.active.lock_or_recover();
        active.remove(&tx_id);

        let mut committed = self.committed.lock_or_recover();
        committed.insert(tx_id, commit_timestamp);
        drop(committed);
        drop(active);

        // Unregister snapshot
        let mut active_snapshots = self.active_snapshots.lock_or_recover();
        active_snapshots.remove(&tx_id);
        drop(active_snapshots);

        // Periodic cleanup trigger
        let count = self
            .commit_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count.is_multiple_of(self.cleanup_interval) {
            self.cleanup_old_committed();
        }
    }

    /// Register a transaction abort.
    ///
    /// This should be called when a transaction aborts (rolls back).
    /// It removes the transaction from the active set and unregisters
    /// its snapshot.
    ///
    /// # Arguments
    /// * `tx_id` - The ID of the aborting transaction
    pub fn register_abort(&self, tx_id: TxId) {
        let mut active = self.active.lock_or_recover();
        active.remove(&tx_id);
        drop(active);

        // Unregister snapshot
        let mut active_snapshots = self.active_snapshots.lock_or_recover();
        active_snapshots.remove(&tx_id);
    }

    /// Clean up old committed transaction records no longer needed.
    ///
    /// Removes committed transactions whose commit timestamps are older
    /// than the oldest active snapshot. This is safe because:
    /// 1. Visibility rule: commit_ts < snapshot_ts
    /// 2. If commit_ts < min(all snapshots), no active snapshot can reference it
    /// 3. Future snapshots will have even higher timestamps
    ///
    /// # Returns
    /// The number of records removed
    ///
    /// # Implementation Note
    /// This method is automatically called periodically by `register_commit`
    /// based on the cleanup interval. Manual calls are also safe and will
    /// return 0 if no cleanup is possible.
    pub fn cleanup_old_committed(&self) -> usize {
        // Find watermark: minimum snapshot timestamp
        let watermark = {
            let active_snapshots = self.active_snapshots.lock_or_recover();

            if active_snapshots.is_empty() {
                // No active snapshots - can't safely cleanup
                // (future snapshots might need these records)
                return 0;
            }

            active_snapshots
                .values()
                .copied()
                .min()
                .expect("checked non-empty above")
        };

        // Remove old committed records
        let mut committed = self.committed.lock_or_recover();
        let original_count = committed.len();

        // Retain only records that might be visible to active snapshots
        // Keep if: commit_timestamp >= watermark
        // (Because visibility check is: commit_ts < snapshot_ts)
        committed.retain(|_tx_id, &mut commit_ts| commit_ts >= watermark);

        original_count - committed.len()
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

    /// Get visibility manager statistics for monitoring.
    ///
    /// Returns a tuple of (active_count, committed_count, snapshot_count).
    /// Useful for monitoring memory usage and cleanup effectiveness.
    ///
    /// # Returns
    /// (active_count, committed_count, snapshot_count)
    pub fn stats(&self) -> (usize, usize, usize) {
        let active = self.active.lock_or_recover();
        let committed = self.committed.lock_or_recover();
        let snapshots = self.active_snapshots.lock_or_recover();
        (active.len(), committed.len(), snapshots.len())
    }

    /// Get the current cleanup watermark (minimum active snapshot timestamp).
    ///
    /// The watermark represents the oldest snapshot timestamp currently active.
    /// Committed records older than this watermark are candidates for cleanup.
    ///
    /// # Returns
    /// `Some(timestamp)` if there are active snapshots, `None` otherwise
    pub fn watermark(&self) -> Option<Timestamp> {
        let snapshots = self.active_snapshots.lock_or_recover();
        snapshots.values().copied().min()
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

        let snapshot = manager.capture_snapshot(100, TxId::new(999));

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
        let snapshot = manager.capture_snapshot(100, TxId::new(999));

        // Version from tx1 should be visible
        assert!(manager.is_visible(&snapshot, TxId::new(1)));
    }

    #[test]
    fn test_is_visible_uncommitted_transaction() {
        let manager = TxVisibilityManager::new();

        // Start transaction 1 but don't commit
        manager.register_active(TxId::new(1));

        let snapshot = manager.capture_snapshot(100, TxId::new(999));

        // Version from uncommitted tx1 should not be visible
        assert!(!manager.is_visible(&snapshot, TxId::new(1)));
    }

    #[test]
    fn test_is_visible_concurrent_transaction() {
        let manager = TxVisibilityManager::new();

        // Start tx1
        manager.register_active(TxId::new(1));

        // Take snapshot (tx1 is active)
        let snapshot = manager.capture_snapshot(100, TxId::new(999));

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
        let snapshot1 = manager.capture_snapshot(100, TxId::new(998));
        assert_eq!(snapshot1.active_transactions.len(), 1);

        // Commit tx1, start tx2
        manager.register_commit(TxId::new(1), 110);
        manager.register_active(TxId::new(2));

        // Snapshot 2 - sees tx2 as active, tx1 committed
        let snapshot2 = manager.capture_snapshot(120, TxId::new(997));
        assert_eq!(snapshot2.active_transactions.len(), 1);
        assert!(snapshot2.active_transactions.contains(&TxId::new(2)));

        // Original snapshot1 unchanged
        assert_eq!(snapshot1.active_transactions.len(), 1);
        assert!(snapshot1.active_transactions.contains(&TxId::new(1)));
    }

    #[test]
    fn test_cleanup_removes_old_entries() {
        let manager = TxVisibilityManager::new();

        // Commit transactions at various times
        manager.register_active(TxId::new(1));
        manager.register_commit(TxId::new(1), 50);

        manager.register_active(TxId::new(2));
        manager.register_commit(TxId::new(2), 100);

        manager.register_active(TxId::new(3));
        manager.register_commit(TxId::new(3), 150);

        manager.register_active(TxId::new(4));
        manager.register_commit(TxId::new(4), 200);

        assert_eq!(manager.committed_count(), 4);

        // Create snapshot at timestamp 150 (watermark)
        manager.register_active(TxId::new(10));
        manager.capture_snapshot(150, TxId::new(10));

        // Cleanup - should remove entries with commit_ts < 150 (tx1, tx2)
        let removed = manager.cleanup_old_committed();
        assert_eq!(removed, 2);
        assert_eq!(manager.committed_count(), 2);
    }

    #[test]
    fn test_no_cleanup_without_snapshots() {
        let manager = TxVisibilityManager::new();

        manager.register_active(TxId::new(1));
        manager.register_commit(TxId::new(1), 100);

        manager.register_active(TxId::new(2));
        manager.register_commit(TxId::new(2), 200);

        // No snapshots - cleanup should remove nothing
        let removed = manager.cleanup_old_committed();
        assert_eq!(removed, 0);
        assert_eq!(manager.committed_count(), 2);
    }

    #[test]
    fn test_automatic_cleanup_trigger() {
        // Use small interval for testing
        let manager = TxVisibilityManager::with_cleanup_interval(10);

        // Create base snapshot to establish watermark
        manager.register_active(TxId::new(999));
        manager.capture_snapshot(500, TxId::new(999));

        // Commit transactions - cleanup triggers every 10
        for i in 0..25 {
            let tx_id = TxId::new(i);
            manager.register_active(tx_id);
            manager.capture_snapshot(100 + i as i64, tx_id);
            manager.register_commit(tx_id, 100 + i as i64);
        }

        // Cleanup should have triggered, reducing committed count
        let (_, committed, _) = manager.stats();
        assert!(committed < 25);
    }

    #[test]
    fn test_stats_method() {
        let manager = TxVisibilityManager::new();

        // Initially empty
        let (active, committed, snapshots) = manager.stats();
        assert_eq!(active, 0);
        assert_eq!(committed, 0);
        assert_eq!(snapshots, 0);

        // Add active transaction with snapshot
        manager.register_active(TxId::new(1));
        manager.capture_snapshot(100, TxId::new(1));

        let (active, committed, snapshots) = manager.stats();
        assert_eq!(active, 1);
        assert_eq!(committed, 0);
        assert_eq!(snapshots, 1);

        // Commit another transaction (with timestamp >= watermark to avoid cleanup)
        manager.register_active(TxId::new(2));
        manager.capture_snapshot(110, TxId::new(2));
        manager.register_commit(TxId::new(2), 150); // >= watermark (100)

        let (active, committed, snapshots) = manager.stats();
        assert_eq!(active, 1); // tx1 still active
        assert_eq!(committed, 1); // tx2 committed
        assert_eq!(snapshots, 1); // tx1 snapshot still active, tx2 snapshot unregistered
    }

    #[test]
    fn test_watermark_calculation() {
        let manager = TxVisibilityManager::new();

        // No snapshots - no watermark
        assert_eq!(manager.watermark(), None);

        // Add snapshots at different times
        manager.register_active(TxId::new(1));
        manager.capture_snapshot(100, TxId::new(1));

        manager.register_active(TxId::new(2));
        manager.capture_snapshot(200, TxId::new(2));

        manager.register_active(TxId::new(3));
        manager.capture_snapshot(50, TxId::new(3));

        // Watermark should be minimum (50)
        assert_eq!(manager.watermark(), Some(50));

        // Remove minimum snapshot
        manager.register_abort(TxId::new(3));

        // Watermark should now be 100
        assert_eq!(manager.watermark(), Some(100));
    }
}
