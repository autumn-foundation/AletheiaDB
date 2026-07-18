//! In-memory, transaction-time-ordered directory of cold-resident change versions (Issue #3677).
//!
//! # Why
//!
//! `AletheiaDB::list_changes` serves a bi-temporal changefeed window by merging a hot-tier scan
//! with a cold-tier scan. The cold tier (redb) is keyed by `version_id`, which is **not**
//! transaction-time ordered (a version's id is allocated at transaction *build* time while its
//! commit timestamp is assigned later under the commit-clock lock — two concurrent commits can
//! invert the two orders). So the naive fix — `range()` the ascending `version_id` key and
//! early-stop — is unsound: it would silently drop or misorder rows.
//!
//! This directory is the sound alternative (Approach B): an in-memory `BTreeSet<ChangeCursor>`
//! that mirrors the *membership* of the cold tier, ordered by the changefeed's real
//! [`ChangeCursor`] (transaction-time first). A windowed `list_changes` then ranges the set over
//! the query's `[start, end)` transaction-time window and point-reads only the versions that name
//! candidate cold rows, decoding `O(window)` versions instead of the whole cold store.
//!
//! # Correctness
//!
//! - Each entry is *just* a `ChangeCursor` (≈5 machine words) — a self-sufficient pointer that
//!   already carries `kind_ord` + `version_id`, enough to point-read the exact cold row. The
//!   materialization decodes the selected rows through the **same** `consider_version` the
//!   full-scan path uses, so parity is byte-identical by construction.
//! - The directory is **never authoritative**. Under a memory budget it may evict its oldest
//!   entries; an eligibility watermark then forces any query whose window could touch an evicted
//!   region to **degrade to the full cold scan**. Correctness never depends on the directory being
//!   complete.
//! - Its `RwLock` is a **leaf**: it is only ever acquired after the `historical` lock is released,
//!   and it never calls back into `historical` / `wal` / `current_timestamp` (see the CLAUDE.md
//!   lock order). No redb I/O is performed while it is held.

use crate::core::changefeed::ChangeCursor;
use crate::core::temporal::TimeRange;
use parking_lot::RwLock;
use std::collections::BTreeSet;
use std::ops::Bound;

/// In-memory, `ChangeCursor`-ordered directory of the cold tier's change-version membership.
pub(crate) struct ColdChangeDirectory {
    inner: RwLock<ColdDirInner>,
    /// Maximum retained entries. `0` disables the directory entirely (every query degrades to the
    /// full cold scan).
    max_entries: usize,
}

struct ColdDirInner {
    /// Transaction-time-ordered membership of cold-resident versions.
    entries: BTreeSet<ChangeCursor>,
    /// `false` once anything has been evicted for budget — the directory is then only a partial
    /// (newest-region) view, guarded by `coverage_watermark`.
    complete: bool,
    /// The largest cursor ever evicted for budget. A query whose scan lower bound is at or below
    /// this watermark could be missing an evicted in-window row, so it must degrade to a scan.
    coverage_watermark: Option<ChangeCursor>,
}

impl ColdChangeDirectory {
    /// Create an empty directory retaining at most `max_entries` cursors (`0` disables it).
    pub(crate) fn new(max_entries: usize) -> Self {
        Self {
            inner: RwLock::new(ColdDirInner {
                entries: BTreeSet::new(),
                complete: true,
                coverage_watermark: None,
            }),
            max_entries,
        }
    }

    /// Insert a single cold-resident version's cursor.
    #[cfg(test)]
    pub(crate) fn insert(&self, cursor: ChangeCursor) {
        if self.max_entries == 0 {
            return;
        }
        let mut inner = self.inner.write();
        inner.entries.insert(cursor);
        inner.enforce_cap(self.max_entries);
    }

    /// Insert many cold-resident versions' cursors, enforcing the cap once at the end.
    pub(crate) fn insert_many(&self, cursors: impl IntoIterator<Item = ChangeCursor>) {
        if self.max_entries == 0 {
            return;
        }
        let mut inner = self.inner.write();
        for cursor in cursors {
            inner.entries.insert(cursor);
        }
        inner.enforce_cap(self.max_entries);
    }

