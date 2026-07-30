//! Atomic-publish gate for replica batch application (Issue #3788).
//!
//! # The problem this solves
//!
//! [`crate::storage::replication::apply::apply_replica_batch`] is careful about
//! frame *selection*: it only ever hands a complete `[BeginTx .. CommitTx]`
//! band to the replay engine, so an incomplete transaction is never submitted.
//! But the selected frame is then applied **operation by operation** into
//! [`super::CurrentStorage`], and current-state reads (`get_node`, `get_edge`,
//! adjacency lookups) take no lock at all. The interval between inserting a
//! transaction's first node and its last is therefore a window in which a
//! concurrent reader observes a *half-applied* transaction -- a node whose
//! sibling, or whose edge endpoint, does not exist yet.
//!
//! Whole-frame **submission** is not reader **isolation** during apply. This
//! gate supplies the second property.
//!
//! # Design: a seqlock that is disarmed (and free) on a primary
//!
//! A plain `RwLock` around every current-state read would work but would put a
//! contended atomic RMW on the hot path of *every* database, primary included,
//! and would serialize the reader fan-out a read-scale-out replica exists to
//! provide. Instead:
//!
//! * The gate starts **disarmed**. A disarmed read is one acquire load of a
//!   never-written cache line (free on x86, a cheap load-acquire elsewhere)
//!   plus a perfectly-predicted branch -- the primary pays effectively nothing,
//!   and no lock is ever touched.
//! * [`ApplyGate::arm`] is called exactly once, when a database enters replica
//!   mode ([`crate::db::AletheiaDB::start_replication`]), before the applier
//!   thread that will publish into it is spawned.
//! * While armed, reads use a **seqlock**: read `seq`, run the read, re-read
//!   `seq`, retry if it moved. Readers issue only *loads* on the shared
//!   sequence word, so the line stays Shared in every core's cache and reader
//!   throughput still scales with cores. Only a reader that genuinely collides
//!   with an in-flight publish pays anything.
//! * A publisher ([`ApplyGate::begin_publish`]) makes `seq` odd for the whole
//!   duration of its current-state mutations, so any read overlapping the
//!   publish is retried rather than allowed to observe intermediate state.
//!
//! # Blocking fallback
//!
//! A pure spin-retry would burn CPU if a replica were applying continuously, and
//! it cannot serve reads whose closure is `FnOnce` (the zero-copy
//! [`super::CurrentStorage::with_node`] accessor). So the gate also carries an
//! `RwLock<()>` the publisher holds for exactly the same window: a reader that
//! spins out falls back to `fallback.read()` and *sleeps* until the publish
//! completes, and `FnOnce` readers use that path directly.
//!
//! # Re-entrancy
//!
//! The replay engine itself reads current state while publishing (the #3419
//! idempotent re-application guards call `current.get_node(..)`). Those reads
//! run on the publishing thread, which already holds the gate; taking it again
//! would deadlock on the fallback lock and livelock on the seqlock. A
//! thread-local publish depth makes the gate transparently re-entrant for the
//! publisher, and only for it.
//!
//! # Scope of the guarantee
//!
//! The gate makes each **point lookup** on [`super::CurrentStorage`] atomic with
//! respect to a replica apply: entity lookups, adjacency lookups, and degree
//! reads either see all of a replicated transaction or none of it. Bulk scans
//! and iterators over the current-state maps are deliberately *not* gated --
//! `DashMap` iteration was never a point-in-time snapshot even on a primary, so
//! gating it would be a promise the storage layer cannot keep. A caller that
//! needs a true snapshot should use the bi-temporal `*_at_time` reads.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::RwLock;

