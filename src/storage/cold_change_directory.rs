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
//! - The directory is **never authoritative** in two senses. First, it must be **successfully
//!   seeded** from the cold tier before it may serve any query: an as-yet-unseeded directory, or
//!   one whose seed scan *failed* over a populated cold store, returns `None` from
//!   [`ColdChangeDirectory::eligible_candidates`] so the query **degrades to the full cold scan**
//!   — it never serves an empty-but-untrusted set as if it were a complete answer (which would
//!   silently drop every cold row). A failed seed latches a permanent `Degraded` state for the
//!   process lifetime rather than thrashing a re-scan of a broken store on every query. Second,
//!   even once seeded, a memory budget may evict its oldest entries; an eligibility watermark then
//!   forces any query whose window could touch an evicted region to degrade to the full cold scan.
//!   Correctness never depends on the directory being complete — or even present.
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

/// Whether the directory has been authoritatively seeded from the cold tier.
///
/// This is distinct from [`ColdDirInner::complete`] (the *budget* flag). `complete` answers "has
/// nothing been evicted?"; `DirectoryState` answers the prior question "may the directory be
/// trusted to speak for the cold tier at all?". A directory that has never been seeded, or whose
/// seed scan failed, must **not** serve queries even though its (empty) entry set is trivially
/// `complete` — otherwise a failed seed over a populated cold store would answer every query with
/// an empty set, silently dropping all cold rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DirectoryState {
    /// No successful seed has happened yet. Every query degrades to the full cold scan.
    Unseeded,
    /// A full seed scan completed successfully; the directory is authoritative (subject only to
    /// the budget watermark below).
    Seeded,
    /// A seed scan failed (cold I/O / decode error) over the cold tier. The directory can never be
    /// trusted for the remainder of the process lifetime, so every query permanently degrades to
    /// the (always-correct) full cold scan. Latched — never re-attempted.
    Degraded,
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
    /// Whether the directory has been authoritatively seeded (see [`DirectoryState`]). Gates
    /// eligibility independently of `complete`.
    state: DirectoryState,
}

impl ColdChangeDirectory {
    /// Create an empty directory retaining at most `max_entries` cursors (`0` disables it).
    pub(crate) fn new(max_entries: usize) -> Self {
        Self {
            inner: RwLock::new(ColdDirInner {
                entries: BTreeSet::new(),
                complete: true,
                coverage_watermark: None,
                // Starts NOT seeded: a directory that has not yet scanned the cold tier must not be
                // trusted, so `eligible_candidates` degrades to the full scan until a successful
                // seed (`mark_seeded`) makes it authoritative.
                state: DirectoryState::Unseeded,
            }),
            max_entries,
        }
    }

    /// Mark the directory as authoritatively seeded after a **successful** full seed scan of the
    /// cold tier. Only after this does [`eligible_candidates`](Self::eligible_candidates) serve
    /// queries (a successfully-scanned *empty* cold store counts as seeded, so a fresh empty
    /// database stays on the fast path). A directory already latched `Degraded` stays degraded.
    pub(crate) fn mark_seeded(&self) {
        let mut inner = self.inner.write();
        if inner.state != DirectoryState::Degraded {
            inner.state = DirectoryState::Seeded;
        }
    }

