//! The commit clock: HLC frontier with a lock-free snapshot-read path.
//!
//! # What this replaces
//!
//! The commit clock used to be a bare `Mutex<Timestamp>` serving two unrelated
//! jobs at once:
//!
//! 1. **Writer serialization.** A committer holds it from commit-stamp
//!    assignment all the way through WAL append, durability, apply, and
//!    finalize (Issue #3413). That is load-bearing: running the precondition
//!    guards under the same held lock that spans apply is what guarantees a
//!    guard which passed pre-WAL is still valid at apply time, so a transaction
//!    rejected at runtime can never leave a durable frame for crash recovery to
//!    reapply.
//!
//! 2. **Snapshot reservation.** Every `read_transaction()` locked the same
//!    mutex to read the frontier, compute a strictly-greater stamp, and write
//!    it back.
//!
//! Job 2 does not need a mutex, and paying for one made every snapshot read
//! contend on the exact cache line the writers own. This type keeps job 1 on a
//! mutex (unchanged) and makes job 2 lock-free.
//!
//! # Why one atomic is not enough
//!
//! The obvious refactor -- swap the frontier for an atomic and let readers CAS
//! it -- is wrong, and the reason is worth stating because it is not obvious.
//!
//! A committer assigns its stamp `C` at the *start* of the commit and applies
//! its writes at the *end*. Today a reader cannot observe that window because
//! it blocks on the very mutex the committer is holding. Make the reader
//! lock-free against a single frontier and it can now be handed a snapshot
//! `S > C` while `C`'s writes are still not in storage. MVCC visibility is
//! `commit_ts < snapshot_ts`, so the reader would consider `C` visible and then
//! fail to find it -- a snapshot-isolation violation, and exactly the class of
//! bug the held mutex was accidentally preventing.
//!
//! So the allocation frontier carries an explicit **in-flight bit**:
//!
//! - bits 0..96 hold the packed timestamp -- the highest stamp handed out to
//!   anyone, whether assigned to a commit or reserved by a reader;
//! - bit 127 is set from the moment a committer assigns its stamp until the
//!   moment its writes are visible in storage;
//! - bit 126 marks the timestamp as a reader's reservation rather than a commit
//!   stamp, which is what lets a later reader share it instead of installing its
//!   own.
//!
//! Both live in one 128-bit cell, which is the point: a reader's decision
//! ("is a commit in flight?") and its reservation are then a single atomic
//! step, and no interleaving can slip between them. Splitting them across two
//! cells looks equivalent and is not -- whichever order the committer writes
//! them in, there is a window where a reader reads a stale flag against a fresh
//! frontier and reserves straight past an unapplied commit.
//!
//! A separate `applied` frontier is also tracked, but only as an observable:
//! nothing in the read protocol consults it.
//!
//! # The read protocol
//!
//! ```text
//! loop {
//!     let state = frontier.load();
//!     if state.in_flight() {
//!         return state.timestamp();     // park exactly ON the commit stamp:
//!     }                                 // strict < keeps it invisible
//!     if state.reserved() && state.timestamp() >= now {
//!         return state.timestamp();     // share a live reservation: NO write
//!     }
//!     let s = max(now, state.timestamp() + 1 logical);
//!     if frontier.compare_exchange(state, s | RESERVED).is_ok() { return s }
//! }
//! ```
//!
//! The middle branch is what keeps this cheap. Replacing the reader's mutex with
//! a CAS did not help at all on its own -- an atomic read-modify-write on one
//! shared word serializes on that cache line much as a lock does. So most reads
//! must not write. A reservation is already strictly greater than every applied
//! commit and no future commit can land on it, so a later reader can simply
//! return it. Only the first reader after a commit, or the first after the
//! wallclock ticks past the standing reservation, pays for a CAS.
//!
//! Sharing bounds staleness to the resolution of the clock: a shared snapshot is
//! refreshed as soon as `now` moves past it. That matters -- an indefinitely
//! frozen snapshot would keep a fact whose valid time starts later (Issue #3221)
//! from ever coming into view on an idle database.
//!
//! Parking is safe because committers are serialized: if a commit is in flight,
//! every commit ordered before it has already applied, so a snapshot at exactly
//! its stamp sees all of them and not it.
//!
//! Reserving is safe because of the CAS, not the load: a committer that slipped
//! in after the load is caught when the exchange fails, and the reader retries
//! into the parking branch.
//!
//! An earlier version of this type inferred "in flight" from `frontier !=
//! applied` instead of a bit. That is memory-safe and preserves isolation, but
//! it cannot tell a committer apart from another reader's reservation, so a
//! single read would park every subsequent read on one stamp until the next
//! commit came along. Correct, and quietly wrong: with the snapshot frozen, a
//! fact whose valid time starts later (Issue #3221) would never come into view
//! on an idle database. The bit distinguishes the two cases.
//!
//! Reservation itself is load-bearing and predates this type. Without it a
//! commit landing in the same wallclock tick as a read would recompute an
//! identical stamp `S`, closing a superseded version's transaction interval at
//! exactly `[C1, S)`; the half-open upper bound then excludes `S` and the
//! historical fallback misses the version. Reserving `S` sorts every later
//! commit strictly after this read.
//!
//! # Aborts
//!
//! A committer that assigns `C` and then aborts must clear the in-flight bit or
//! every later read would park on `C` forever. [`CommitClockGuard`]'s `Drop`
//! does that, so an abort needs no handling at the call site. The stamp `C`
//! itself is simply burned: the frontier stays there and the next commit sorts
//! after it.

