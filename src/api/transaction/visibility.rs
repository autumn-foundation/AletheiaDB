//! Transaction visibility management for Snapshot Isolation.
//!
//! This module implements the visibility rules for Snapshot Isolation (SI),
//! which ensures that each transaction sees a consistent snapshot of the database
//! from the time it started.

use crate::api::transaction::types::TxId;
use crate::core::temporal::Timestamp;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

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
}

/// Transaction visibility manager for Snapshot Isolation.
///
/// Tracks only the set of currently **active** (in-flight) transactions.
/// Commit timestamps are no longer stored here — they are embedded directly
/// in each `NodeVersion` / `EdgeVersion` (Issue #238, HyPer/TiDB pattern).
///
/// # Why no committed map?
///
/// Previously, a `committed: BTreeMap<TxId, Timestamp>` grew without bound as
/// transactions completed.  After Issue #238 embedded commit timestamps in version
/// structs, visibility checks use those timestamps directly via
/// [`is_visible_with_embedded_ts`], so the map is unnecessary.
///
/// # Concurrency
///
/// The `active` set uses `Arc`-wrapping with copy-on-write semantics (Issue #221):
/// snapshot capture is O(1) (just an `Arc` clone), while mutations clone only when
/// the `Arc` is shared.
pub struct TxVisibilityManager {
    /// Currently active (not yet committed or aborted) transactions.
    ///
    /// Uses copy-on-write: mutations create a new `HashSet`, update it, and replace
    /// the `Arc`.  Snapshots just clone the `Arc`, avoiding full `HashSet` clones.
    active: Mutex<Arc<HashSet<TxId>>>,
}

impl TxVisibilityManager {
    /// Create a new visibility manager with an empty active-transaction set.
    pub fn new() -> Self {
        TxVisibilityManager {
            active: Mutex::new(Arc::new(HashSet::new())),
        }
    }

    /// Register a new active transaction.  Call when a transaction begins.
    pub fn register_active(&self, tx_id: TxId) {
        let mut guard = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::make_mut(&mut *guard).insert(tx_id);
    }

    /// Capture a snapshot for a transaction (O(1) — just an Arc clone).
    pub fn capture_snapshot(&self, snapshot_timestamp: Timestamp) -> TransactionSnapshot {
        let guard = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        TransactionSnapshot {
            snapshot_timestamp,
            active_transactions: Arc::clone(&guard),
        }
    }

    /// Register a transaction commit.
    ///
    /// Removes the transaction from the active set.  The commit timestamp is no
    /// longer stored here — it is embedded in each version struct (Issue #238).
    pub fn register_commit(&self, tx_id: TxId) {
        let mut guard = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::make_mut(&mut *guard).remove(&tx_id);
    }

    /// Register a transaction abort.  Removes the transaction from the active set.
    pub fn register_abort(&self, tx_id: TxId) {
        let mut guard = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::make_mut(&mut *guard).remove(&tx_id);
    }

    /// Check version visibility using the commit timestamp embedded in the version
    /// (HyPer/TiDB pattern, Issue #238).
    ///
    /// No lock acquisition, no map lookup — the check reduces to a single comparison
    /// of the embedded `commit_timestamp` against the snapshot timestamp.
    ///
    /// # Arguments
    /// * `snapshot` - The transaction's snapshot
    /// * `created_by_tx` - The transaction that created the version
    /// * `commit_timestamp` - Embedded commit timestamp, or `None` if uncommitted
    pub fn is_visible_with_embedded_ts(
        &self,
        snapshot: &TransactionSnapshot,
        created_by_tx: TxId,
        commit_timestamp: Option<Timestamp>,
    ) -> bool {
        // TxId(0) is pre-existing data (fixtures, migrations) — always visible.
        if created_by_tx.as_u64() == 0 {
            return true;
        }
        snapshot.is_visible(created_by_tx, commit_timestamp)
    }

    /// Number of currently active (in-flight) transactions.  Useful for monitoring.
    pub fn active_count(&self) -> usize {
        let guard = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        guard.len()
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
        assert!(snapshot.is_visible(TxId::new(1), Some(50.into())));
    }