    /// Return the in-window candidate cursors (ascending) the caller may point-read, or `None` to
    /// signal the query must **degrade to the full cold scan**.
    ///
    /// `tx_window` is the half-open `[start, end)` transaction-time window; `resume_after` is the
    /// strict `> cursor` pagination anchor (already validated by `list_changes`).
    ///
    /// Eligibility: the directory may serve the window iff (a) it is enabled (`max_entries > 0`)
    /// and (b) either it is `complete` or its `coverage_watermark` is strictly below the query's
    /// effective scan lower bound (so no evicted cursor can fall inside the window). Otherwise it
    /// returns `None` and the caller falls back to the sound full scan.
    pub(crate) fn eligible_candidates(
        &self,
        tx_window: &TimeRange,
        resume_after: Option<ChangeCursor>,
    ) -> Option<Vec<ChangeCursor>> {
        if self.max_entries == 0 {
            return None;
        }

        // Half-open window bounds as sentinel cursors: `min_at(start)` (inclusive lower) and
        // `min_at(end)` (exclusive upper) exactly select versions whose commit lies in
        // `[start, end)`, mirroring `TimeRange::contains`.
        let start_cursor = ChangeCursor::min_at(tx_window.start());
        let hi = ChangeCursor::min_at(tx_window.end());

        // Effective lower bound of the scan: the max of the window start and (exclusively) the
        // resume anchor. This is the smallest cursor the directory could be asked to serve.
        let lo = match resume_after {
            Some(r) if r > start_cursor => r,
            _ => start_cursor,
        };

        let inner = self.inner.read();

        // Partial-coverage guard: if anything was evicted and the largest evicted cursor is at or
        // above the query's lower bound, an in-window row might be missing → degrade to scan.
        if !inner.complete
            && let Some(watermark) = inner.coverage_watermark
            && watermark >= lo
        {
            return None;
        }

        // Retained set fully covers the window → collect the in-range cursors (ascending). The
        // lower bound is exclusive of `resume_after` when it dominates the window start (strict
        // `> cursor` resume), else inclusive of the window start.
        let lo_bound = match resume_after {
            Some(r) if r > start_cursor => Bound::Excluded(r),
            _ => Bound::Included(start_cursor),
        };
        let candidates: Vec<ChangeCursor> = inner
            .entries
            .range((lo_bound, Bound::Excluded(hi)))
            .copied()
            .collect();
        Some(candidates)
    }

    /// Test-only: number of retained cursors.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner.read().entries.len()
    }

    /// Test-only: whether the directory still holds every cold cursor (nothing evicted).
    #[cfg(test)]
    pub(crate) fn is_complete(&self) -> bool {
        self.inner.read().complete
    }
}

impl ColdDirInner {
    /// Evict the smallest (oldest tx-time) cursors until within `max_entries`, advancing the
    /// coverage watermark and clearing the completeness flag.
    fn enforce_cap(&mut self, max_entries: usize) {
        while self.entries.len() > max_entries {
            let Some(evicted) = self.entries.pop_first() else {
                break;
            };
            self.complete = false;
            self.coverage_watermark = Some(match self.coverage_watermark {
                Some(w) => w.max(evicted),
                None => evicted,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::changefeed::EntityKind;
    use crate::core::temporal::Timestamp;

    fn tr(from: i64, to: i64) -> TimeRange {
        TimeRange::new(Timestamp::from(from), Timestamp::from(to)).unwrap()
    }

    fn node_cursor(tx: i64, id: u64) -> ChangeCursor {
        ChangeCursor::for_version(Timestamp::from(tx), EntityKind::Node, id, id)
    }

    #[test]
    fn empty_directory_is_complete_and_serves_empty() {
        let dir = ColdChangeDirectory::new(1000);
        assert!(dir.is_complete());
        let got = dir.eligible_candidates(&tr(0, 1_000_000), None).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn ranges_half_open_window() {
        let dir = ColdChangeDirectory::new(1000);
        for i in 1..=10u64 {
            dir.insert(node_cursor((i as i64) * 100, i));
        }
        // Window [300, 700): ticks 300,400,500,600 -> ids 3,4,5,6.
        let got = dir.eligible_candidates(&tr(300, 700), None).unwrap();
        let ids: Vec<u64> = got.iter().map(|c| c.version_id).collect();
        assert_eq!(ids, vec![3, 4, 5, 6]);
    }

    #[test]
    fn resume_after_is_exclusive() {
        let dir = ColdChangeDirectory::new(1000);
        for i in 1..=10u64 {
            dir.insert(node_cursor((i as i64) * 100, i));
        }
        let resume = node_cursor(400, 4);
        let got = dir.eligible_candidates(&tr(0, 1000), Some(resume)).unwrap();
        let ids: Vec<u64> = got.iter().map(|c| c.version_id).collect();
        assert_eq!(
            ids,
            vec![5, 6, 7, 8, 9],
            "strictly greater than the resume anchor"
        );
    }

    #[test]
    fn eviction_sets_watermark_and_degrades_old_window() {
        let dir = ColdChangeDirectory::new(3);
        for i in 1..=10u64 {
            dir.insert(node_cursor((i as i64) * 100, i));
        }
        assert!(!dir.is_complete());
        assert_eq!(dir.len(), 3, "capped to max_entries");
        // Recent window above the watermark is eligible.
        assert!(dir.eligible_candidates(&tr(850, 1100), None).is_some());
        // Old window below the watermark degrades.
        assert!(dir.eligible_candidates(&tr(0, 500), None).is_none());
    }

    #[test]
    fn disabled_directory_always_degrades() {
        let dir = ColdChangeDirectory::new(0);
        dir.insert(node_cursor(100, 1));
        assert_eq!(dir.len(), 0);
        assert!(dir.eligible_candidates(&tr(0, 1000), None).is_none());
    }
}