use crate::core::error::{Result, TransactionError};
use crate::core::hlc::HybridTimestamp;
use crate::core::temporal::Timestamp;
use portable_atomic::AtomicU128;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard};

/// Encode a timestamp into a single 128-bit word.
///
/// The wallclock's sign bit is flipped so that unsigned comparison of the
/// packed form agrees with `HybridTimestamp`'s derived `Ord` (wallclock first,
/// then logical). Nothing here relies on that ordering today -- comparisons
/// unpack and use `Ord` directly -- but an order-preserving encoding keeps a
/// future `fetch_max` correct by construction rather than by luck.
#[inline]
const fn pack(ts: Timestamp) -> u128 {
    let biased = (ts.wallclock() as u64) ^ (1u64 << 63);
    ((biased as u128) << 32) | (ts.logical() as u128)
}

/// Inverse of [`pack`].
#[inline]
fn unpack(v: u128) -> Timestamp {
    let biased = (v >> 32) as u64;
    let wallclock = (biased ^ (1u64 << 63)) as i64;
    let logical = (v & 0xFFFF_FFFF) as u32;
    // SAFETY-equivalent: every value in the cell was produced by `pack` from an
    // already-valid `HybridTimestamp`, so both fields round-trip in range.
    HybridTimestamp::new_unchecked(wallclock, logical)
}

/// Top bit of the frontier word: a commit is sequenced but not yet visible.
///
/// Lives above the 96 bits `pack` uses, so it never disturbs the timestamp.
const IN_FLIGHT: u128 = 1 << 127;

/// Second flag bit: the frontier currently holds a reader's *reservation*
/// rather than a commit stamp.
///
/// This is what lets most reads be a pure load. A reservation is already
/// strictly greater than every applied commit, so a later reader can simply
/// return it instead of installing one of its own -- no atomic RMW, no write to
/// the shared cache line. It is only refreshed once the wallclock has moved past
/// it, which bounds how stale a shared snapshot can be to the resolution of the
/// clock itself (one microsecond).
///
/// Without this bit a reader cannot tell whether the frontier is a reservation
/// it may share or a commit stamp it must sort after, and must conservatively
/// CAS a fresh stamp every single time.
const RESERVED: u128 = 1 << 126;

/// Everything that is not a flag.
const TS_MASK: u128 = !(IN_FLIGHT | RESERVED);

/// The timestamp carried by a frontier word, ignoring the in-flight bit.
#[inline]
fn state_timestamp(state: u128) -> Timestamp {
    unpack(state & TS_MASK)
}

/// Hybrid-logical commit clock shared by every reader and writer.
#[derive(Debug)]
pub(crate) struct CommitClock {
    /// Writer serialization. Carries no data -- the frontier lives in the
    /// atomics -- but holding it is what keeps at most one commit in flight,
    /// which the read protocol depends on.
    serial: Mutex<()>,
    /// Allocation frontier: highest stamp handed to a commit or a reader.
    frontier: AtomicU128,
    /// Visibility frontier: highest commit stamp whose writes are in storage.
    applied: AtomicU128,
}