    #[test]
    fn test_snapshot_visibility_committed_after() {
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };
        assert!(!snapshot.is_visible(TxId::new(1), Some(150.into())));
    }

    #[test]
    fn test_snapshot_visibility_uncommitted() {
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Arc::new(HashSet::new()),
        };
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
        assert!(!snapshot.is_visible(TxId::new(1), Some(50.into())));
    }

    #[test]
    fn test_visibility_manager_creation() {
        let manager = TxVisibilityManager::new();
        assert_eq!(manager.active_count(), 0);
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
        manager.register_commit(TxId::new(1));
        assert_eq!(manager.active_count(), 0);
    }

    #[test]
    fn test_register_abort() {
        let manager = TxVisibilityManager::new();
        manager.register_active(TxId::new(1));
        assert_eq!(manager.active_count(), 1);
        manager.register_abort(TxId::new(1));
        assert_eq!(manager.active_count(), 0);
    }

    #[test]
    fn test_concurrent_snapshots() {
        let manager = TxVisibilityManager::new();
        manager.register_active(TxId::new(1));
        let snapshot1 = manager.capture_snapshot(100.into());
        assert_eq!(snapshot1.active_transactions.len(), 1);

        manager.register_commit(TxId::new(1));
        manager.register_active(TxId::new(2));
        let snapshot2 = manager.capture_snapshot(120.into());

        assert!(snapshot2.active_transactions.contains(&TxId::new(2)));
        assert_eq!(snapshot1.active_transactions.len(), 1);
        assert!(snapshot1.active_transactions.contains(&TxId::new(1)));
    }

    #[test]
    fn test_count_methods() {
        let manager = TxVisibilityManager::new();
        assert_eq!(manager.active_count(), 0);

        manager.register_active(TxId::new(1));
        manager.register_active(TxId::new(2));
        assert_eq!(manager.active_count(), 2);

        manager.register_commit(TxId::new(1));
        assert_eq!(manager.active_count(), 1);

        manager.register_abort(TxId::new(2));
        assert_eq!(manager.active_count(), 0);
    }

    #[test]
    fn test_concurrent_visibility_checks() {
        use std::sync::Arc;
        use std::thread;

        let manager = Arc::new(TxVisibilityManager::new());
        for i in 1..=10u64 {
            manager.register_active(TxId::new(i));
            manager.register_commit(TxId::new(i));
        }
        let snapshot = manager.capture_snapshot(101.into());

        let handles: Vec<_> = (0..10u64)
            .map(|i| {
                let mgr = Arc::clone(&manager);
                let snap = snapshot.clone();
                thread::spawn(move || {
                    for _ in 0..1000 {
                        let tx_id = TxId::new((i % 10) + 1);
                        let commit_ts: Option<crate::core::temporal::Timestamp> =
                            Some(((i * 10 + 1) as i64).into());
                        let _ = mgr.is_visible_with_embedded_ts(&snap, tx_id, commit_ts);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
    }
}

// ============================================================================
// RED PHASE: Tests for embedded-timestamp visibility (Issue #238)
// HyPer/TiDB approach: visibility checks use commit_timestamp embedded in
// the version struct, bypassing the TxVisibilityManager::committed map.
// ============================================================================
#[cfg(test)]
mod embedded_timestamp_visibility_tests {
    use super::*;

    #[test]
    fn test_is_visible_with_embedded_ts_committed_before_snapshot() {
        let manager = TxVisibilityManager::new();
        let snapshot = manager.capture_snapshot(100.into());

        // commit_timestamp < snapshot_timestamp → visible (no committed-map lookup)
        assert!(
            manager.is_visible_with_embedded_ts(&snapshot, TxId::new(1), Some(50.into())),
            "Version committed before snapshot should be visible via embedded timestamp"
        );
    }

    #[test]
    fn test_is_visible_with_embedded_ts_committed_after_snapshot() {
        let manager = TxVisibilityManager::new();
        let snapshot = manager.capture_snapshot(100.into());

        // commit_timestamp >= snapshot_timestamp → not visible
        assert!(
            !manager.is_visible_with_embedded_ts(&snapshot, TxId::new(1), Some(150.into())),
            "Version committed after snapshot should not be visible"
        );
    }

    #[test]
    fn test_is_visible_with_embedded_ts_concurrent_transaction() {
        let manager = TxVisibilityManager::new();
        manager.register_active(TxId::new(1));

        // Snapshot while tx1 is still active
        let snapshot = manager.capture_snapshot(100.into());
        manager.register_commit(TxId::new(1));

        // Even though commit_ts < snapshot_ts, tx1 was active at snapshot → not visible
        assert!(
            !manager.is_visible_with_embedded_ts(&snapshot, TxId::new(1), Some(90.into())),
            "Version from concurrent transaction should not be visible"
        );
    }

    #[test]
    fn test_is_visible_with_embedded_ts_tx_zero_always_visible() {
        let manager = TxVisibilityManager::new();
        let snapshot = manager.capture_snapshot(100.into());

        // TxId(0) is reserved for pre-existing data and is always visible
        assert!(
            manager.is_visible_with_embedded_ts(&snapshot, TxId::new(0), Some(0.into())),
            "TxId(0) pre-existing data must always be visible"
        );
    }

    #[test]
    fn test_is_visible_with_embedded_ts_matches_map_based_check() {
        // Both methods must agree: embedded-ts path and committed-map path
        let manager = TxVisibilityManager::new();
        manager.register_active(TxId::new(1));
        manager.register_commit(TxId::new(1));

        let snapshot = manager.capture_snapshot(100.into());

        // Direct snapshot check (the underlying primitive) and embedded-ts path must agree.
        let direct_result = snapshot.is_visible(TxId::new(1), Some(50.into()));
        let embedded_result =
            manager.is_visible_with_embedded_ts(&snapshot, TxId::new(1), Some(50.into()));

        assert_eq!(
            direct_result, embedded_result,
            "Embedded-ts visibility must match direct snapshot visibility check"
        );
    }

    #[test]
    fn test_is_visible_with_embedded_ts_no_committed_map_required() {
        // The key property: is_visible_with_embedded_ts works even when
        // the transaction is NOT registered in the committed map.
        // This demonstrates the architecture: versions are self-describing.
        let manager = TxVisibilityManager::new();
        let snapshot = manager.capture_snapshot(100.into());

        // TxId(42) never registered — committed map has no entry for it.
        // But with an embedded commit_timestamp, visibility is still deterministic.
        let commit_ts: Option<Timestamp> = Some(50.into());
        assert!(
            manager.is_visible_with_embedded_ts(&snapshot, TxId::new(42), commit_ts),
            "Embedded-ts check must work without a committed-map entry"
        );
    }
}