thread_local! {
    /// Publish nesting depth for the current thread. Non-zero exactly while
    /// this thread is inside [`ApplyGate::begin_publish`], which makes the
    /// gate re-entrant for the publisher (and nobody else).
    static PUBLISH_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// How many seqlock attempts a colliding reader makes before parking on the
/// blocking fallback instead of spinning. A publish window is a handful of
/// microseconds; this is comfortably longer than a typical one while still
/// bounding wasted CPU.
const SPIN_LIMIT: u32 = 64;

/// Publish gate making a replica's batch apply atomically visible to concurrent
/// current-state readers. See the module docs.
#[derive(Debug)]
pub(crate) struct ApplyGate {
    /// Set once when the owning database enters replica mode. Disarmed gates
    /// short-circuit every read before touching `seq` or `fallback`.
    armed: AtomicBool,
    /// Even = current state is stable; odd = a publish is mutating it.
    seq: AtomicU64,
    /// Held exclusively for the publish window, so a colliding reader can sleep
    /// instead of spinning (and so `FnOnce` readers have a path at all).
    fallback: RwLock<()>,
}

impl ApplyGate {
    /// Create a disarmed gate: reads short-circuit, publishes are no-op-cheap.
    pub(crate) const fn new() -> Self {
        Self {
            armed: AtomicBool::new(false),
            seq: AtomicU64::new(0),
            fallback: RwLock::new(()),
        }
    }

    /// Arm the gate so current-state reads become atomic with respect to
    /// [`Self::begin_publish`].
    ///
    /// Must happen-before the first publish. `start_replication` arms before
    /// spawning the applier thread, and thread spawn is a synchronization edge,
    /// so the publisher always observes the armed gate.
    pub(crate) fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }

    /// Disarm the gate, restoring the ungated read fast path.
    ///
    /// Only valid once no publisher can still be running -- `promote_to_primary`
    /// stops and joins the applier thread before calling this.
    pub(crate) fn disarm(&self) {
        self.armed.store(false, Ordering::Release);
    }

    /// Whether reads are currently gated. Diagnostic only -- the read paths
    /// load `armed` directly rather than going through this.
    #[cfg(test)]
    #[inline]
    pub(crate) fn is_armed(&self) -> bool {
        self.armed.load(Ordering::Acquire)
    }

    /// Run a retryable read so that it never observes a partially-applied
    /// replicated transaction.
    ///
    /// `f` must be side-effect free: it is re-run when it races a publish.
    #[inline]
    pub(crate) fn read<R>(&self, f: impl Fn() -> R) -> R {
        if !self.armed.load(Ordering::Acquire) {
            return f();
        }
        self.read_gated(f)
    }

    /// The armed path of [`Self::read`], kept out of line so the disarmed
    /// fast path stays a load-and-branch at every call site.
    fn read_gated<R>(&self, f: impl Fn() -> R) -> R {
        for _ in 0..SPIN_LIMIT {
            let before = self.seq.load(Ordering::Acquire);
            if before & 1 == 0 {
                let out = f();
                // Canonical seqlock retry check: an Acquire fence AFTER the
                // guarded reads and BEFORE re-reading the counter. A plain
                // `load(Acquire)` here would be the wrong side -- it orders
                // what comes after it, not the reads that came before -- so
                // the fence is what actually stops the guarded reads from
                // being observed as if they happened after the re-read.
                std::sync::atomic::fence(Ordering::Acquire);
                if self.seq.load(Ordering::Relaxed) == before {
                    return out;
                }
            } else if publishing_here() {
                // The publisher reading its own in-progress state (the replay
                // engine's idempotency guards). It is the only thread allowed
                // to see the intermediate state -- it is the one creating it.
                return f();
            }
            std::hint::spin_loop();
        }

        // Sustained apply: sleep on the fallback rather than burning a core.
        // Holding it guarantees no publish is in flight for the whole read.
        let _guard = self.fallback.read();
        f()
    }

    /// Run a single-shot read under the gate.
    ///
    /// The `FnOnce` counterpart to [`Self::read`], for zero-copy accessors whose
    /// closure cannot be re-run. It cannot use the seqlock's retry, so while
    /// armed it always takes the blocking fallback -- correct, and heavier than
    /// [`Self::read`]. Prefer `read` whenever the closure is `Fn`.
    ///
    /// # Nesting
    ///
    /// A [`Self::read`] nested inside `f` is safe: no publisher can make `seq`
    /// odd while this fallback read guard is held, so the inner seqlock succeeds
    /// on its first attempt and never reaches its own blocking fallback. A
    /// *second* `read_once` nested inside `f` would re-acquire the fallback as a
    /// reader, which `parking_lot` may park behind a queued writer -- so callers
    /// must honour the "do not re-enter storage from the closure" contract
    /// already documented on `CurrentStorage::with_node`.
    #[inline]
    pub(crate) fn read_once<R>(&self, f: impl FnOnce() -> R) -> R {
        if !self.armed.load(Ordering::Acquire) || publishing_here() {
            return f();
        }
        let _guard = self.fallback.read();
        f()
    }

    /// Open a publish window: every current-state mutation made until the
    /// returned guard is dropped becomes visible to gated readers atomically,
    /// all at once, when it drops.
    ///
    /// Windows must not **nest** — a second `begin_publish` on a thread already
    /// holding one would block forever on the fallback's exclusive lock. The
    /// re-entrancy this gate provides is for *reads* issued by the publishing
    /// thread while its window is open, which is what the replay engine needs.
    /// There is exactly one publisher (the applier thread) per replica.
    pub(crate) fn begin_publish(&self) -> PublishGuard<'_> {
        let guard = self.fallback.write();
        PUBLISH_DEPTH.with(|d| d.set(d.get() + 1));
        // Odd => "unstable" for every seqlock reader, for the whole window.
        self.seq.fetch_add(1, Ordering::AcqRel);
        PublishGuard {
            gate: self,
            _lock: guard,
        }
    }
}

