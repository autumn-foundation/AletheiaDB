//! Transaction visibility management for Snapshot Isolation.
//!
//! This module implements the visibility rules for Snapshot Isolation (SI),
//! which ensures that each transaction sees a consistent snapshot of the database
//! from the time it started.

use crate::api::transaction::types::TxId;
use crate::core::temporal::Timestamp;
use crate::utils::lock::{MutexExt, RwLockExt};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

/// Snapshot of transaction visibility at a point in time.
///
/// A snapshot captures the set of transactions that were active when the
/// snapshot was taken. This is used to determine which versions are visible
/// to a transaction using Snapshot Isolation.
///
/// # Performance (Issue #221)
///
/// Uses `Arc<HashSet<TxId>>` instead of `HashSet<TxId>` to avoid O(N²) cloning
/// overhead when capturing snapshots. With N concurrent transactions, cloning
/// a HashSet containing N elements on every transaction creation was creating
/// quadratic scaling. Arc allows cheap snapshot captures (just Arc clone).
#[derive(Debug, Clone)]
pub struct TransactionSnapshot {
    /// Timestamp when snapshot was taken
    pub snapshot_timestamp: Timestamp,

    /// Transactions that were active when snapshot was taken
    /// (not yet committed or aborted).
    ///
    /// Wrapped in Arc to enable efficient snapshot capture without full HashSet cloning.
    pub active_transactions: Arc<HashSet<TxId>>,
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