impl CommitClock {
    /// Create a clock whose frontier and visibility both start at `ts`.
    pub(crate) fn new(ts: Timestamp) -> Self {
        Self {
            serial: Mutex::new(()),
            frontier: AtomicU128::new(pack(ts)),
            applied: AtomicU128::new(pack(ts)),
        }
    }

    /// Whether the 128-bit cell is genuinely lock-free on this target.
    ///
    /// True on x86-64 with `cmpxchg16b` and on aarch64 with `casp`. If this is
    /// ever false the clock still behaves correctly -- `portable-atomic` falls
    /// back to its own locking -- but the point of the exercise is lost.
    #[cfg(test)]
    pub(crate) fn is_lock_free() -> bool {
        AtomicU128::is_lock_free()
    }

    /// Acquire the writer serialization lock.
    ///
    /// Returns a guard caching the allocation frontier as observed at lock
    /// time. Mirrors `Mutex::lock`'s shape so existing `.map_err(..)` and
    /// `.unwrap()` call sites keep working.
    pub(crate) fn lock(&self) -> std::result::Result<CommitClockGuard<'_>, ClockPoisoned> {
        let serial = self.serial.lock().map_err(|_| ClockPoisoned)?;
        let current = state_timestamp(self.frontier.load(Ordering::Acquire));
        Ok(CommitClockGuard {
            clock: self,
            _serial: serial,
            current,
            in_flight: false,
        })
    }

    /// Hand out a snapshot timestamp without taking any lock.
    ///
    /// See the module docs for why this is two frontiers and not one.
    pub(crate) fn snapshot_for_read(&self) -> Result<Timestamp> {
        loop {
            let state = self.frontier.load(Ordering::Acquire);

            if state & IN_FLIGHT != 0 {
                // A commit is sequenced but its writes are not in storage yet.
                // Park exactly on its stamp: every commit ordered before it has
                // applied, and strict-less-than visibility keeps it unseen.
                return Ok(state_timestamp(state));
            }

            let last = state_timestamp(state);
            let now = crate::core::temporal::time::now();

            // Fast path: an existing reservation already sits at or ahead of the
            // wallclock. Share it. It is strictly greater than every applied
            // commit (that is what made it a valid reservation), and no future
            // commit can land on it, because committers derive their stamp from
            // this same frontier. So this read needs no write at all -- which is
            // the whole point: a CAS on one shared word does not scale any
            // better than a mutex on one shared word.
            if state & RESERVED != 0 && last >= now {
                return Ok(last);
            }

            // Either the frontier is a commit stamp we must sort after, or the
            // wallclock has moved past the last reservation and it is time for a
            // fresh one. Both need a real reservation installed.
            let snapshot = if now > last {
                now
            } else {
                let next_logical =
                    last.logical()
                        .checked_add(1)
                        .ok_or(crate::core::error::Error::Temporal(
                            crate::core::error::TemporalError::LogicalCounterOverflow {
                                wallclock: last.wallclock(),
                                current_logical: last.logical(),
                            },
                        ))?;
                HybridTimestamp::new_unchecked(last.wallclock(), next_logical)
            };

            if self
                .frontier
                .compare_exchange_weak(
                    state,
                    pack(snapshot) | RESERVED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Ok(snapshot);
            }
            // The frontier moved under us -- a committer sequenced, or another
            // reader reserved. Retry against what is there now.
        }
    }

    /// The allocation frontier, without locking.
    ///
    /// This is the "last stamp handed out" and is what the old
    /// `*current_timestamp.lock()` read returned.
    pub(crate) fn load(&self) -> Timestamp {
        state_timestamp(self.frontier.load(Ordering::Acquire))
    }

    /// Whether the serialization lock is poisoned.
    #[cfg(test)]
    pub(crate) fn is_poisoned(&self) -> bool {
        self.serial.is_poisoned()
    }

    /// Poison the serialization lock, for tests that assert commits surface
    /// `LockPoisoned` rather than panicking.
    #[cfg(test)]
    pub(crate) fn poison_for_test(&self) {
        let _ = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _guard = self.serial.lock().expect("fresh clock is not poisoned");
                    panic!("deliberate poison");
                })
                .join()
        });
    }

    /// Whether a commit is currently sequenced but not yet visible.
    #[cfg(test)]
    pub(crate) fn commit_in_flight(&self) -> bool {
        self.frontier.load(Ordering::Acquire) & IN_FLIGHT != 0
    }

    /// The visibility frontier: the newest commit whose writes are in storage.
    #[cfg(test)]
    pub(crate) fn applied(&self) -> Timestamp {
        unpack(self.applied.load(Ordering::Acquire))
    }

    /// Raise both frontiers to `ts`, if `ts` is ahead of the allocation
    /// frontier; otherwise do nothing.
    ///
    /// For the replica applier, which must keep the local clock from regressing
    /// behind replicated history. Raising *both* is correct there because it is
    /// called after the batch has been applied, so the history is already
    /// visible. Returns whether the clock moved.
    pub(crate) fn raise_to(&self, ts: Timestamp) -> Result<bool> {
        let _serial = self
            .serial
            .lock()
            .map_err(|_| TransactionError::LockPoisoned {
                resource: "current_timestamp".to_string(),
            })?;
        let state = self.frontier.load(Ordering::Acquire);
        debug_assert!(
            state & IN_FLIGHT == 0,
            "the serialization lock excludes an in-flight commit"
        );
        if ts <= state_timestamp(state) {
            return Ok(false);
        }
        self.frontier.store(pack(ts), Ordering::Release);
        self.applied.store(pack(ts), Ordering::Release);
        Ok(true)
    }

    /// Force both frontiers to `ts`.
    ///
    /// For startup and recovery, which install a frontier wholesale rather than
    /// advancing it. Takes the serialization lock so it cannot race a commit.
    pub(crate) fn reset_to(&self, ts: Timestamp) -> Result<()> {
        let _serial = self
            .serial
            .lock()
            .map_err(|_| TransactionError::LockPoisoned {
                resource: "current_timestamp".to_string(),
            })?;
        self.frontier.store(pack(ts), Ordering::Release);
        self.applied.store(pack(ts), Ordering::Release);
        Ok(())
    }
}

