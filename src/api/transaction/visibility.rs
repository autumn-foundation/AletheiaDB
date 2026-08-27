//! Transaction visibility management for Snapshot Isolation.
//!
//! This module implements the visibility rules for Snapshot Isolation (SI),
//! which ensures that each transaction sees a consistent snapshot of the database
//! from the time it started.

use crate::api::transaction::types::TxId;
use crate::core::temporal::Timestamp;
use arc_swap::ArcSwap;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
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

    /// Write transactions that were in flight when the snapshot was taken.
    ///
    /// `None` means "none were", which is the overwhelmingly common case on a
    /// read-heavy database -- and it is `None` rather than an empty `Arc` on
    /// purpose. Cloning an `Arc` is an atomic increment on a cache line every
    /// thread shares, and that refcount traffic, not any lock, is what
    /// dominates opening a read transaction. `None` costs a plain atomic load
    /// of a counter and no write at all.
    pub active_transactions: Option<Arc<HashSet<TxId>>>,
}

impl TransactionSnapshot {
    /// Number of write transactions that were in flight at snapshot time.
    pub fn active_len(&self) -> usize {
        self.active_transactions.as_ref().map_or(0, |a| a.len())
    }

    /// Whether `tx_id` was in flight at snapshot time.
    pub fn is_active(&self, tx_id: TxId) -> bool {
        self.active_transactions
            .as_ref()
            .is_some_and(|a| a.contains(&tx_id))
    }

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
                ts < self.snapshot_timestamp && !self.is_active(created_by_tx)
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
/// [`Self::is_visible_with_embedded_ts`], so the map is unnecessary.
///
/// # What goes in the active set
///
/// **Write transactions only.** The set exists to answer one question, asked in
/// [`TransactionSnapshot::is_visible`]: was the transaction that *created* this
/// version still in flight when I took my snapshot? Only a write transaction
/// ever creates a version, so a read transaction's id can never be the
/// `created_by_tx` being tested, and its membership is unobservable.
///
/// Registering reads anyway was pure cost, and it was the dominant cost: reads
/// outnumber writes, so read traffic set the size of the set that every
/// mutation had to copy, and it took the lock twice more per transaction (once
/// to register, once to deregister on drop).
///
/// # Concurrency
///
/// Snapshot capture is a lock-free `ArcSwap` load. Mutations are copy-on-write
/// under a mutex that now only ever serializes *committers* -- off the read path
/// entirely.
pub struct TxVisibilityManager {
    /// Currently active (not yet committed or aborted) **write** transactions.
    ///
    /// Read with no lock; replaced wholesale under `mutate`.
    active: ArcSwap<HashSet<TxId>>,
    /// Number of entries in `active`, so a snapshot can tell "nobody is in
    /// flight" from a plain load instead of cloning the `Arc`.
    ///
    /// Maintained under `mutate` alongside `active`. A snapshot that reads this
    /// as zero a moment before a writer registers is still correct: that
    /// writer has not committed, so its versions carry a commit timestamp
    /// after this snapshot and the timestamp check excludes them anyway.
    active_len: AtomicUsize,
    /// Serializes the read-modify-write of `active`.
    ///
    /// `ArcSwap` gives lock-free reads but no atomic read-modify-write, and the
    /// mutations here are insert/remove on a shared set, so they still need
    /// serializing. Contended only between concurrent write transactions.
    mutate: Mutex<()>,
}

impl TxVisibilityManager {
    /// Create a new visibility manager with an empty active-transaction set.
    pub fn new() -> Self {
        TxVisibilityManager {
            active: ArcSwap::from_pointee(HashSet::new()),
            active_len: AtomicUsize::new(0),
            mutate: Mutex::new(()),
        }
    }

    /// Copy-on-write mutation of the active set.
    fn mutate<F: FnOnce(&mut HashSet<TxId>)>(&self, edit: F) {
        let _serial = self.mutate.lock().unwrap_or_else(PoisonError::into_inner);
        let mut next = HashSet::clone(&self.active.load());
        edit(&mut next);
        // Publish the length first: an observer may then see a non-zero length
        // with the old set, which only costs it a redundant `Arc` clone, never
        // a wrong answer. The reverse order could let one see zero while the
        // new set is already installed.
        self.active_len.store(next.len(), Ordering::Release);
        self.active.store(Arc::new(next));
    }

    /// Register a new active transaction.  Call when a **write** transaction
    /// begins; read transactions deliberately do not register (see the type
    /// docs).
    pub fn register_active(&self, tx_id: TxId) {
        self.mutate(|active| {
            active.insert(tx_id);
        });
    }

    /// Capture a snapshot for a transaction.
    ///
    /// Lock-free: one `ArcSwap` load and an `Arc` clone.
    pub fn capture_snapshot(&self, snapshot_timestamp: Timestamp) -> TransactionSnapshot {
        TransactionSnapshot {
            snapshot_timestamp,
            // Fast path: no write transaction is in flight, so there is nothing
            // to test membership against and no `Arc` to clone.
            active_transactions: if self.active_len.load(Ordering::Acquire) == 0 {
                None
            } else {
                Some(self.active.load_full())
            },
        }
    }

    /// Register a transaction commit.
    ///
    /// Removes the transaction from the active set.  The commit timestamp is no
    /// longer stored here — it is embedded in each version struct (Issue #238).
    pub fn register_commit(&self, tx_id: TxId) {
        self.mutate(|active| {
            active.remove(&tx_id);
        });
    }

    /// Register a transaction abort.  Removes the transaction from the active set.
    pub fn register_abort(&self, tx_id: TxId) {
        self.mutate(|active| {
            active.remove(&tx_id);
        });
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
        self.active_len.load(Ordering::Acquire)
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
            active_transactions: None,
        };
        assert!(snapshot.is_visible(TxId::new(1), Some(50.into())));
    }

    #[test]
    fn test_snapshot_visibility_committed_after() {
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: None,
        };
        assert!(!snapshot.is_visible(TxId::new(1), Some(150.into())));
    }

    #[test]
    fn test_snapshot_visibility_uncommitted() {
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: None,
        };
        assert!(!snapshot.is_visible(TxId::new(1), None));
    }

    #[test]
    fn test_snapshot_visibility_active_transaction() {
        let mut active = HashSet::new();
        active.insert(TxId::new(1));
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: 100.into(),
            active_transactions: Some(Arc::new(active)),
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
        assert_eq!(snapshot.active_len(), 2);
        assert!(snapshot.is_active(TxId::new(1)));
        assert!(snapshot.is_active(TxId::new(2)));
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
        assert_eq!(snapshot1.active_len(), 1);

        manager.register_commit(TxId::new(1));
        manager.register_active(TxId::new(2));
        let snapshot2 = manager.capture_snapshot(120.into());

        assert!(snapshot2.is_active(TxId::new(2)));
        assert_eq!(snapshot1.active_len(), 1);
        assert!(snapshot1.is_active(TxId::new(1)));
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
