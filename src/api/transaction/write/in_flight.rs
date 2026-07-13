//! In-flight (durable-but-not-yet-applied) LSN tracking (lost-write persist race).
//!
//! # Why this exists
//!
//! The commit path makes a write **durable** (WAL fsync + ack) BEFORE it is
//! **applied** to the in-memory current/historical stores, and no single lock
//! spans both points. Meanwhile index persistence stamps the manifest with the
//! WAL *allocation frontier* (the next-to-allocate LSN) and startup replay drops
//! every WAL entry with `lsn < manifest.lsn`. A write that is fsynced (so its
//! `lsn < frontier`) but not yet applied at snapshot time is therefore absent
//! from the persisted snapshot AND dropped by replay — a silently lost,
//! already-acknowledged write.
//!
//! [`InFlightLsns`] records the **lowest LSN of every commit that is durable but
//! not yet fully applied**. Index persistence stamps the manifest with the
//! minimum such in-flight LSN (the *applied watermark*) instead of the
//! allocation frontier, so replay always re-covers any durable-but-unapplied
//! write. Re-applying a write that *did* make it into the snapshot is safe:
//! startup replay is idempotent (keyed by `version_id`).
//!
//! # Lifecycle
//!
//! A commit [`register`](InFlightLsns::register)s its base LSN AFTER the WAL
//! append allocates it but BEFORE the durability fsync (so a durable write is
//! always registered), and holds the returned [`InFlightGuard`] until AFTER the
//! in-memory apply + commit-timestamp finalization complete (so a *deregistered*
//! LSN is guaranteed present in any current+historical snapshot). The guard's
//! `Drop` deregisters, so any early-return / panic path also releases the LSN.
//!
//! # Lock ordering
//!
//! The internal mutex is a **leaf**: it is only ever held for the duration of a
//! single `BTreeSet` insert / remove / min read and is never held while
//! acquiring any other write-path synchronization primitive. In the commit path
//! it is taken under `current_timestamp` (register) and with no lock held
//! (deregister); in the persistence path it is taken while `historical`/
//! `snapshot_lock` are held. Because it is never held across another
//! acquisition, it cannot participate in a deadlock cycle and needs no fixed
//! position in the CLAUDE.md lock order beyond "leaf, acquired last".

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

/// Tracks the base LSNs of commits that are durable but not yet applied.
///
/// Cloneable via `Arc`; shared between the database and every write transaction.
#[derive(Debug, Default)]
pub(crate) struct InFlightLsns {
    /// Multiset is unnecessary: each in-flight commit owns a distinct base LSN
    /// (LSN allocation is monotonic and a commit registers exactly once), so a
    /// plain ordered set suffices and gives O(log n) min via `first()`.
    lsns: Mutex<BTreeSet<u64>>,
}

impl InFlightLsns {
    /// Create an empty tracker.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Register `lsn` as in-flight and return an RAII guard that deregisters it
    /// on drop.
    ///
    /// Must be called AFTER the WAL append allocates `lsn` and BEFORE the
    /// durability fsync, so every durable write is registered. The guard must be
    /// kept alive until AFTER the in-memory apply + finalization complete.
    pub(crate) fn register(self: &Arc<Self>, lsn: u64) -> InFlightGuard {
        // Leaf critical section: a single set insert, no nested lock acquisition.
        if let Ok(mut set) = self.lsns.lock() {
            set.insert(lsn);
        }
        InFlightGuard {
            tracker: Arc::clone(self),
            lsn,
        }
    }

    /// The minimum in-flight (durable-but-unapplied) LSN, or `None` when nothing
    /// is in flight.
    ///
    /// When `None`, the caller falls back to the WAL allocation frontier, which
    /// is safe precisely because no durable-but-unapplied write exists.
    pub(crate) fn min(&self) -> Option<u64> {
        // Leaf critical section: a single ordered-set min read.
        self.lsns
            .lock()
            .ok()
            .and_then(|set| set.iter().next().copied())
    }

    /// Deregister `lsn`. Invoked only by [`InFlightGuard::drop`].
    fn deregister(&self, lsn: u64) {
        if let Ok(mut set) = self.lsns.lock() {
            set.remove(&lsn);
        }
    }
}

/// RAII guard that keeps an LSN registered as in-flight for its lifetime.
///
/// Dropping the guard — on the success path (after apply + finalize) OR on any
/// early-return / panic path — deregisters the LSN. It carries no borrow of the
/// transaction, so it can outlive the `current_timestamp` critical section and
/// survive to the end of the commit function.
#[must_use = "keep the guard alive until after apply + finalize; dropping it deregisters the LSN"]
pub(crate) struct InFlightGuard {
    tracker: Arc<InFlightLsns>,
    lsn: u64,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.tracker.deregister(self.lsn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tracker_has_no_min() {
        let t = InFlightLsns::new();
        assert_eq!(t.min(), None);
    }

    #[test]
    fn register_exposes_min_and_guard_deregisters() {
        let t = Arc::new(InFlightLsns::new());
        {
            let _g1 = t.register(10);
            let _g2 = t.register(5);
            let _g3 = t.register(7);
            assert_eq!(t.min(), Some(5));
            drop(_g2);
            assert_eq!(t.min(), Some(7));
        }
        assert_eq!(t.min(), None);
    }

    #[test]
    fn min_falls_back_to_none_after_all_dropped() {
        let t = Arc::new(InFlightLsns::new());
        let g = t.register(42);
        assert_eq!(t.min(), Some(42));
        drop(g);
        assert_eq!(t.min(), None);
    }
}