/// The commit clock's serialization lock was poisoned by a panicking committer.
#[derive(Debug)]
pub(crate) struct ClockPoisoned;

/// Guard held by a committer for the duration of a commit.
///
/// Deref yields the allocation frontier as observed when the lock was taken.
pub(crate) struct CommitClockGuard<'a> {
    clock: &'a CommitClock,
    _serial: MutexGuard<'a, ()>,
    current: Timestamp,
    /// Set once `advance_with` has marked the frontier in-flight, cleared by
    /// `publish_applied`. If it survives to `Drop` the commit aborted and the
    /// bit has to come back off.
    in_flight: bool,
}

impl std::ops::Deref for CommitClockGuard<'_> {
    type Target = Timestamp;

    fn deref(&self) -> &Timestamp {
        &self.current
    }
}

impl std::fmt::Debug for CommitClockGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CommitClockGuard({})", self.current)
    }
}

impl CommitClockGuard<'_> {
    /// Assign this commit's stamp, publishing it to the allocation frontier.
    ///
    /// `compute` derives the commit stamp from the frontier it is handed and
    /// must return something strictly greater. It is a closure rather than a
    /// value because a reader can reserve a stamp between the guard being taken
    /// and this call: on CAS failure the frontier has moved, and the stamp has
    /// to be recomputed against the new one rather than silently clobbering the
    /// reservation.
    ///
    /// Retries are rare (they need a reader to reserve inside a window of a few
    /// instructions) and `compute` is pure apart from reading the wallclock.
    pub(crate) fn advance_with<F>(&mut self, mut compute: F) -> Result<Timestamp>
    where
        F: FnMut(Timestamp) -> Result<Timestamp>,
    {
        loop {
            let observed = self.clock.frontier.load(Ordering::Acquire);
            debug_assert!(
                observed & IN_FLIGHT == 0,
                "committers are serialized, so no commit can already be in flight"
            );
            let base = state_timestamp(observed);
            let commit = compute(base)?;
            debug_assert!(
                commit > base,
                "commit stamp must be strictly greater than the frontier it was derived from"
            );
            if self
                .clock
                .frontier
                .compare_exchange_weak(
                    observed,
                    pack(commit) | IN_FLIGHT,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                self.current = commit;
                self.in_flight = true;
                return Ok(commit);
            }
            // Only a reader's reservation can land here (committers are
            // serialized), and it has moved the frontier past `base`. Recompute
            // against the new frontier rather than clobbering the reservation.
        }
    }

    /// Publish `ts` as visible.
    ///
    /// Called once apply and finalize are complete, so that a lock-free reader
    /// may start handing out snapshots strictly after this commit. Must not be
    /// called before the writes are in storage -- that is the whole invariant.
    pub(crate) fn publish_applied(&mut self, ts: Timestamp) {
        self.clock.applied.store(pack(ts), Ordering::Release);
        // Release the parked readers. `fetch_and` rather than a store: a reader
        // may have reserved... it may not, in fact -- readers never write while
        // the bit is set -- but clearing exactly one bit keeps this correct
        // even if that ever changes.
        self.clock.frontier.fetch_and(!IN_FLIGHT, Ordering::Release);
        self.in_flight = false;
    }
}