    /// Latch the directory permanently `Degraded` after a **failed** seed scan (cold I/O / decode
    /// error). Every subsequent query degrades to the full cold scan for the process lifetime; the
    /// seed is never re-attempted (no thrash on a broken store).
    pub(crate) fn mark_degraded(&self) {
        self.inner.write().state = DirectoryState::Degraded;
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
    /// strict `> cursor` pagination anchor (already validated by `list_changes`). `bound` is the
    /// per-tier limit and `has_filter` is `true` iff a valid-time window and/or label filter is
    /// active for this query.
    ///
    /// Eligibility: the directory may serve the window iff (a) it is enabled (`max_entries > 0`),
    /// (b) it has been authoritatively **seeded** ([`DirectoryState::Seeded`] — an unseeded or
    /// permanently-degraded directory always degrades, so a failed/absent seed can never serve an
    /// empty set as a complete answer), and (c) either it is `complete` or its `coverage_watermark`
    /// is strictly below the query's effective scan lower bound (so no evicted cursor can fall
    /// inside the window). Otherwise it returns `None` and the caller falls back to the sound full
    /// scan.
    ///
    /// Bound-aware allocation: when no valid-window/label filter is active (`has_filter == false`),
    /// every in-window candidate survives `consider_version`, so the `bound`-smallest survivors are
    /// exactly the ascending prefix — the collected candidate `Vec` is capped at `bound`, restoring
    /// `O(bound)` allocation for the common unfiltered windowed query. When a filter is active
    /// survival can't be predicted, so the full in-window range is collected (the point-read walk
    /// still early-stops after `bound` survivors). This changes neither output: the point-read path
    /// already retains only the `bound`-smallest.
    pub(crate) fn eligible_candidates(
        &self,
        tx_window: &TimeRange,
        resume_after: Option<ChangeCursor>,
        bound: usize,
        has_filter: bool,
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

        // Authority guard: an unseeded or permanently-degraded directory must never be trusted —
        // degrade to the (always-correct) full cold scan regardless of `complete`.
        if inner.state != DirectoryState::Seeded {
            return None;
        }

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
        let range = inner
            .entries
            .range((lo_bound, Bound::Excluded(hi)))
            .copied();
        // Unfiltered windowed query: only the `bound`-smallest can reach the page, so cap the
        // candidate allocation at the ascending prefix. A filtered query keeps the whole range.
        let candidates: Vec<ChangeCursor> = if !has_filter && bound != usize::MAX {
            range.take(bound).collect()
        } else {
            range.collect()
        };
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

    /// Test-only: whether the directory has been authoritatively seeded (may serve queries).
    #[cfg(test)]
    pub(crate) fn is_seeded(&self) -> bool {
        self.inner.read().state == DirectoryState::Seeded
    }

    /// Test-only: whether the directory has latched the permanent `Degraded` state.
    #[cfg(test)]
    pub(crate) fn is_degraded(&self) -> bool {
        self.inner.read().state == DirectoryState::Degraded
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
    fn unseeded_directory_degrades_even_when_complete() {
        // A never-seeded directory is trivially `complete` (empty), but must still degrade — a
        // failed/absent seed must never serve an empty set as if it were a complete answer.
        let dir = ColdChangeDirectory::new(1000);
        assert!(dir.is_complete());
        assert!(!dir.is_seeded());
        assert!(
            dir.eligible_candidates(&tr(0, 1_000_000), None, usize::MAX, false)
                .is_none(),
            "unseeded directory must degrade to scan"
        );
    }

    #[test]
    fn degraded_directory_never_serves() {
        let dir = ColdChangeDirectory::new(1000);
        dir.mark_degraded();
        // A subsequent successful seed can never un-degrade it.
        dir.mark_seeded();
        assert!(dir.is_degraded());
        assert!(
            dir.eligible_candidates(&tr(0, 1_000_000), None, usize::MAX, false)
                .is_none()
        );
    }

    #[test]
    fn empty_directory_seeded_serves_empty() {
        let dir = ColdChangeDirectory::new(1000);
        dir.mark_seeded();
        assert!(dir.is_complete());
        let got = dir
            .eligible_candidates(&tr(0, 1_000_000), None, usize::MAX, false)
            .unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn ranges_half_open_window() {
        let dir = ColdChangeDirectory::new(1000);
        for i in 1..=10u64 {
            dir.insert(node_cursor((i as i64) * 100, i));
        }
        dir.mark_seeded();
        // Window [300, 700): ticks 300,400,500,600 -> ids 3,4,5,6.
        let got = dir
            .eligible_candidates(&tr(300, 700), None, usize::MAX, false)
            .unwrap();
        let ids: Vec<u64> = got.iter().map(|c| c.version_id).collect();
        assert_eq!(ids, vec![3, 4, 5, 6]);
    }

    #[test]
    fn resume_after_is_exclusive() {
        let dir = ColdChangeDirectory::new(1000);
        for i in 1..=10u64 {
            dir.insert(node_cursor((i as i64) * 100, i));
        }
        dir.mark_seeded();
        let resume = node_cursor(400, 4);
        let got = dir
            .eligible_candidates(&tr(0, 1000), Some(resume), usize::MAX, false)
            .unwrap();
        let ids: Vec<u64> = got.iter().map(|c| c.version_id).collect();
        assert_eq!(
            ids,
            vec![5, 6, 7, 8, 9],
            "strictly greater than the resume anchor"
        );
    }

    #[test]
    fn bound_caps_unfiltered_candidate_prefix() {
        let dir = ColdChangeDirectory::new(1000);
        for i in 1..=10u64 {
            dir.insert(node_cursor((i as i64) * 100, i));
        }
        dir.mark_seeded();
        // Unfiltered, bound 3 over window [0, 1100): only the ascending prefix 1,2,3 is collected.
        let got = dir
            .eligible_candidates(&tr(0, 1100), None, 3, false)
            .unwrap();
        let ids: Vec<u64> = got.iter().map(|c| c.version_id).collect();
        assert_eq!(ids, vec![1, 2, 3], "unfiltered bound caps the prefix");
        // Filtered: the full in-window range is collected (survival can't be predicted).
        let got_filtered = dir
            .eligible_candidates(&tr(0, 1100), None, 3, true)
            .unwrap();
        assert_eq!(
            got_filtered.len(),
            10,
            "filtered query keeps the full range"
        );
    }

    #[test]
    fn eviction_sets_watermark_and_degrades_old_window() {
        let dir = ColdChangeDirectory::new(3);
        for i in 1..=10u64 {
            dir.insert(node_cursor((i as i64) * 100, i));
        }
        dir.mark_seeded();
        assert!(!dir.is_complete());
        assert_eq!(dir.len(), 3, "capped to max_entries");
        // Recent window above the watermark is eligible.
        assert!(
            dir.eligible_candidates(&tr(850, 1100), None, usize::MAX, false)
                .is_some()
        );
        // Old window below the watermark degrades.
        assert!(
            dir.eligible_candidates(&tr(0, 500), None, usize::MAX, false)
                .is_none()
        );
    }

    #[test]
    fn disabled_directory_always_degrades() {
        let dir = ColdChangeDirectory::new(0);
        dir.insert(node_cursor(100, 1));
        dir.mark_seeded();
        assert_eq!(dir.len(), 0);
        assert!(
            dir.eligible_candidates(&tr(0, 1000), None, usize::MAX, false)
                .is_none()
        );
    }
}