impl Default for ApplyGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether the calling thread is the one currently publishing.
#[inline]
fn publishing_here() -> bool {
    PUBLISH_DEPTH.with(|d| d.get()) > 0
}

/// Open publish window; dropping it publishes the batch atomically.
pub(crate) struct PublishGuard<'a> {
    gate: &'a ApplyGate,
    _lock: parking_lot::RwLockWriteGuard<'a, ()>,
}

impl Drop for PublishGuard<'_> {
    fn drop(&mut self) {
        // Back to even: readers that raced this window retry and now observe
        // the complete batch. Release pairs with the readers' Acquire loads, so
        // every mutation made inside the window is visible to them.
        self.gate.seq.fetch_add(1, Ordering::Release);
        PUBLISH_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn disarmed_gate_runs_reads_directly() {
        let gate = ApplyGate::new();
        assert!(!gate.is_armed());
        assert_eq!(gate.read(|| 7), 7);
        assert_eq!(gate.read_once(|| 8), 8);
    }

    #[test]
    fn arm_and_disarm_flip_the_fast_path() {
        let gate = ApplyGate::new();
        gate.arm();
        assert!(gate.is_armed());
        assert_eq!(gate.read(|| 1), 1);
        gate.disarm();
        assert!(!gate.is_armed());
    }

    #[test]
    fn publish_window_makes_seq_odd_then_even() {
        let gate = ApplyGate::new();
        gate.arm();
        assert_eq!(gate.seq.load(Ordering::Acquire) & 1, 0);
        {
            let _publish = gate.begin_publish();
            assert_eq!(gate.seq.load(Ordering::Acquire) & 1, 1);
        }
        assert_eq!(gate.seq.load(Ordering::Acquire) & 1, 0);
    }

    #[test]
    fn publisher_can_read_its_own_in_progress_state() {
        // The replay engine's idempotency guards read current state while the
        // publish window is open. Without re-entrancy this deadlocks.
        let gate = ApplyGate::new();
        gate.arm();
        let _publish = gate.begin_publish();
        assert_eq!(gate.read(|| 42), 42);
        assert_eq!(gate.read_once(|| 43), 43);
    }

    #[test]
    fn closing_a_window_resets_state_so_the_next_one_works() {
        let gate = ApplyGate::new();
        gate.arm();
        let first = gate.begin_publish();
        assert_eq!(gate.read(|| 1), 1);
        drop(first);
        // Both the publish depth and the exclusive lock are released, so the
        // applier's next batch opens a fresh window normally.
        let _second = gate.begin_publish();
        assert_eq!(gate.read(|| 2), 2);
    }

    /// The core property: a reader never observes a publish's intermediate
    /// state. The "state" here is two counters a publisher bumps one at a time;
    /// a reader must never see them disagree.
    #[test]
    fn concurrent_reads_never_observe_a_partial_publish() {
        let gate = Arc::new(ApplyGate::new());
        gate.arm();
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let publisher = {
            let (gate, a, b, stop) = (
                Arc::clone(&gate),
                Arc::clone(&a),
                Arc::clone(&b),
                Arc::clone(&stop),
            );
            std::thread::spawn(move || {
                for _ in 0..2_000 {
                    let _publish = gate.begin_publish();
                    a.fetch_add(1, Ordering::SeqCst);
                    std::thread::yield_now();
                    b.fetch_add(1, Ordering::SeqCst);
                }
                stop.store(true, Ordering::SeqCst);
            })
        };

        let readers: Vec<_> = (0..3)
            .map(|_| {
                let (gate, a, b, stop) = (
                    Arc::clone(&gate),
                    Arc::clone(&a),
                    Arc::clone(&b),
                    Arc::clone(&stop),
                );
                std::thread::spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        let (seen_a, seen_b) = gate.read(|| {
                            let x = a.load(Ordering::SeqCst);
                            let y = b.load(Ordering::SeqCst);
                            (x, y)
                        });
                        assert_eq!(
                            seen_a, seen_b,
                            "a gated read observed a partially-published update"
                        );
                    }
                })
            })
            .collect();

        publisher.join().expect("publisher thread");
        for reader in readers {
            reader.join().expect("reader thread");
        }
    }
}