    /// Check if a version is visible using embedded metadata (Issue #238 Phase 2).
    ///
    /// This method eliminates the need to query `TxVisibilityManager.committed` map
    /// by using the commit timestamp embedded directly in the version's metadata.
    ///
    /// This is the key enabler for removing the unbounded `committed` map, as versions
    /// become self-contained for visibility checks.
    ///
    /// # Arguments
    /// * `metadata` - The version's embedded metadata containing TxId and commit timestamp
    ///
    /// # Returns
    /// `true` if the version is visible in this snapshot, `false` otherwise
    ///
    /// # Special Cases
    /// - **TxId(0)**: Always visible for backward compatibility (legacy/migrated data)
    /// - **Uncommitted**: Never visible (`commit_timestamp` is `None`)
    ///
    /// # Example
    /// ```ignore
    /// use gallifreydb::api::transaction::{TransactionSnapshot, TxId};
    /// use gallifreydb::storage::version::VersionMetadata;
    ///
    /// let snapshot = TransactionSnapshot { ... };
    /// let version = NodeVersion { metadata: VersionMetadata::new(tx_id, commit_ts), ... };
    ///
    /// // Direct visibility check without querying TxVisibilityManager
    /// if snapshot.is_visible_from_metadata(&version.metadata) {
    ///     // Version is visible
    /// }
    /// ```
    pub fn is_visible_from_metadata(
        &self,
        metadata: &crate::storage::version::VersionMetadata,
    ) -> bool {
        // Special case: TxId(0) is for pre-existing data (migrations, test fixtures)
        // Always treat it as visible for backward compatibility
        if metadata.created_by_tx.as_u64() == 0 {
            return true;
        }

        // Delegate to existing is_visible logic
        self.is_visible(metadata.created_by_tx, metadata.commit_timestamp)
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
/// This struct uses `lock_or_recover()`, `read_or_recover()`, and `write_or_recover()`
/// for all lock acquisitions. This is safe because the protected data structures
/// (`HashSet<TxId>` and `BTreeMap<TxId, Timestamp>`) have no complex invariants that
/// could be violated by a mid-operation panic:
///
/// - **Worst case**: A transaction ID may be missing from the active/committed sets
/// - **Behavior**: This fails safe by being conservative (treating as uncommitted/not visible)
/// - **No corruption**: Standard library collections remain valid after partial operations
///
/// # Concurrency
///
/// The `committed` map uses an `RwLock` instead of `Mutex` to allow concurrent readers.
/// This eliminates read contention in `is_visible()`, which is called during every
/// read operation (get_node, get_edge). Multiple transactions can check visibility
/// simultaneously, while commits still acquire exclusive write access.
///
/// # Performance Optimization (Issue #221)
///
/// The `active` set uses Arc-wrapping with copy-on-write semantics to avoid O(N²)
/// cloning overhead in `capture_snapshot`. Previously, with N concurrent transactions,
/// each new transaction would clone a HashSet of N elements. Now we only clone the
/// Arc (cheap pointer copy) and use copy-on-write for mutations.
pub struct TxVisibilityManager {
    /// Currently active transactions, wrapped in Arc for efficient snapshot capture.
    ///
    /// Uses copy-on-write: mutations create a new HashSet, update it, and replace the Arc.
    /// Snapshots just clone the Arc (cheap), avoiding full HashSet clones.
    active: Mutex<Arc<HashSet<TxId>>>,

    /// Committed transactions: TxId → commit_timestamp
    ///
    /// # Memory Characteristics
    ///
    /// This map grows with the total number of committed transactions (~24 bytes per entry).
    /// For temporal databases supporting historical queries, this is expected behavior.
    ///
    /// **Memory estimates:**
    /// - 1M transactions: ~24MB
    /// - 10M transactions: ~240MB
    /// - 100M transactions: ~2.4GB
    ///
    /// This metadata is essential for MVCC snapshot isolation and cannot be safely
    /// removed without breaking visibility semantics (see issue #226 discussion).
    ///
    /// **Future optimizations** (tracked in separate issues):
    /// - Epoch-based compression for 10-100x memory savings
    /// - Embedding timestamps in versions (architectural refactor)
    committed: RwLock<BTreeMap<TxId, Timestamp>>,
}

impl TxVisibilityManager {
    /// Create a new visibility manager.
    pub fn new() -> Self {
        TxVisibilityManager {
            active: Mutex::new(Arc::new(HashSet::new())),
            committed: RwLock::new(BTreeMap::new()),
        }
    }

    /// Register a new active transaction.
    ///
    /// This should be called when a transaction begins.
    ///
    /// # Arguments
    /// * `tx_id` - The ID of the transaction being started
    ///
    /// # Implementation (Copy-on-Write)
    ///
    /// Uses copy-on-write to modify the active set: clones the HashSet, modifies it,
    /// and replaces the Arc. This ensures that existing snapshots remain valid while
    /// allowing efficient snapshot capture (just Arc clone, not HashSet clone).
    ///
    /// # Note
    ///
    /// Uses lock recovery to prevent cascade panics if the lock was poisoned
    /// by a panicking thread. The transaction set can safely be used after recovery.
    pub fn register_active(&self, tx_id: TxId) {
        let mut active_guard = self.active.lock_or_recover();

        // Use Arc::make_mut for idiomatic copy-on-write.
        // This avoids a clone if the Arc is not shared (only one strong reference).
        Arc::make_mut(&mut *active_guard).insert(tx_id);
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
    ///
    /// # Performance (Issue #221 Fix)
    ///
    /// Previously cloned the entire HashSet of active transactions, creating O(N²)
    /// overhead with N concurrent transactions. Now clones only the Arc (cheap pointer
    /// copy), reducing snapshot capture from O(N) to O(1).
    pub fn capture_snapshot(&self, snapshot_timestamp: Timestamp) -> TransactionSnapshot {
        let active_guard = self.active.lock_or_recover();

        TransactionSnapshot {
            snapshot_timestamp,
            active_transactions: Arc::clone(&active_guard),
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
    ///
    /// # Implementation (Copy-on-Write)
    ///
    /// Uses copy-on-write to remove from active set: clones HashSet, removes tx_id,
    /// wraps in new Arc. This ensures existing snapshots remain valid.
    pub fn register_commit(&self, tx_id: TxId, commit_timestamp: Timestamp) {
        // Drop active lock before acquiring committed write lock to reduce contention
        {
            let mut active_guard = self.active.lock_or_recover();

            // Use Arc::make_mut for idiomatic copy-on-write.
            Arc::make_mut(&mut *active_guard).remove(&tx_id);
        } // active lock released here

        let mut committed = self.committed.write_or_recover();
        committed.insert(tx_id, commit_timestamp);
    }

    /// Register a transaction abort.
    ///
    /// This should be called when a transaction aborts (rolls back).
    /// It removes the transaction from the active set.
    ///
    /// # Arguments
    /// * `tx_id` - The ID of the aborting transaction
    ///
    /// # Implementation (Copy-on-Write)
    ///
    /// Uses copy-on-write to remove from active set: clones HashSet, removes tx_id,
    /// wraps in new Arc. This ensures existing snapshots remain valid.
    pub fn register_abort(&self, tx_id: TxId) {
        let mut active_guard = self.active.lock_or_recover();

        // Use Arc::make_mut for idiomatic copy-on-write.
        Arc::make_mut(&mut *active_guard).remove(&tx_id);
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
        let committed = self.committed.read_or_recover();

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
        let active_guard = self.active.lock_or_recover();
        active_guard.len()
    }

    /// Get the number of committed transactions tracked.
    ///
    /// This is primarily useful for testing and monitoring.
    #[allow(dead_code)]
    pub fn committed_count(&self) -> usize {
        let committed = self.committed.read_or_recover();
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
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };

        // Version committed before snapshot - visible
        assert!(snapshot.is_visible(TxId::new(1), Some(50.into())));
    }

    #[test]
    fn test_snapshot_visibility_committed_after() {
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };

        // Version committed after snapshot - not visible
        assert!(!snapshot.is_visible(TxId::new(1), Some(150.into())));
    }

    #[test]
    fn test_snapshot_visibility_uncommitted() {
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };

        // Uncommitted version - not visible
        assert!(!snapshot.is_visible(TxId::new(1), None));
    }

    #[test]
    fn test_snapshot_visibility_active_transaction() {
        let mut active = HashSet::new();
        active.insert(TxId::new(1));

        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(active),
        };

        // Version from active transaction - not visible even if committed before snapshot
        assert!(!snapshot.is_visible(TxId::new(1), Some(50.into())));
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

        let snapshot = manager.capture_snapshot(100.into());

        assert_eq!(snapshot.snapshot_timestamp, 100.into());
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

        manager.register_commit(TxId::new(1), 100.into());

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
        manager.register_commit(TxId::new(1), 50.into());

        // Take snapshot after commit
        let snapshot = manager.capture_snapshot(100.into());

        // Version from tx1 should be visible
        assert!(manager.is_visible(&snapshot, TxId::new(1)));
    }

    #[test]
    fn test_is_visible_uncommitted_transaction() {
        let manager = TxVisibilityManager::new();

        // Start transaction 1 but don't commit
        manager.register_active(TxId::new(1));

        let snapshot = manager.capture_snapshot(100.into());

        // Version from uncommitted tx1 should not be visible
        assert!(!manager.is_visible(&snapshot, TxId::new(1)));
    }

    #[test]
    fn test_is_visible_concurrent_transaction() {
        let manager = TxVisibilityManager::new();

        // Start tx1
        manager.register_active(TxId::new(1));

        // Take snapshot (tx1 is active)
        let snapshot = manager.capture_snapshot(100.into());

        // Commit tx1 after snapshot
        manager.register_commit(TxId::new(1), 90.into());

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
        let snapshot1 = manager.capture_snapshot(100.into());
        assert_eq!(snapshot1.active_transactions.len(), 1);

        // Commit tx1, start tx2
        manager.register_commit(TxId::new(1), 110.into());
        manager.register_active(TxId::new(2));

        // Snapshot 2 - sees tx2 as active, tx1 committed
        let snapshot2 = manager.capture_snapshot(120.into());
        assert_eq!(snapshot1.active_transactions.len(), 1);
        assert!(snapshot2.active_transactions.contains(&TxId::new(2)));

        // Original snapshot1 unchanged
        assert_eq!(snapshot1.active_transactions.len(), 1);
        assert!(snapshot1.active_transactions.contains(&TxId::new(1)));
    }

    #[test]
    fn test_count_methods() {
        let manager = TxVisibilityManager::new();

        // Initially empty
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.committed_count(), 0);

        // Add active transactions
        manager.register_active(TxId::new(1));
        manager.register_active(TxId::new(2));
        assert_eq!(manager.active_count(), 2);
        assert_eq!(manager.committed_count(), 0);

        // Commit one
        manager.register_commit(TxId::new(1), 100.into());
        assert_eq!(manager.active_count(), 1);
        assert_eq!(manager.committed_count(), 1);

        // Abort the other
        manager.register_abort(TxId::new(2));
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.committed_count(), 1);
    }

    #[test]
    fn test_concurrent_visibility_checks() {
        use std::sync::Arc;
        use std::thread;

        let manager = Arc::new(TxVisibilityManager::new());

        // Setup: commit several transactions
        for i in 1..=10 {
            manager.register_active(TxId::new(i));
            manager.register_commit(TxId::new(i), ((i * 10) as i64).into());
        }

        // Take snapshot after all commits (timestamp 100 is the last commit)
        let snapshot = manager.capture_snapshot(101.into());

        // Spawn multiple threads doing concurrent visibility checks
        // This test demonstrates that RwLock allows concurrent readers
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let mgr = Arc::clone(&manager);
                let snap = snapshot.clone();
                thread::spawn(move || {
                    // Each thread performs many visibility checks
                    for _ in 0..1000 {
                        let tx_id = TxId::new((i % 10) + 1);
                        // All these transactions were committed before snapshot time
                        assert!(mgr.is_visible(&snap, tx_id));
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify the manager is still in a valid state
        assert_eq!(manager.committed_count(), 10);
    }

    // ========================================================================
    // RED Phase Tests: Visibility from Embedded Metadata (Issue #238 Phase 2)
    // ========================================================================
    //
    // These tests verify that TransactionSnapshot can check visibility directly
    // from embedded VersionMetadata, eliminating the need to query the
    // TxVisibilityManager.committed map.

    #[test]
    fn test_is_visible_from_metadata_committed_before() {
        use crate::storage::version::VersionMetadata;

        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };

        // Metadata: tx1 committed at timestamp 50
        let metadata = VersionMetadata::new(TxId::new(1), 50.into());

        // Committed before snapshot, not active -> visible
        assert!(snapshot.is_visible_from_metadata(&metadata));
    }

    #[test]
    fn test_is_visible_from_metadata_committed_after() {
        use crate::storage::version::VersionMetadata;

        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };

        // Metadata: tx1 committed at timestamp 150
        let metadata = VersionMetadata::new(TxId::new(1), 150.into());

        // Committed after snapshot -> not visible
        assert!(!snapshot.is_visible_from_metadata(&metadata));
    }

    #[test]
    fn test_is_visible_from_metadata_uncommitted() {
        use crate::storage::version::VersionMetadata;

        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };

        // Metadata: tx1 uncommitted
        let metadata = VersionMetadata::uncommitted(TxId::new(1));

        // Uncommitted -> not visible
        assert!(!snapshot.is_visible_from_metadata(&metadata));
    }

    #[test]
    fn test_is_visible_from_metadata_active_transaction() {
        use crate::storage::version::VersionMetadata;

        let mut active = HashSet::new();
        active.insert(TxId::new(1));

        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(active),
        };

        // Metadata: tx1 committed at 50 (before snapshot)
        let metadata = VersionMetadata::new(TxId::new(1), 50.into());

        // Even though committed before snapshot, tx1 was active at snapshot time -> not visible
        assert!(!snapshot.is_visible_from_metadata(&metadata));
    }

    #[test]
    fn test_is_visible_from_metadata_committed_at_snapshot() {
        use crate::storage::version::VersionMetadata;

        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };

        // Metadata: tx1 committed at exactly snapshot timestamp
        let metadata = VersionMetadata::new(TxId::new(1), 100.into());

        // Committed at same time as snapshot -> not visible (strict <, not <=)
        assert!(!snapshot.is_visible_from_metadata(&metadata));
    }

    #[test]
    fn test_is_visible_from_metadata_txid_zero() {
        use crate::storage::version::VersionMetadata;

        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };

        // Metadata with TxId(0) - migration/legacy data
        let metadata = VersionMetadata::default_for_existing();

        // TxId(0) should be visible for backward compatibility
        assert!(snapshot.is_visible_from_metadata(&metadata));
    }

    #[test]
    fn test_is_visible_from_metadata_matches_is_visible() {
        use crate::storage::version::VersionMetadata;

        // Test that is_visible_from_metadata produces the same results as is_visible
        let mut active = HashSet::new();
        active.insert(TxId::new(2));

        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(active),
        };

        // Test various scenarios
        let test_cases = vec![
            (TxId::new(1), Some(50.into())),  // committed before, not active
            (TxId::new(1), Some(150.into())), // committed after
            (TxId::new(1), None),             // uncommitted
            (TxId::new(2), Some(50.into())),  // active transaction
            (TxId::new(0), Some(0.into())),   // TxId(0) special case
        ];

        for (tx_id, commit_ts) in test_cases {
            let via_is_visible = snapshot.is_visible(tx_id, commit_ts);

            let metadata = match commit_ts {
                Some(ts) => VersionMetadata::new(tx_id, ts),
                None => VersionMetadata::uncommitted(tx_id),
            };

            let via_metadata = snapshot.is_visible_from_metadata(&metadata);

            assert_eq!(
                via_is_visible, via_metadata,
                "Mismatch for tx_id={:?}, commit_ts={:?}",
                tx_id, commit_ts
            );
        }
    }
}