impl Drop for CommitClockGuard<'_> {
    fn drop(&mut self) {
        if self.in_flight {
            // The commit aborted after assigning its stamp. Clear the bit so
            // readers stop parking on a stamp that will never be applied; the
            // stamp itself stays burned at the frontier, which is harmless
            // because the next commit sorts strictly after it.
            self.clock.frontier.fetch_and(!IN_FLIGHT, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::temporal::time;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};

    fn ts(wallclock: i64, logical: u32) -> Timestamp {
        HybridTimestamp::new_unchecked(wallclock, logical)
    }

    /// A commit as the write path performs one: derive strictly-greater from
    /// whatever frontier we are handed.
    fn commit_on(clock: &CommitClock) -> Timestamp {
        let mut guard = clock.lock().expect("clock");
        let commit = guard
            .advance_with(|base| {
                let now = time::now().wallclock();
                base.send(now).map_err(crate::core::error::Error::Temporal)
            })
            .expect("advance");
        guard.publish_applied(commit);
        commit
    }

    #[test]
    fn packing_round_trips_including_the_extremes() {
        for case in [
            ts(0, 0),
            ts(1, 1),
            ts(-1, 0),
            ts(i64::MIN, 0),
            ts(i64::MAX, u32::MAX),
            ts(1_800_000_000_000_000, 4_096),
            time::now(),
        ] {
            assert_eq!(unpack(pack(case)), case, "round trip failed for {case}");
        }
    }

    #[test]
    fn packed_order_matches_timestamp_order() {
        // Ordered ascending, and deliberately straddling zero so the sign-bias
        // is exercised rather than assumed.
        let ordered = [
            ts(i64::MIN, 0),
            ts(-5, 0),
            ts(-5, 7),
            ts(0, 0),
            ts(0, 1),
            ts(17, 0),
            ts(i64::MAX, u32::MAX),
        ];
        for pair in ordered.windows(2) {
            assert!(pair[0] < pair[1], "test data not ascending");
            assert!(
                pack(pair[0]) < pack(pair[1]),
                "packed order disagrees with Ord for {} vs {}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn reports_whether_the_cell_is_lock_free() {
        // Not an assertion: correctness does not depend on it, but a silent
        // fallback to portable-atomic's own locking would quietly undo the
        // point of this type, so make it visible in test output.
        println!(
            "AtomicU128 lock-free on this target: {}",
            CommitClock::is_lock_free()
        );
    }

    #[test]
    fn quiescent_reads_never_go_backwards_and_do_move_forward() {
        // Reads share a standing reservation within one clock tick, so
        // consecutive snapshots may be equal -- that is the optimization. What
        // must hold is that they never regress, and that they do advance once
        // the wallclock moves, or an idle database would freeze its snapshot
        // and never show a fact whose valid time starts later.
        let clock = CommitClock::new(time::now());
        let first = clock.snapshot_for_read().expect("snapshot");
        let mut previous = first;
        let mut shared = 0;
        for _ in 0..1_000 {
            let next = clock.snapshot_for_read().expect("snapshot");
            assert!(
                next >= previous,
                "snapshots must never regress: {next} < {previous}"
            );
            if next == previous {
                shared += 1;
            }
            previous = next;
        }
        assert!(
            shared > 0,
            "no snapshot was shared, so the fast path never engaged"
        );

        // Now let the wallclock move and confirm the reservation refreshes.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let after_idle = clock.snapshot_for_read().expect("snapshot");
        assert!(
            after_idle > previous,
            "a standing reservation must refresh once the wallclock passes it: \
             {after_idle} !> {previous}"
        );
    }

    #[test]
    fn an_unapplied_commit_is_invisible_and_an_applied_one_is_visible() {
        let clock = CommitClock::new(time::now());

        let mut guard = clock.lock().expect("clock");
        let commit = guard
            .advance_with(|base| {
                base.send(time::now().wallclock())
                    .map_err(crate::core::error::Error::Temporal)
            })
            .expect("advance");

        // Sequenced but not applied: a reader must land exactly ON the commit
        // stamp, which the strict-less-than visibility check treats as unseen.
        let during = clock.snapshot_for_read().expect("snapshot");
        assert_eq!(
            during, commit,
            "a reader must not be handed a stamp past an unapplied commit"
        );
        // Spelled as the MVCC predicate itself so the assertion reads the way
        // the visibility rule does, rather than as an inverted comparison.
        let visible = commit < during;
        assert!(!visible, "unapplied commit must not be visible");

        guard.publish_applied(commit);
        drop(guard);

        // Applied: now a reader must be able to see it.
        let after = clock.snapshot_for_read().expect("snapshot");
        assert!(
            commit < after,
            "an applied commit must be visible: {commit} !< {after}"
        );
    }

    #[test]
    fn an_aborted_commit_leaves_the_clock_correct_not_merely_safe() {
        let clock = CommitClock::new(time::now());
        let applied_before = clock.applied();

        // Advance the frontier, then abort without publishing.
        {
            let mut guard = clock.lock().expect("clock");
            guard
                .advance_with(|base| {
                    base.send(time::now().wallclock())
                        .map_err(crate::core::error::Error::Temporal)
                })
                .expect("advance");
        }

        // Readers park on the abandoned stamp. That is correct: every applied
        // commit is strictly below it, and no commit at it exists.
        assert!(
            !clock.commit_in_flight(),
            "dropping the guard must clear the in-flight bit, or every later \
             read parks forever on a stamp that will never apply"
        );

        let stranded = clock.snapshot_for_read().expect("snapshot");
        assert_eq!(stranded, clock.load());
        assert!(applied_before < stranded);
        assert_eq!(clock.applied(), applied_before, "abort must not publish");

        // And the next real commit closes the gap.
        let next = commit_on(&clock);
        assert!(
            next > stranded,
            "next commit must clear the abandoned stamp"
        );
        let after = clock.snapshot_for_read().expect("snapshot");
        assert!(next < after, "and become visible once applied");
    }

    #[test]
    fn a_reservation_never_collides_with_a_later_commit() {
        // The reservation exists to stop a same-tick commit from recomputing
        // the reader's exact stamp, which would close a superseded version's
        // interval at [C1, S) and hide it from the reader that reserved S.
        let clock = CommitClock::new(time::now());
        for _ in 0..2_000 {
            let snapshot = clock.snapshot_for_read().expect("snapshot");
            let commit = commit_on(&clock);
            assert!(
                commit > snapshot,
                "commit {commit} must sort strictly after reserved snapshot {snapshot}"
            );
        }
    }

    /// The invariant that makes the lock-free read path safe, stated directly:
    /// a snapshot is never past the allocation frontier, and the frontier
    /// always carries an in-flight commit's stamp. A single-frontier design --
    /// reader computes `max(now, f+1)` unconditionally -- violates this and
    /// this test is what catches it.
    #[test]
    fn concurrent_readers_never_outrun_an_in_flight_commit() {
        let clock = Arc::new(CommitClock::new(time::now()));
        let stop = Arc::new(AtomicBool::new(false));
        // The in-flight commit stamp, packed; 0 means "none in flight".
        let in_flight = Arc::new(portable_atomic::AtomicU128::new(0));
        let violations = Arc::new(AtomicU64::new(0));
        let observations = Arc::new(AtomicU64::new(0));

        let writer = {
            let clock = Arc::clone(&clock);
            let stop = Arc::clone(&stop);
            let in_flight = Arc::clone(&in_flight);
            std::thread::spawn(move || {
                let mut commits = 0u64;
                while !stop.load(AtomicOrdering::Relaxed) {
                    let mut guard = clock.lock().expect("clock");
                    let commit = guard
                        .advance_with(|base| {
                            base.send(time::now().wallclock())
                                .map_err(crate::core::error::Error::Temporal)
                        })
                        .expect("advance");
                    in_flight.store(pack(commit), AtomicOrdering::Release);
                    // Stand in for guards + WAL + fsync + apply: a window in
                    // which the commit is sequenced but absent from storage.
                    for _ in 0..64 {
                        std::hint::spin_loop();
                    }
                    in_flight.store(0, AtomicOrdering::Release);
                    guard.publish_applied(commit);
                    commits += 1;
                }
                commits
            })
        };

        let readers: Vec<_> = (0..4)
            .map(|_| {
                let clock = Arc::clone(&clock);
                let stop = Arc::clone(&stop);
                let in_flight = Arc::clone(&in_flight);
                let violations = Arc::clone(&violations);
                let observations = Arc::clone(&observations);
                std::thread::spawn(move || {
                    while !stop.load(AtomicOrdering::Relaxed) {
                        let snapshot = clock.snapshot_for_read().expect("snapshot");
                        let packed = in_flight.load(AtomicOrdering::Acquire);
                        if packed != 0 {
                            observations.fetch_add(1, AtomicOrdering::Relaxed);
                            // A commit is in flight and NOT in storage. If our
                            // snapshot sorts after it, MVCC would call it
                            // visible and the read would find nothing.
                            if unpack(packed) < snapshot {
                                violations.fetch_add(1, AtomicOrdering::Relaxed);
                            }
                        }
                        // Frontier-bound invariant: holds unconditionally.
                        assert!(
                            snapshot <= clock.load(),
                            "snapshot {snapshot} outran the allocation frontier"
                        );
                    }
                })
            })
            .collect();

        std::thread::sleep(std::time::Duration::from_millis(300));
        stop.store(true, AtomicOrdering::Relaxed);

        let commits = writer.join().expect("writer");
        for reader in readers {
            reader.join().expect("reader");
        }

        assert!(commits > 0, "writer made no progress");
        assert!(
            observations.load(AtomicOrdering::Relaxed) > 0,
            "test never observed an in-flight commit, so it proved nothing \
             (commits: {commits})"
        );
        assert_eq!(
            violations.load(AtomicOrdering::Relaxed),
            0,
            "readers were handed snapshots past an unapplied commit"
        );
    }

    #[test]
    fn concurrent_reads_are_monotonic_and_a_later_commit_still_outranks_them_all() {
        // Readers deliberately SHARE a reservation now, so uniqueness is not the
        // property to test. What still has to hold -- and is what the
        // reservation exists for -- is that no commit ever lands on a stamp
        // already handed to a reader.
        let clock = Arc::new(CommitClock::new(time::now()));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let clock = Arc::clone(&clock);
                std::thread::spawn(move || {
                    let mut seen = Vec::with_capacity(5_000);
                    let mut previous: Option<Timestamp> = None;
                    for _ in 0..5_000 {
                        let s = clock.snapshot_for_read().expect("snapshot");
                        if let Some(p) = previous {
                            assert!(s >= p, "a thread saw its snapshots regress: {s} < {p}");
                        }
                        previous = Some(s);
                        seen.push(s);
                    }
                    seen
                })
            })
            .collect();

        let all: Vec<Timestamp> = handles
            .into_iter()
            .flat_map(|h| h.join().expect("reader"))
            .collect();

        let highest = *all.iter().max().expect("snapshots");
        let commit = commit_on(&clock);
        assert!(
            commit > highest,
            "a commit landed at or below a snapshot already handed out: \
             {commit} !> {highest}"
        );
    }

    #[test]
    fn reset_installs_both_frontiers() {
        let clock = CommitClock::new(time::now());
        commit_on(&clock);
        let target = ts(1_700_000_000_000_000, 0);
        clock.reset_to(target).expect("reset");
        assert_eq!(clock.load(), target);
        assert_eq!(clock.applied(), target);
        // And the clock is quiescent again, so reads reserve forward from it.
        let snapshot = clock.snapshot_for_read().expect("snapshot");
        assert!(snapshot > target);
    }
}
