//! Unified Concurrent WAL System.
//!
//! This module provides [`ConcurrentWalSystem`], which combines the concurrent
//! WAL striped architecture with the flush coordinator into a single, cohesive
//! component that can be used as a drop-in replacement for the old `WriteAheadLog`.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    ConcurrentWalSystem                           │
//! │                                                                  │
//! │  ┌──────────────────────┐    ┌─────────────────────────────┐   │
//! │  │    ConcurrentWal     │    │     FlushCoordinator        │   │
//! │  │  (Striped Buffers)   │───▶│   (Segment Management)      │   │
//! │  └──────────────────────┘    └─────────────────────────────┘   │
//! │                                                                  │
//! │  ┌──────────────────────────────────────────────────────────┐  │
//! │  │                   Background Flush Thread                  │  │
//! │  │  - Drains stripes periodically                            │  │
//! │  │  - Writes to segment files                                │  │
//! │  │  - Notifies completion handles                            │  │
//! │  └──────────────────────────────────────────────────────────┘  │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use aletheiadb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
//!
//! let config = ConcurrentWalSystemConfig::new("data/wal");
//! let wal = ConcurrentWalSystem::new(config)?;
//!
//! // Async append (returns immediately)
//! let lsn = wal.append_async(operation)?;
//!
//! // Sync append (waits for durability)
//! let lsn = wal.append_sync(operation)?;
//!
//! // Shutdown gracefully
//! wal.shutdown();
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use arc_swap::ArcSwapOption;

use super::concurrent::{
    AppendDrainer, ConcurrentWal, ConcurrentWalConfig, DEFAULT_MAX_APPEND_BLOCK_MS,
};
use super::flush_coordinator::{FlushCoordinator, FlushCoordinatorConfig, FlushStats};
use super::group_commit::{DEFAULT_ACQUIRE_TIMEOUT_MS, GroupCommitConfig, GroupCommitCoordinator};
use super::{LSN, WalOperation};
use crate::core::error::{Error, Result, StorageError};
use crate::storage::wal::DurabilityMode;

/// Configuration for the concurrent WAL system.
#[derive(Clone)]
pub struct ConcurrentWalSystemConfig {
    /// WAL directory path.
    pub wal_dir: PathBuf,
    /// Number of stripes (should be power of 2).
    pub num_stripes: usize,
    /// Ring buffer capacity per stripe.
    pub stripe_capacity: usize,
    /// Maximum segment size in bytes before rotation.
    pub segment_size: usize,
    /// Number of segments to retain.
    pub segments_to_retain: usize,
    /// Flush interval in milliseconds.
    pub flush_interval_ms: u64,
    /// Durability mode.
    pub durability_mode: DurabilityMode,
    /// Write buffer size for segment files.
    pub write_buffer_size: usize,
    /// Optional cipher for WAL entry encryption.
    ///
    /// When set, entries are encrypted before writing to disk and segments
    /// use version 2 format. Passed through to `FlushCoordinatorConfig`.
    pub wal_cipher: Option<Arc<dyn crate::encryption::cipher::Cipher>>,
    /// Optional provisioned WAL key version (Issue #488 version-provisioning).
    ///
    /// When set alongside `wal_cipher`, the WAL keyring is built at this version
    /// (via [`WalKeyring::single_versioned`]) instead of the hard-coded
    /// [`INITIAL_WAL_KEY_VERSION`], so a rotate-then-reopen stamps and reports
    /// the real on-disk version. `None` (the default) reproduces prior behavior
    /// exactly. Ignored when `wal_cipher` is `None` (a plaintext WAL has no
    /// keyring).
    pub wal_key_version: Option<u32>,
    /// Recovery policy for a crash-torn trailing entry (Issue #3433).
    ///
    /// When `true` (the default), [`ConcurrentWalSystem::read_from`] tolerates
    /// an undecodable trailing entry in the final WAL segment during replay;
    /// when `false`, any parse failure hard-errors (fail-stop recovery). See
    /// [`crate::config::WalConfig::tolerate_torn_tail`].
    pub tolerate_torn_tail: bool,
    /// Maximum time (milliseconds) a writer blocks on a full ring buffer
    /// before failing with a diagnosable error (Issue #3798).
    ///
    /// `0` means unbounded (legacy behavior: block forever). Plumbed straight
    /// through to [`ConcurrentWalConfig::max_append_block_ms`]; default
    /// [`DEFAULT_MAX_APPEND_BLOCK_MS`].
    pub max_append_block_ms: u64,
    /// Bound (milliseconds) on acquiring the group-commit coordinator's state
    /// mutex (Issue #3798).
    ///
    /// `0` means unbounded (legacy `Mutex::lock`). Plumbed through to
    /// [`GroupCommitConfig::acquire_timeout_ms`]; only consulted by the
    /// durability modes that build a coordinator (GroupCommit, AsyncBatched).
    /// Default [`DEFAULT_ACQUIRE_TIMEOUT_MS`].
    pub acquire_timeout_ms: u64,
}

impl std::fmt::Debug for ConcurrentWalSystemConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConcurrentWalSystemConfig")
            .field("wal_dir", &self.wal_dir)
            .field("num_stripes", &self.num_stripes)
            .field("stripe_capacity", &self.stripe_capacity)
            .field("segment_size", &self.segment_size)
            .field("segments_to_retain", &self.segments_to_retain)
            .field("flush_interval_ms", &self.flush_interval_ms)
            .field("durability_mode", &self.durability_mode)
            .field("write_buffer_size", &self.write_buffer_size)
            .field(
                "wal_cipher",
                &self.wal_cipher.as_ref().map(|c| c.algorithm_name()),
            )
            .field("wal_key_version", &self.wal_key_version)
            .field("tolerate_torn_tail", &self.tolerate_torn_tail)
            .field("max_append_block_ms", &self.max_append_block_ms)
            .field("acquire_timeout_ms", &self.acquire_timeout_ms)
            .finish()
    }
}

impl Default for ConcurrentWalSystemConfig {
    fn default() -> Self {
        Self {
            wal_dir: PathBuf::from("data/wal"),
            num_stripes: 16,
            stripe_capacity: 1024,
            segment_size: 64 * 1024 * 1024, // 64 MB
            segments_to_retain: 10,
            flush_interval_ms: 10,
            durability_mode: DurabilityMode::Synchronous,
            write_buffer_size: 64 * 1024, // 64 KB
            wal_cipher: None,
            wal_key_version: None,
            tolerate_torn_tail: true,
            max_append_block_ms: DEFAULT_MAX_APPEND_BLOCK_MS,
            acquire_timeout_ms: DEFAULT_ACQUIRE_TIMEOUT_MS,
        }
    }
}

impl ConcurrentWalSystemConfig {
    /// Create a new config with the specified WAL directory.
    pub fn new(wal_dir: impl Into<PathBuf>) -> Self {
        Self {
            wal_dir: wal_dir.into(),
            ..Default::default()
        }
    }

    /// Set the durability mode.
    pub fn with_durability_mode(mut self, mode: DurabilityMode) -> Self {
        self.durability_mode = mode;
        self
    }

    /// Set the number of stripes.
    pub fn with_num_stripes(mut self, num_stripes: usize) -> Self {
        self.num_stripes = num_stripes.next_power_of_two();
        self
    }

    /// Set the flush interval in milliseconds.
    pub fn with_flush_interval_ms(mut self, ms: u64) -> Self {
        self.flush_interval_ms = ms;
        self
    }

    /// Set the bound on blocking appends in milliseconds (Issue #3798).
    ///
    /// `0` disables the bound (legacy unbounded blocking).
    pub fn with_max_append_block_ms(mut self, ms: u64) -> Self {
        self.max_append_block_ms = ms;
        self
    }

    /// Set the bound on acquiring the group-commit state mutex, in
    /// milliseconds (Issue #3798).
    ///
    /// `0` disables the bound (legacy unbounded acquisition).
    pub fn with_acquire_timeout_ms(mut self, ms: u64) -> Self {
        self.acquire_timeout_ms = ms;
        self
    }
}

/// Signal for waking up the flush thread when batch is full.
struct FlushNotifier {
    /// Lock for condvar.
    lock: Mutex<bool>,
    /// Condvar to signal immediate flush.
    condvar: Condvar,
}

impl FlushNotifier {
    fn new() -> Self {
        Self {
            lock: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    /// Signal the flush thread to wake up immediately.
    fn notify(&self) {
        let mut guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        *guard = true;
        self.condvar.notify_one();
    }

    /// Wait for a signal or timeout, returns true if signaled.
    fn wait_timeout(&self, duration: Duration) -> bool {
        let mut guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());

        // Check if already signaled before waiting.
        // This handles the race where notify() is called before we enter wait_timeout().
        if *guard {
            *guard = false; // Reset signal
            return true;
        }

        let (new_guard, result) = self
            .condvar
            .wait_timeout(guard, duration)
            .unwrap_or_else(|e| e.into_inner());
        guard = new_guard;

        let was_signaled = *guard && !result.timed_out();
        *guard = false; // Reset signal
        was_signaled
    }
}

/// Threshold for consecutive flush errors before logging a critical warning.
const FLUSH_ERROR_WARNING_THRESHOLD: u64 = 3;

/// Floor for the heartbeat staleness threshold (Issue #3798). Even a very
/// short flush interval gets at least this much slack before the flush thread
/// is judged stalled, so a scheduling hiccup never reads as a dead flusher.
///
/// Five seconds, not one: at the default 10ms interval the floor IS the
/// threshold, and a single slow fsync, a heavily oversubscribed CI runner, or
/// a `cargo llvm-cov` run can starve a background thread for well over a
/// second. A stalled flusher stays stalled, so waiting a few seconds longer to
/// say so costs nothing, while a false "unhealthy" costs an operator a page.
const MIN_HEARTBEAT_STALENESS: Duration = Duration::from_secs(5);

/// How long the flush heartbeat may go unchanged before the WAL is considered
/// unhealthy (Issue #3798): ten flush intervals, floored at
/// [`MIN_HEARTBEAT_STALENESS`].
///
/// Pure function of the configured interval so it can be reasoned about (and
/// tested) without spawning a flush thread.
fn heartbeat_staleness_threshold(flush_interval: Duration) -> Duration {
    (flush_interval * 10).max(MIN_HEARTBEAT_STALENESS)
}

// ---- flush-thread path: begin ----
//
// Everything between this marker and the matching end marker runs ON the
// background flush thread. A panic here kills the only consumer of the ring
// buffers, which is the #3798 stall itself, so the source guard
// `test_no_panicking_constructs_on_flush_thread_path` scans these regions for
// panicking constructs. Move the markers only together with the code.

/// Report a flush-thread error without ever panicking (Issue #3798).
///
/// A thin alias for the shared [`super::log_wal_diagnostic`], which carries
/// the EPIPE rationale for both this path and the group-commit lock
/// diagnostics (Issue #3798 review round 2). It stays a named function so the
/// call sites below read as flush-thread reporting rather than as generic
/// logging.
fn log_flush_error(message: &str) {
    super::log_wal_diagnostic(message);
}

/// Microseconds since a process-start baseline, on the MONOTONIC clock.
/// Used to timestamp flusher heartbeats (Issue #3798).
///
/// Deliberately not the wallclock: heartbeat staleness is a duration measured
/// across a window that an NTP step or a VM suspend/resume can move. A step
/// forward would report a perfectly healthy flusher as dead; a step backward
/// would make the elapsed time read as zero and mask a genuinely dead one --
/// exactly the case the heartbeat exists to catch. `Instant` cannot go
/// backwards, so neither can this.
fn monotonic_micros() -> u64 {
    static BASELINE: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    BASELINE
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_micros() as u64
}

/// Helper struct to encapsulate background flush logic.
struct BackgroundFlusher {
    wal: Arc<ConcurrentWal>,
    coordinator: Arc<FlushCoordinator>,
    shutdown: Arc<AtomicBool>,
    flush_notifier: Arc<FlushNotifier>,
    group_commit: Option<Arc<GroupCommitCoordinator>>,
    error_counter: Arc<AtomicU64>,
    interval: Duration,
    sync_on_flush: bool,
    /// Completed loop iterations, published so callers can prove this thread
    /// is still alive (Issue #3798).
    flush_heartbeat: Arc<AtomicU64>,
    /// Monotonic micros of the most recent heartbeat (Issue #3798).
    last_beat_micros: Arc<AtomicU64>,
    /// Non-poison coordinator errors survived instead of dying (Issue #3798).
    flush_cycle_errors: Arc<AtomicU64>,
    /// Test-only error injection for the flush cycle (Issue #3798). Held
    /// unconditionally so construction is cfg-uniform; only consulted under
    /// `#[cfg(test)]`.
    #[allow(dead_code)]
    test_inject_cycle_error: Arc<AtomicBool>,
    /// Epochs that `start_flush` opened but `finish_flush` could not close,
    /// oldest first, retried at the top of every subsequent cycle
    /// (Issue #3798 review round).
    ///
    /// Dropping such an epoch freezes the coordinator's durability frontier
    /// PERMANENTLY: `finish_flush` advances `flushed_epoch` only over a
    /// contiguous run of completed epochs, so an epoch that never completes is
    /// a wall no later flush can climb, and every committer then waits out its
    /// full deadlock-detection timeout, forever. The outcome is stored as
    /// `None` = success / `Some(message)` = the flush error to deliver, since
    /// `Error` is not `Clone`.
    ///
    /// A QUEUE, not a single slot (Issue #3798 review round 2): a slot forced
    /// `mark_group_commit_flushed` to refuse to open a new epoch while one was
    /// outstanding, which DISCARDED that cycle's outcome — including a FAILED
    /// disk flush. The failure then reached nobody, and a later cycle
    /// published a clean success for the very epoch whose entries never
    /// reached disk: an acknowledged-but-lost commit. An outcome is never
    /// dropped now; at worst it is deferred.
    ///
    /// `RefCell` rather than a lock: the flusher is owned by one thread and
    /// never shared.
    pending_finishes: std::cell::RefCell<std::collections::VecDeque<(u64, Option<String>)>>,
    /// A FAILING outcome whose epoch could not even be OPENED, carried into
    /// the next epoch that does open (Issue #3798 review round 2).
    ///
    /// A refused `start_flush` advances nothing, so `current_epoch` still
    /// covers exactly the transactions whose data was just lost — the next
    /// successfully opened epoch IS their epoch, and must report the failure
    /// rather than a clean success. Stored as a message for the same
    /// `Error: !Clone` reason as above; the FIRST such failure wins, since it
    /// is the original loss.
    carried_failure: std::cell::RefCell<Option<String>>,
}

/// How many un-finished epochs the flusher will hold before it stops trying
/// (Issue #3798 review round 2).
///
/// Reaching this depth means `finish_flush` has been refused on ~1024
/// consecutive cycles — the coordinator lock has been unusable for minutes and
/// no waiter has been released in all that time. Neither of the alternatives
/// is defensible: dropping the oldest entry silently destroys the durability
/// outcome this queue exists to preserve, and growing without bound converts a
/// wedged lock into an OOM. So the flusher fails fast the same way a poisoned
/// coordinator lock does, with a message that names the real fault.
const MAX_PENDING_FINISHES: usize = 1024;

impl BackgroundFlusher {
    fn run(&self) {
        while !self.shutdown.load(Ordering::Relaxed) {
            self.perform_flush_cycle();
            // Publish liveness once per COMPLETED cycle, before parking
            // (Issue #3798). Bumping here and not after the wait is what makes
            // a wedged flusher observable: a thread asleep in `wait_timeout`
            // for an hour stops publishing, so its heartbeat goes stale while
            // a healthy flusher's advances every interval.
            self.flush_heartbeat.fetch_add(1, Ordering::Relaxed);
            self.last_beat_micros
                .store(monotonic_micros(), Ordering::Relaxed);
            // Wait for flush interval OR immediate signal (batch full)
            self.flush_notifier.wait_timeout(self.interval);
        }
        self.perform_final_flush();
    }

    /// React to a `GroupCommitCoordinator` error raised on the flush thread
    /// (Issue #3798).
    ///
    /// LOCK POISONING: a poisoned coordinator lock means a thread died holding
    /// it and the system is unrecoverable; failing fast is correct, because
    /// continuing would leave waiting transactions hanging indefinitely.
    ///
    /// EVERY OTHER ERROR is transient. Tearing the flush thread down over one
    /// is what converts a momentary coordinator hiccup into a permanent stall:
    /// nothing drains the ring buffers afterwards, so writers pile up behind a
    /// full buffer forever. Count it, log it, skip this cycle, and let the next
    /// interval retry; waiters remain protected by `wait_for_flush`'s own
    /// timeout.
    ///
    /// "Let the next interval retry" is only true because the errors that are
    /// NOT self-healing are retained rather than skipped: a `finish_flush`
    /// that leaves an epoch open is queued and re-driven by
    /// [`Self::drain_pending_finishes`] (skipping it would freeze the
    /// durability frontier for the life of the process), and a failing
    /// *outcome* whose `start_flush` was refused is carried to the next epoch
    /// that opens (skipping it would report a lost transaction as durable).
    fn note_cycle_error(&self, what: &str, err: &Error) {
        if let Error::Storage(StorageError::LockPoisoned { .. }) = err {
            panic!(
                "GroupCommitCoordinator lock poisoned - flush thread cannot continue \
                 (while {}: {})",
                what, err
            );
        }

        self.flush_cycle_errors.fetch_add(1, Ordering::Relaxed);
        log_flush_error(&format!(
            "WAL flush cycle skipped: {} failed ({}); retrying next interval",
            what, err
        ));
    }

    /// Retry every `finish_flush` a previous cycle could not deliver, oldest
    /// first.
    ///
    /// Runs FIRST in every cycle: the coordinator advances its durability
    /// frontier only over a contiguous epoch run, so a stranded epoch must
    /// land before the frontier can climb past it. Draining stops at the first
    /// entry that still fails, which preserves FIFO order — a later epoch must
    /// never be delivered while an older one is still queued behind it, or the
    /// queue would reorder outcomes relative to the epochs they belong to.
    ///
    /// WHY THIS IS SOUND (verified by reading `GroupCommitCoordinator::
    /// finish_flush`, not assumed):
    ///
    /// 1. **Late/out-of-order completion is already handled.** `finish_flush`
    ///    inserts the epoch into a `completed_epochs` `BTreeSet` and then
    ///    advances `flushed_epoch` only over the contiguous run starting at
    ///    `flushed_epoch + 1`. Finishing 6 before 5 therefore parks 6 in the
    ///    set until 5 lands, at which point both are released in one sweep.
    /// 2. **An error is recorded against ITS OWN epoch.** `finish_flush`
    ///    pushes `(epoch, message)` onto `recent_errors`, and `wait_for_flush`
    ///    scans that list for its own epoch number. Arrival order does not
    ///    enter into either side, so deferring delivery never misattributes an
    ///    outcome to a different epoch than the one it came from.
    ///
    /// SCOPE OF THAT GUARANTEE — it is about *epoch* attribution, not about
    /// which transactions an epoch's entries belong to. A transaction is a
    /// member of the epoch it REGISTERS into, and it appends its frame to the
    /// ring buffer strictly before it registers (`log_operations_to_wal` runs
    /// ahead of `commit()` in `api::transaction::write`). A flush cycle that
    /// lands inside that append→register window drains the frame into epoch
    /// `X` while the transaction goes on to register into `X + 1`, so if that
    /// cycle FAILS the error is recorded — correctly, per the above — against
    /// `X`, which the transaction is not waiting on. Epoch membership is
    /// therefore not the same relation as flush membership, and the deferral
    /// machinery here neither creates nor closes that window; see
    /// `ConcurrentWalSystem::commit`'s "Race Condition Handling" for the full
    /// statement of what remains unguarded.
    ///
    /// The flusher is the only caller of `start_flush` / `finish_flush`, so
    /// retrying here cannot race anyone.
    ///
    /// Returns `true` when nothing is outstanding any more.
    fn drain_pending_finishes(&self) -> bool {
        let Some(gc) = self.group_commit.as_ref() else {
            return true;
        };

        loop {
            // Cloned out in its own scope so the borrow ends before anything
            // else runs: `note_cycle_error` below can panic, and a `RefCell`
            // borrow held across it (or across any future re-entry into this
            // flusher) is how a clean fail-fast becomes a confusing
            // already-borrowed panic instead.
            let next = { self.pending_finishes.borrow().front().cloned() };
            let Some((epoch, stored)) = next else {
                return true;
            };

            let outcome = match &stored {
                None => Ok(()),
                Some(message) => Err(crate::core::error::Error::other(message.clone())),
            };
            match gc.finish_flush(epoch, outcome) {
                Ok(()) => {
                    self.pending_finishes.borrow_mut().pop_front();
                }
                Err(err) => {
                    self.note_cycle_error("retrying the deferred group-commit finish_flush", &err);
                    return false;
                }
            }
        }
    }

    /// Advance the group-commit epoch for this cycle and hand `outcome` to its
    /// waiters.
    ///
    /// Split into explicit `start_flush` / `finish_flush` (rather than the
    /// fused `mark_flushed`) precisely so the two failures can be told apart:
    /// a failed `start_flush` opened no epoch, while a failed `finish_flush`
    /// has already advanced `current_epoch` and MUST be retried or the
    /// frontier never moves again.
    ///
    /// THE ONE INVARIANT HERE: an outcome is never dropped. This method always
    /// tries to open an epoch and always tries to deliver `outcome` to it,
    /// even with older epochs still queued — a queued epoch defers *delivery*,
    /// it must not suppress the *outcome*. Suppressing it is what turned a
    /// failed disk flush into a silent success for those transactions
    /// (Issue #3798 review round 2).
    fn mark_group_commit_flushed(&self, outcome: Result<()>) {
        let Some(gc) = self.group_commit.as_ref() else {
            return;
        };

        let epoch = match gc.start_flush() {
            Ok(epoch) => epoch,
            Err(err) => {
                // No epoch was opened, so nothing is stranded and no waiter
                // was woken -- but a FAILING outcome still has nowhere to go,
                // and `current_epoch` did not move, so the epoch this cycle
                // would have flushed is the same one the next cycle will open.
                // Carry the failure to it rather than losing it.
                if let Err(e) = &outcome {
                    let mut carried = self.carried_failure.borrow_mut();
                    if carried.is_none() {
                        *carried = Some(e.to_string());
                    }
                }
                self.note_cycle_error("starting the group-commit flush epoch", &err);
                return;
            }
        };

        // Fold in any failure carried from a cycle whose `start_flush` was
        // refused: those transactions are covered by THIS epoch.
        let carried = self.carried_failure.borrow_mut().take();
        let outcome = match (carried, outcome) {
            (None, outcome) => outcome,
            (Some(earlier), Ok(())) => Err(crate::core::error::Error::other(earlier)),
            (Some(earlier), Err(current)) => Err(crate::core::error::Error::other(format!(
                "{earlier}; and then {current}"
            ))),
        };

        let stored = outcome.as_ref().err().map(|e| e.to_string());
        if let Err(err) = gc.finish_flush(epoch, outcome) {
            let depth = {
                let mut queue = self.pending_finishes.borrow_mut();
                queue.push_back((epoch, stored));
                queue.len()
            };
            self.note_cycle_error("finishing the group-commit flush epoch", &err);
            if depth > MAX_PENDING_FINISHES {
                // Same fail-fast reasoning as a poisoned coordinator lock: see
                // `MAX_PENDING_FINISHES` for why neither dropping nor growing
                // is an acceptable alternative.
                panic!(
                    "GroupCommitCoordinator has refused finish_flush for {} consecutive \
                     epochs (queue depth {}) - the state mutex is unusable and no \
                     transaction has reached durability in that time; flush thread cannot \
                     continue",
                    MAX_PENDING_FINISHES, depth
                );
            }
        }
    }

    fn perform_flush_cycle(&self) {
        self.drain_pending_finishes();

        let entries = self.wal.drain_all();

        // Always try to advance the epoch when there are entries OR when
        // group commit has pending transactions. A non-poison failure to read
        // the pending batch size is treated as "nothing pending" for THIS
        // cycle only (see `note_cycle_error`).
        let should_mark_flushed = !entries.is_empty()
            || self
                .group_commit
                .as_ref()
                .is_some_and(|gc| match self.current_batch_size(gc) {
                    Ok(pending) => pending > 0,
                    Err(err) => {
                        self.note_cycle_error("reading the group-commit batch size", &err);
                        false
                    }
                });

        if !entries.is_empty() {
            // Flush to coordinator
            let result = self.coordinator.flush(entries, self.sync_on_flush);
            self.handle_flush_result(result.map(|_| ()));
        } else if should_mark_flushed {
            // No entries but there are pending transactions - advance epoch anyway
            self.handle_flush_result(Ok(()));
        }
    }

    /// Read the group-commit coordinator's pending batch size.
    ///
    /// Test builds can inject a synthetic (non-poison) error here, which flows
    /// through exactly the same downstream handling a real coordinator error
    /// would take — the point being to prove the flush thread survives it
    /// (Issue #3798) without having to poison a real lock.
    fn current_batch_size(&self, gc: &GroupCommitCoordinator) -> Result<usize> {
        #[cfg(test)]
        if self.test_inject_cycle_error.swap(false, Ordering::Relaxed) {
            return Err(Error::Storage(StorageError::WalError {
                reason: "injected test error".into(),
            }));
        }
        gc.current_batch_size()
    }

    fn perform_final_flush(&self) {
        // Last chance to land a stranded epoch before this thread is gone.
        self.drain_pending_finishes();

        let entries = self.wal.drain_all();
        if !entries.is_empty() {
            let result = self.coordinator.flush(entries, true);
            self.handle_flush_result(result.map(|_| ()));
        }
    }

    fn handle_flush_result(&self, result: Result<()>) {
        match result {
            Ok(_) => {
                // Reset error counter on success
                self.error_counter.store(0, Ordering::Relaxed);
                self.mark_group_commit_flushed(Ok(()));
            }
            Err(e) => {
                // Track consecutive errors for health monitoring
                let errors = self.error_counter.fetch_add(1, Ordering::Relaxed) + 1;
                if errors == FLUSH_ERROR_WARNING_THRESHOLD {
                    log_flush_error(&format!(
                        "CRITICAL: WAL flush failed {} consecutive times. \
                         Data durability may be compromised. Last error: {}",
                        errors, e
                    ));
                } else {
                    log_flush_error(&format!("WAL flush error: {}", e));
                }

                // The failure still has to reach this epoch's waiters, so it
                // travels the same start/finish path a success does (as a new
                // error built from the string representation).
                self.mark_group_commit_flushed(Err(crate::core::error::Error::other(
                    e.to_string(),
                )));
            }
        }
    }
}

// ---- flush-thread path: end ----

/// Unified concurrent WAL system.
///
/// This combines the striped concurrent WAL with the flush coordinator
/// and a background flush thread to provide a complete WAL solution.
pub struct ConcurrentWalSystem {
    /// The concurrent WAL with striped buffers.
    wal: Arc<ConcurrentWal>,
    /// The flush coordinator for segment management.
    coordinator: Arc<FlushCoordinator>,
    /// Handle to the background flush thread.
    flush_thread: Option<JoinHandle<()>>,
    /// Signal to stop the flush thread.
    shutdown_signal: Arc<AtomicBool>,
    /// Signal to wake up flush thread immediately (batch full).
    flush_notifier: Arc<FlushNotifier>,
    /// Durability mode.
    durability_mode: DurabilityMode,
    /// Group commit coordinator for epoch-based waiting (GroupCommit mode only).
    group_commit: Option<Arc<GroupCommitCoordinator>>,
    /// Counter for consecutive flush errors (for health monitoring).
    consecutive_flush_errors: Arc<AtomicU64>,
    /// Completed background-flusher loop iterations (Issue #3798). A strictly
    /// increasing value proves the flush thread is alive; a frozen one is the
    /// signal that writers are about to pile up behind a full ring buffer.
    flush_heartbeat: Arc<AtomicU64>,
    /// MONOTONIC micros of the most recent heartbeat — the [`monotonic_micros`]
    /// process-start `Instant` baseline, never the wallclock — seeded at
    /// construction so startup is a grace period rather than an instant
    /// staleness trip (Issue #3798).
    ///
    /// It must never become a wallclock reading: staleness is a duration
    /// measured across a window an NTP step or a VM suspend/resume can move.
    /// A step forward would report a healthy flusher as dead; a step backward
    /// would read the elapsed time as zero and mask a genuinely dead one —
    /// exactly the case the heartbeat exists to catch.
    last_beat_micros: Arc<AtomicU64>,
    /// Non-poison flush-cycle errors the flusher skipped instead of dying
    /// (Issue #3798).
    flush_cycle_errors: Arc<AtomicU64>,
    /// Configured background-flush interval, retained so the heartbeat
    /// staleness threshold scales with it (Issue #3798).
    flush_interval: Duration,
    /// Test-only staleness override in micros (`0` = use the computed
    /// threshold), so liveness tests need not wait real seconds.
    health_staleness_override_micros: AtomicU64,
    /// Test-only flush-cycle error injection, shared with the
    /// [`BackgroundFlusher`] (Issue #3798).
    #[allow(dead_code)] // Written by tests, read by the flusher under cfg(test).
    test_inject_cycle_error: Arc<AtomicBool>,
    /// Crash-torn-tail recovery policy applied by [`Self::read_from`]
    /// (Issue #3433).
    tolerate_torn_tail: bool,
    /// Optional WAL DEK keyring for encryption at rest, held in a runtime-swappable
    /// presence cell (Issues #3617, #3616). Retained so [`Self::read_from`] can
    /// decrypt encrypted segments during recovery replay — dispatching per segment
    /// on its `key_version` so a mixed old-DEK/new-DEK directory (an in-flight
    /// full-MEK rotation) replays correctly.
    ///
    /// The cell is a single `Arc<ArcSwapOption<..>>` **shared** (Arc-cloned) with
    /// the flush coordinator's config, so a runtime install (`None` → `Some`, Issue
    /// #3616 PR2) or a rotation's inner-generation advance (Issue #3617) is observed
    /// by both the write path (flush) and the recovery/`is_encrypted` path at once.
    /// Off-lock readers (`read_from`, `is_encrypted`, `wal_keyring`) `load()` it
    /// lock-free; the write path's reads stay serialized under the coordinator
    /// `writer` mutex, and the install stores into it inside the seal→reopen
    /// hand-off (see [`Self::install_wal_keyring`]).
    wal_keyring: Arc<ArcSwapOption<crate::encryption::wal_encryption::WalKeyring>>,
    /// Serializes runtime keyring installs (Issue #3616 PR2) so two concurrent
    /// installers cannot both observe the cell as `None`, both pass the presence
    /// check, and both run seal->store->reopen -- the second silently replacing
    /// the first keyring and producing an undecryptable segment. This mutex is
    /// held for the entire [`Self::install_wal_keyring`] body (presence check +
    /// seal + store + reopen) so a second concurrent installer blocks, then
    /// observes `Some`, and returns the existing rejection `Err`.
    ///
    /// Why a dedicated field: it is a private LEAF, taken only at the very top of
    /// install. The hot path (`append`/`flush`) never touches it, so it adds zero
    /// steady-state overhead; and because install itself never calls into
    /// `historical`/`current_timestamp`/cold, it is never held across any lock
    /// ordered after `wal` -- it merely wraps the presence check and the
    /// coordinator-`writer`-guarded seal->store->reopen hand-off.
    #[cfg(not(target_arch = "wasm32"))]
    install_lock: Mutex<()>,
}

impl ConcurrentWalSystem {
    /// Create a new concurrent WAL system.
    pub fn new(config: ConcurrentWalSystemConfig) -> Result<Self> {
        // Create ConcurrentWal config
        let wal_config = ConcurrentWalConfig {
            wal_dir: config.wal_dir.clone(),
            num_stripes: config.num_stripes,
            stripe_capacity: config.stripe_capacity,
            segment_size: config.segment_size,
            segments_to_retain: config.segments_to_retain,
            max_append_block_ms: config.max_append_block_ms,
        };

        // Build the WAL DEK keyring from the configured cipher (Issue #3617). A
        // never-rotated encrypted WAL starts as a single-generation keyring
        // (stamps INITIAL_WAL_KEY_VERSION, decrypts any segment); a full-MEK
        // rotation later advances it to a second generation. Plaintext WALs have
        // no keyring.
        //
        // Issue #488 version-provisioning: when the durable `open()` path
        // resolves a provisioned key version from durable on-disk state, build
        // the keyring at that version (still `match_any`, so mixed/legacy
        // segments decrypt exactly as before) so new segments stamp the real
        // version and `current_version` reports it. `None` reproduces prior
        // behavior exactly.
        let initial_keyring = config.wal_cipher.clone().map(|cipher| {
            use crate::encryption::wal_encryption::WalKeyring;
            match config.wal_key_version {
                Some(v) => WalKeyring::single_versioned(cipher, v),
                None => WalKeyring::single(cipher),
            }
        });
        // One shared presence cell, cloned (Arc-cloned, same underlying cell) into
        // both the system and the coordinator so a runtime install or rotation is
        // observed by both the flush and recovery paths (Issues #3616, #3617).
        let wal_keyring = Arc::new(ArcSwapOption::from(initial_keyring.map(Arc::new)));

        // Create FlushCoordinator config
        let coordinator_config = FlushCoordinatorConfig {
            wal_dir: config.wal_dir,
            segment_size: config.segment_size,
            segments_to_retain: config.segments_to_retain,
            flush_interval_ms: config.flush_interval_ms,
            sync_on_flush: matches!(
                config.durability_mode,
                DurabilityMode::Synchronous | DurabilityMode::GroupCommit { .. }
            ),
            write_buffer_size: config.write_buffer_size,
            wal_keyring: Arc::clone(&wal_keyring),
        };

        // Who drains the handle-less append paths depends on the durability
        // mode, and this is the only place that knows it (Issue #3798 review
        // round 2). The real write-transaction path uses the async batch
        // append in EVERY mode, and `Synchronous` starts no flush thread, so
        // without this a Synchronous-mode stall blamed a flusher that does not
        // exist and pointed at `is_healthy()`, which that mode reports as
        // `true` by construction.
        let mut wal_inner = ConcurrentWal::new(wal_config)?;
        wal_inner.set_async_drainer(match config.durability_mode {
            DurabilityMode::Synchronous => AppendDrainer::CallingThread,
            _ => AppendDrainer::BackgroundFlusher,
        });
        let wal = Arc::new(wal_inner);
        let coordinator = Arc::new(FlushCoordinator::new(coordinator_config)?);
        let shutdown_signal = Arc::new(AtomicBool::new(false));

        // Create group commit coordinator for modes that need epoch tracking
        let group_commit = match config.durability_mode {
            DurabilityMode::GroupCommit {
                max_batch_size,
                max_delay_ms,
            } => Some(Arc::new(GroupCommitCoordinator::with_config(
                // `with_config`, not `new`: `new` discards everything but the
                // two batching knobs, which is what made the #3798
                // acquisition bound unreachable from any user config.
                GroupCommitConfig {
                    max_delay_ms,
                    max_batch_size,
                    acquire_timeout_ms: config.acquire_timeout_ms,
                    ..GroupCommitConfig::default()
                },
            ))),
            DurabilityMode::AsyncBatched {
                max_batch_size,
                max_delay_ms,
                ..
            } => Some(Arc::new(GroupCommitCoordinator::with_config(
                GroupCommitConfig {
                    max_delay_ms,
                    max_batch_size,
                    acquire_timeout_ms: config.acquire_timeout_ms,
                    ..GroupCommitConfig::default()
                },
            ))),
            _ => None,
        };

        // Create flush notifier for batch-size-triggered flushes
        let flush_notifier = Arc::new(FlushNotifier::new());

        // Create error counter for health monitoring
        let consecutive_flush_errors = Arc::new(AtomicU64::new(0));

        // Flusher liveness signals (Issue #3798). `last_beat_micros` starts at
        // construction time so a freshly started system has a full staleness
        // window of grace before it can be judged stalled.
        let flush_heartbeat = Arc::new(AtomicU64::new(0));
        let last_beat_micros = Arc::new(AtomicU64::new(monotonic_micros()));
        let flush_cycle_errors = Arc::new(AtomicU64::new(0));
        let test_inject_cycle_error = Arc::new(AtomicBool::new(false));
        let flush_interval = Duration::from_millis(config.flush_interval_ms);

        // Start background flush thread for async/group-commit modes
        // ---- flush-thread path: begin ----
        let flush_thread = if matches!(
            config.durability_mode,
            DurabilityMode::Async { .. }
                | DurabilityMode::GroupCommit { .. }
                | DurabilityMode::AsyncBatched { .. }
        ) {
            let wal_clone = Arc::clone(&wal);
            let coordinator_clone = Arc::clone(&coordinator);
            let shutdown_clone = Arc::clone(&shutdown_signal);
            let flush_notifier_clone = Arc::clone(&flush_notifier);
            let group_commit_clone = group_commit.clone();
            let error_counter_clone = Arc::clone(&consecutive_flush_errors);
            let heartbeat_clone = Arc::clone(&flush_heartbeat);
            let last_beat_clone = Arc::clone(&last_beat_micros);
            let cycle_errors_clone = Arc::clone(&flush_cycle_errors);
            let inject_clone = Arc::clone(&test_inject_cycle_error);
            let sync_on_flush =
                matches!(config.durability_mode, DurabilityMode::GroupCommit { .. });

            Some(thread::spawn(move || {
                Self::flush_loop(
                    wal_clone,
                    coordinator_clone,
                    shutdown_clone,
                    flush_notifier_clone,
                    group_commit_clone,
                    error_counter_clone,
                    flush_interval,
                    sync_on_flush,
                    heartbeat_clone,
                    last_beat_clone,
                    cycle_errors_clone,
                    inject_clone,
                );
            }))
        } else {
            None
        };
        // ---- flush-thread path: end ----

        Ok(Self {
            wal,
            coordinator,
            flush_thread,
            shutdown_signal,
            flush_notifier,
            durability_mode: config.durability_mode,
            group_commit,
            consecutive_flush_errors,
            flush_heartbeat,
            last_beat_micros,
            flush_cycle_errors,
            flush_interval,
            health_staleness_override_micros: AtomicU64::new(0),
            test_inject_cycle_error,
            tolerate_torn_tail: config.tolerate_torn_tail,
            wal_keyring,
            #[cfg(not(target_arch = "wasm32"))]
            install_lock: Mutex::new(()),
        })
    }

    // ---- flush-thread path: begin ----
    /// Background flush loop.
    ///
    /// Wakes up either when:
    /// - The flush interval expires (normal periodic flush)
    /// - The flush_notifier is signaled (batch size reached)
    /// - Shutdown is requested
    #[allow(clippy::too_many_arguments)]
    fn flush_loop(
        wal: Arc<ConcurrentWal>,
        coordinator: Arc<FlushCoordinator>,
        shutdown: Arc<AtomicBool>,
        flush_notifier: Arc<FlushNotifier>,
        group_commit: Option<Arc<GroupCommitCoordinator>>,
        error_counter: Arc<AtomicU64>,
        interval: Duration,
        sync_on_flush: bool,
        flush_heartbeat: Arc<AtomicU64>,
        last_beat_micros: Arc<AtomicU64>,
        flush_cycle_errors: Arc<AtomicU64>,
        test_inject_cycle_error: Arc<AtomicBool>,
    ) {
        let flusher = BackgroundFlusher {
            wal,
            coordinator,
            shutdown,
            flush_notifier,
            group_commit,
            error_counter,
            interval,
            sync_on_flush,
            flush_heartbeat,
            last_beat_micros,
            flush_cycle_errors,
            test_inject_cycle_error,
            pending_finishes: std::cell::RefCell::new(std::collections::VecDeque::new()),
            carried_failure: std::cell::RefCell::new(None),
        };
        flusher.run();
    }
    // ---- flush-thread path: end ----

    /// Append an operation asynchronously (fire and forget).
    ///
    /// For `DurabilityMode::Synchronous`, this blocks until durable.
    /// For other modes, this returns immediately.
    pub fn append(&self, operation: WalOperation) -> Result<LSN> {
        match self.durability_mode {
            DurabilityMode::Synchronous => self.append_sync(operation),
            DurabilityMode::Async { .. }
            | DurabilityMode::GroupCommit { .. }
            | DurabilityMode::AsyncBatched { .. } => self.append_async(operation),
        }
    }

    /// Append an operation asynchronously (returns immediately).
    ///
    /// The entry is buffered and will be flushed by the background thread.
    pub fn append_async(&self, operation: WalOperation) -> Result<LSN> {
        self.wal.append_async(operation)
    }

    /// Append a batch of operations to the buffer without flushing (returns immediately).
    ///
    /// This is the batch counterpart of [`append_async`](Self::append_async): it buffers
    /// every operation under a single atomic LSN allocation (the Issue #219 win) and lets
    /// the configured durability path flush them later. Like `append_async`, it performs
    /// **no** durability-mode branching — callers that need a flush still call
    /// [`commit`](Self::commit) afterwards, which honors the active `DurabilityMode`. This
    /// keeps the transaction commit path's behavior identical across Sync/Async/GroupCommit
    /// while routing multi-operation transactions through the efficient batch append.
    pub fn append_batch_async(&self, operations: Vec<WalOperation>) -> Result<Vec<LSN>> {
        self.wal.append_batch(operations)
    }

    /// Append an operation synchronously (waits for durability).
    ///
    /// This flushes immediately and waits for fsync.
    pub fn append_sync(&self, operation: WalOperation) -> Result<LSN> {
        let (lsn, handle) = self.wal.append_with_handle(operation)?;

        // Drain and flush immediately for sync mode
        let entries = self.wal.drain_all();
        if !entries.is_empty() {
            self.coordinator.flush(entries, true)?;
        }

        // Wait for durability
        handle.wait().map_err(|e| {
            Error::Storage(StorageError::WalError {
                reason: format!("WAL flush failed: {}", e),
            })
        })?;

        Ok(lsn)
    }

    /// Append a batch of operations efficiently.
    ///
    /// This method provides significant performance improvements for high-throughput
    /// workloads by batching multiple operations into fewer I/O operations.
    ///
    /// # Performance Benefits
    ///
    /// Compared to calling `append()` multiple times:
    /// - Single atomic LSN allocation for all operations (vs N atomic operations)
    /// - Better CPU cache locality during serialization
    /// - Reduced stripe buffer contention
    ///
    /// # Durability Behavior
    ///
    /// The durability semantics follow the configured `DurabilityMode`:
    /// - **Synchronous**: All operations are flushed and synced before returning
    /// - **Async**: Operations are buffered and flushed by background thread (eventual consistency)
    /// - **GroupCommit**: Operations are buffered and flushed by background thread (caller must wait on epoch)
    /// - **AsyncBatched**: Same as GroupCommit (operations buffered, background flush)
    ///
    /// # Arguments
    ///
    /// * `operations` - Vector of operations to append
    ///
    /// # Returns
    ///
    /// Vector of allocated LSNs in the same order as the operations.
    /// Returns an empty vector if `operations` is empty.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::storage::wal::{WalOperation, ConcurrentWalSystem};
    ///
    /// let ops = vec![
    ///     WalOperation::CreateNode { /* ... */ },
    ///     WalOperation::CreateEdge { /* ... */ },
    ///     WalOperation::UpdateNode { /* ... */ },
    /// ];
    ///
    /// // Efficient batch append
    /// let lsns = wal.append_batch(ops)?;
    /// assert_eq!(lsns.len(), 3);
    ///
    /// // For GroupCommit mode, commit and wait
    /// if let Some(epoch) = wal.commit()? {
    ///     wal.group_commit_coordinator().unwrap().wait_for_flush(epoch)?;
    /// }
    /// ```
    pub fn append_batch(&self, operations: Vec<WalOperation>) -> Result<Vec<LSN>> {
        // Handle empty batch early
        if operations.is_empty() {
            return Ok(Vec::new());
        }

        // Use the underlying WAL's batch append for async modes
        match self.durability_mode {
            DurabilityMode::Synchronous => {
                // For synchronous mode, append batch then flush all
                let (lsns, handles) = self.wal.append_batch_with_handles(operations)?;

                // Drain and flush immediately for sync mode
                let entries = self.wal.drain_all();
                if !entries.is_empty() {
                    self.coordinator.flush(entries, true).map_err(|e| {
                        Error::Storage(StorageError::WalError {
                            reason: format!("Failed to flush batch after drain: {}", e),
                        })
                    })?;
                }

                // Wait for all handles to ensure durability.
                // Note: Since flush coordinator preserves LSN order, waiting for the last one
                // technically implies all previous ones are done, but waiting for all is safer
                // against future changes and handles errors correctly.
                if let Some(last_handle) = handles.into_iter().last() {
                    last_handle.wait().map_err(|e| {
                        Error::Storage(StorageError::WalError {
                            reason: format!("WAL flush failed: {}", e),
                        })
                    })?;
                }

                Ok(lsns)
            }
            DurabilityMode::Async { .. }
            | DurabilityMode::GroupCommit { .. }
            | DurabilityMode::AsyncBatched { .. } => {
                // For async modes, just batch append (background thread handles flush)
                self.wal.append_batch(operations)
            }
        }
    }

    /// Force a flush of all pending entries.
    pub fn flush(&self) -> Result<FlushStats> {
        let entries = self.wal.drain_all();
        if entries.is_empty() {
            return Ok(FlushStats::default());
        }

        let should_sync = !matches!(self.durability_mode, DurabilityMode::Async { .. });
        self.coordinator.flush(entries, should_sync)
    }

    /// Commit with the configured durability mode.
    ///
    /// # Usage
    ///
    /// **Important**: All `append_async()` calls for a transaction MUST complete
    /// before calling `commit()`. The typical pattern is:
    ///
    /// ```ignore
    /// wal.append_async(op1)?;
    /// wal.append_async(op2)?;
    /// let epoch = wal.commit()?;  // Register for durability
    /// if let Some(epoch) = epoch {
    ///     wal.group_commit_coordinator().unwrap().wait_for_flush(epoch)?;
    /// }
    /// ```
    ///
    /// # Returns
    ///
    /// Returns an epoch number for GroupCommit/AsyncBatched modes that the
    /// caller should wait on using `group_commit_coordinator().wait_for_flush(epoch)`.
    ///
    /// For other modes, returns `None`:
    /// - Synchronous: Data is already durable when this returns
    /// - Async: No waiting needed (fire-and-forget)
    ///
    /// # Race Condition Handling
    ///
    /// In GroupCommit mode, there's an intentional race between:
    /// 1. The flush thread draining entries
    /// 2. Transactions calling `register_transaction()`
    ///
    /// A caller appends its frames to the ring buffer strictly BEFORE it calls
    /// this method, so a flush cycle can drain those frames into an earlier
    /// epoch than the one the caller then registers into. On the SUCCESS path
    /// that is harmless: the drained epoch advances (possibly with no
    /// registered members), the caller's own epoch completes behind it, and by
    /// the time `wait_for_flush` returns the frames really are on disk —
    /// entries must be in the ring buffer before this method is called, and a
    /// later epoch cannot complete before an earlier one (`flushed_epoch`
    /// advances only over a contiguous run).
    ///
    /// KNOWN GAP — a FAILING flush inside that window is NOT attributed to the
    /// caller (Issue #3798 review round 3, tracked separately). If the cycle
    /// that drained the frames fails, its entries are dropped rather than
    /// re-queued and the error is recorded against the epoch that drained them
    /// — not against the epoch the transaction subsequently registers into,
    /// whose own (entry-free) flush then succeeds. Such a transaction can
    /// therefore see `wait_for_flush` return `Ok` for frames that never reached
    /// disk. The window is exactly append→register: once a transaction is
    /// registered, every outcome for its epoch reaches it (see
    /// `drain_pending_finishes`). Closing it needs the epoch to be claimed
    /// before the frames are appended, or the flush to re-queue what it could
    /// not write; neither is done here.
    ///
    /// `FlushCoordinator::get_max_flushed_lsn` cannot be used to detect this
    /// after the fact: it is a telemetry approximation, not a durability
    /// watermark. It is advanced by `fetch_max` BEFORE the fsync and is not
    /// rolled back when that fsync fails and the segment is truncated, and
    /// because LSNs are allocated before their entries are placed into a
    /// stripe, a drained batch may skip a lower LSN still in flight — so the
    /// value is monotonic but neither durability-gated nor contiguous.
    pub fn commit(&self) -> Result<Option<u64>> {
        match self.durability_mode {
            DurabilityMode::Synchronous => {
                // Drain and flush immediately with fsync
                let entries = self.wal.drain_all();
                if !entries.is_empty() {
                    self.coordinator.flush(entries, true)?;
                }
                Ok(None)
            }
            DurabilityMode::Async { .. } => {
                // Just let background thread handle it
                Ok(None)
            }
            DurabilityMode::GroupCommit { .. } | DurabilityMode::AsyncBatched { .. } => {
                // Register with coordinator and return epoch to wait for
                if let Some(ref gc) = self.group_commit {
                    let (epoch, should_trigger) = gc.register_transaction()?;

                    // If batch is full, signal flush thread to wake up immediately
                    if should_trigger {
                        self.flush_notifier.notify();
                    }

                    Ok(Some(epoch))
                } else {
                    // Fallback to sync if no coordinator (shouldn't happen)
                    let entries = self.wal.drain_all();
                    if !entries.is_empty() {
                        self.coordinator.flush(entries, true)?;
                    }
                    Ok(None)
                }
            }
        }
    }

    /// Get the group commit coordinator for waiting on epochs.
    ///
    /// Returns `None` for modes that don't use group commit.
    pub fn group_commit_coordinator(&self) -> Option<&Arc<GroupCommitCoordinator>> {
        self.group_commit.as_ref()
    }

    /// Get the current (next to be allocated) LSN.
    pub fn current_lsn(&self) -> LSN {
        self.wal.current_lsn()
    }

    /// Set the next LSN to allocate.
    ///
    /// **Warning**: Recovery-only (Issue #3420). Call this during startup —
    /// before any write is accepted — to seed the allocator past every LSN
    /// already durable on disk (WAL segments and/or index manifest). Calling
    /// it during normal operation will cause duplicate LSNs.
    ///
    /// Hardened after review (PR #3428): `pub(crate)` so external users
    /// cannot corrupt allocator state, a no-op when it would move the
    /// allocator BACKWARDS (the underlying allocator uses `fetch_max`
    /// semantics), and a debug assertion that no appends have happened yet.
    pub(crate) fn set_next_lsn(&self, lsn: LSN) {
        debug_assert_eq!(
            self.total_appends(),
            0,
            "set_next_lsn is recovery-only and must run before any append"
        );
        self.wal.set_next_lsn(lsn);
    }

    /// Get total entries appended.
    pub fn total_appends(&self) -> u64 {
        self.wal.total_appends()
    }

    /// The minimum LSN still available to the replication feed (Issue
    /// #3355): `None` when no segment carries metadata AND nothing has been
    /// truncated this process lifetime (a fresh/empty WAL dir), in which
    /// case every LSN is still "available". Truncation-aware: once
    /// `truncate_to_lsn` has removed every sealed segment, this reports the
    /// truncation watermark + 1 rather than `None` (see
    /// [`FlushCoordinator::get_replication_min_lsn`]).
    ///
    /// Used by the replication feed to detect when a requested resume LSN
    /// has fallen behind retention/truncation (`truncate_to_lsn`) and must
    /// report `ResyncRequired` instead of silently skipping a gap.
    pub(crate) fn min_available_lsn(&self) -> Option<LSN> {
        self.coordinator.get_replication_min_lsn()
    }

    /// Best-known maximum LSN actually written to disk (Issue #3355): see
    /// [`FlushCoordinator::get_max_flushed_lsn`]. Used by the replication
    /// feed to report `primary_flushed_lsn` for lag observability.
    pub(crate) fn max_flushed_lsn(&self) -> Option<LSN> {
        self.coordinator.get_max_flushed_lsn()
    }

    /// Get total entries flushed to disk.
    pub fn total_flushed(&self) -> u64 {
        self.coordinator.total_entries_flushed()
    }

    /// Get the durability mode.
    pub fn durability_mode(&self) -> DurabilityMode {
        self.durability_mode
    }

    /// Get the number of consecutive flush errors.
    ///
    /// This can be used for health monitoring. A value > 0 indicates
    /// that the last flush(es) failed. A value >= 3 indicates a critical
    /// condition where data durability may be compromised.
    ///
    /// The counter resets to 0 after a successful flush.
    pub fn consecutive_flush_errors(&self) -> u64 {
        self.consecutive_flush_errors.load(Ordering::Relaxed)
    }

    /// Completed background-flusher loop iterations (Issue #3798).
    ///
    /// Strictly increasing while the flush thread is alive and scheduling; a
    /// value that stops advancing is the observable signal that the flusher
    /// died or wedged, and therefore that writers will soon block on full ring
    /// buffers. Always `0` for durability modes that run no flush thread
    /// (e.g. [`DurabilityMode::Synchronous`]).
    pub fn flush_heartbeat(&self) -> u64 {
        self.flush_heartbeat.load(Ordering::Relaxed)
    }

    /// Non-poison flush-cycle errors the background flusher survived
    /// (Issue #3798).
    ///
    /// Unlike [`Self::consecutive_flush_errors`] this never resets: it counts,
    /// over the process lifetime, how many times a cycle was skipped instead
    /// of taking down the flush thread.
    pub fn flush_cycle_errors(&self) -> u64 {
        self.flush_cycle_errors.load(Ordering::Relaxed)
    }

    /// Override the heartbeat staleness threshold (micros; `0` = use the
    /// computed one). Test-only, so liveness assertions need not wait seconds.
    #[cfg(test)]
    pub(crate) fn set_health_staleness_override_micros(&self, micros: u64) {
        self.health_staleness_override_micros
            .store(micros, Ordering::Relaxed);
    }

    /// Whether the background flusher's heartbeat has gone stale
    /// (Issue #3798).
    ///
    /// Two deliberate non-alarms:
    ///
    /// - **No flush thread** (e.g. [`DurabilityMode::Synchronous`]): there is
    ///   no heartbeat that *could* advance, because the write path flushes
    ///   inline. Such a WAL is never stale.
    /// - **Startup**: `last_beat_micros` is seeded at construction time, so a
    ///   system that has not completed its first cycle yet still gets a full
    ///   staleness window of grace instead of tripping instantly.
    ///
    /// Both ends of the comparison come from [`monotonic_micros`], so a clock
    /// step or a suspend/resume can neither invent staleness nor hide it.
    fn heartbeat_stale(&self) -> bool {
        if self.flush_thread.is_none() {
            return false;
        }

        let threshold_micros = match self
            .health_staleness_override_micros
            .load(Ordering::Relaxed)
        {
            0 => heartbeat_staleness_threshold(self.flush_interval).as_micros() as u64,
            overridden => overridden,
        };

        let last_beat = self.last_beat_micros.load(Ordering::Relaxed);
        monotonic_micros().saturating_sub(last_beat) > threshold_micros
    }

    /// Check if the WAL is healthy.
    ///
    /// Returns `true` when the last flush succeeded **and** the background
    /// flusher is still publishing its heartbeat. A frozen heartbeat
    /// (Issue #3798) is reported as unhealthy even though no flush has
    /// errored: a flusher that died or wedged raises no error at all, it just
    /// stops draining, and the first visible symptom would otherwise be
    /// writers blocking on full ring buffers.
    pub fn is_healthy(&self) -> bool {
        self.consecutive_flush_errors() == 0 && !self.heartbeat_stale()
    }

    /// Get the WAL directory path.
    pub fn wal_dir(&self) -> &std::path::Path {
        self.coordinator.wal_dir()
    }

    /// The bound on blocking appends currently in force, in milliseconds
    /// (`0` = unbounded, Issue #3798).
    ///
    /// Exposed so an operator -- or a configuration test -- can confirm which
    /// value actually reached the WAL rather than inferring it from the config
    /// that was handed in.
    pub fn max_append_block_ms(&self) -> u64 {
        self.wal.config().max_append_block_ms
    }

    /// Whether this WAL is encrypted at rest (a WAL cipher is configured).
    ///
    /// Used by the index key-rotation cross-layer guard (Issue #488): an
    /// index-only key rotation to a new MEK must refuse while any *other* layer
    /// (WAL/checkpoint/cold) is still encrypted under the current MEK, because
    /// switching the key provider afterward would render those un-rotated files
    /// undecryptable. Never exposes the cipher or any key material.
    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn is_encrypted(&self) -> bool {
        self.wal_keyring.load().is_some()
    }

    /// The WAL DEK keyring, if this WAL is encrypted (Issues #3617, #3616).
    ///
    /// Exposed so the full-MEK rotation driver can advance the WAL DEK generation
    /// before force-rolling the active segment. Returns a clone of the shared
    /// keyring handle (a cheap `Arc` clone that shares the same inner
    /// generations), so a caller's `add_generation` is observed by every holder.
    /// Never returns key material directly — only the redacting-`Debug` handle.
    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn wal_keyring(&self) -> Option<crate::encryption::wal_encryption::WalKeyring> {
        self.wal_keyring.load_full().map(|k| (*k).clone())
    }

    /// Install a WAL DEK keyring at runtime, flipping a live plaintext WAL to
    /// encrypted (Issue #3616 PR2). This is the structural seam the plaintext →
    /// encrypted enable engine drives.
    ///
    /// # Transition
    ///
    /// Only the `None` → `Some` transition is supported here. Installing when a
    /// keyring is already present is **rejected** with a structured error (never
    /// a silent replace, so no data is lost): rotation (`Some` → `Some'`) is the
    /// job of the keyring's own [`add_generation`], not this seam.
    ///
    /// # Crash-consistency / concurrency
    ///
    /// You cannot append encrypted (v16) frames into an already-open plaintext
    /// (v13) segment, so the install runs as the `advance` closure inside the
    /// existing seal→reopen hand-off ([`Self::seal_active_segment_for_rotation`]):
    /// it drains + fsyncs in-flight ring-buffer entries into the current plaintext
    /// segment, seals it, stores the keyring into the shared presence cell, then
    /// opens a fresh segment which is now written in the encrypted keyversioned
    /// format. Because the store happens **between** seal and reopen while holding
    /// the coordinator `writer` mutex, no `flush()` can interleave: every segment's
    /// header and frames use one consistent keyring state, and every acknowledged
    /// append is preserved across the flip.
    ///
    /// The store closure obeys the same lock-order contract as a rotation
    /// `advance`: it only mutates the in-memory presence cell and must not acquire
    /// any lock ordered after `wal` (no cold flush, no `historical`,
    /// no `current_timestamp`).
    ///
    /// [`add_generation`]: crate::encryption::wal_encryption::WalKeyring::add_generation
    // PR2 (#3616) ships this structural install seam; its production consumer is
    // the plaintext → encrypted enable engine (#3616 PR3), which drives it from
    // `enable_encryption` and the startup `install_pending_enable_wal_keyring` hook.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn install_wal_keyring(
        &self,
        keyring: crate::encryption::wal_encryption::WalKeyring,
    ) -> Result<()> {
        // Serialize the ENTIRE install — presence check, seal, store, reopen —
        // under a dedicated leaf mutex. Without it, the presence check below runs
        // outside the coordinator `writer` mutex while the store happens inside the
        // seal→reopen hand-off, so two concurrent installers could both observe
        // `None`, both pass the check, and both run seal→store→reopen — the second
        // silently replacing the first keyring and producing an undecryptable
        // segment. Holding this guard for the whole body makes a second concurrent
        // installer block here, then observe `Some`, and take the rejection path.
        let _install_guard = self.install_lock.lock().unwrap_or_else(|e| e.into_inner());

        // Reject a double-install: presence is a one-way None → Some transition.
        // Checked (under the install lock) before the seal so an already-encrypted
        // WAL is never re-rolled.
        if self.wal_keyring.load().is_some() {
            // A DISTINGUISHABLE precondition variant (not a generic `WalError`) so
            // the enable engine's `map_wal_install_err` maps ONLY this to
            // FAILED_PRECONDITION and leaves genuine WAL I/O / seal faults as
            // INTERNAL (Issue #3616 PR3). MCP classifies it as FAILED_PRECONDITION.
            return Err(Error::Storage(StorageError::WalKeyringAlreadyInstalled {
                reason: "a WAL keyring is already installed; runtime install only \
                         supports the plaintext → encrypted (None → Some) transition \
                         (key rotation is a separate operation)"
                    .to_string(),
            }));
        }

        let cell = Arc::clone(&self.wal_keyring);
        let new_keyring = Arc::new(keyring);
        // Store into the shared cell strictly between sealing the plaintext
        // segment and opening the fresh (now encrypted) one, under the writer
        // mutex — the atomic hand-off point.
        self.seal_active_segment_for_rotation(move || {
            cell.store(Some(new_keyring));
        })
    }

    /// Uninstall the WAL DEK keyring at runtime, flipping a live encrypted WAL
    /// back to plaintext (Issue #3616 PR4). This is the structural seam the
    /// encrypted → plaintext DISABLE engine drives — the exact inverse of
    /// [`Self::install_wal_keyring`].
    ///
    /// # Transition
    ///
    /// Only the `Some` → `None` transition is supported here. Uninstalling when
    /// NO keyring is present is **rejected** with a structured error (never a
    /// silent no-op or a spurious segment roll): a caller cannot disable
    /// encryption on an already-plaintext WAL.
    ///
    /// # Crash-consistency / concurrency
    ///
    /// Symmetric to install: you cannot append plaintext (v13) frames into an
    /// already-open encrypted (v16) segment, so the uninstall runs as the
    /// `advance` closure inside the same seal→reopen hand-off
    /// ([`Self::seal_active_segment_for_rotation`]): it drains + fsyncs in-flight
    /// ring-buffer entries into the current encrypted segment, seals it, stores
    /// `None` into the shared presence cell, then opens a fresh segment which is
    /// now written in the plaintext string-label format. Because the store
    /// happens **between** seal and reopen while holding the coordinator `writer`
    /// mutex, no `flush()` can interleave: every segment's header and frames use
    /// one consistent keyring state, and every acknowledged append is preserved
    /// across the flip.
    ///
    /// # Read capability after uninstall
    ///
    /// Dropping the keyring removes read-decrypt capability from the live cell,
    /// so [`Self::read_from`] (which snapshots the cell) can no longer decode the
    /// pre-uninstall encrypted segments still on disk. This is expected and
    /// mirrors the enable engine's inverse concern (its retired plaintext
    /// segments must be *deleted* for security): the DISABLE driver captures a
    /// plaintext index snapshot of all pre-uninstall state, then RETIRES those
    /// encrypted segments, so a reopen reconstructs from the plaintext snapshot
    /// and replays only the fresh plaintext segment. The retire is a separate
    /// driver step (mirroring [`install_wal_keyring`]'s retire being separate),
    /// not part of this seam.
    ///
    /// The store closure obeys the same lock-order contract as a rotation
    /// `advance`: it only mutates the in-memory presence cell and must not
    /// acquire any lock ordered after `wal`.
    ///
    /// [`install_wal_keyring`]: Self::install_wal_keyring
    // PR4 (#3616) ships this structural uninstall seam; its production consumer is
    // the encrypted → plaintext disable engine (#3616 PR4), which drives it from
    // `disable_encryption` and the startup disable-resume hook.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn uninstall_wal_keyring(&self) -> Result<()> {
        // Serialize the ENTIRE uninstall — presence check, seal, store, reopen —
        // under the same dedicated leaf mutex install uses. Without it, two
        // concurrent uninstallers could both observe `Some`, both pass the check,
        // and both run seal→store→reopen — the second sealing an already-plaintext
        // segment spuriously. Holding this guard for the whole body makes a second
        // concurrent uninstaller block here, then observe `None`, and take the
        // rejection path. It also serializes install against uninstall (both take
        // this same leaf), so the presence transition is never torn.
        let _install_guard = self.install_lock.lock().unwrap_or_else(|e| e.into_inner());

        // Reject when there is nothing to uninstall: presence must be `Some` for
        // this one-way Some → None transition. Checked (under the install lock)
        // before the seal so an already-plaintext WAL is never re-rolled.
        if self.wal_keyring.load().is_none() {
            // A DISTINGUISHABLE precondition variant (not a generic `WalError`) so
            // the disable engine maps ONLY this to FAILED_PRECONDITION and leaves
            // genuine WAL I/O / seal faults as INTERNAL (Issue #3616 PR4). MCP
            // classifies it as FAILED_PRECONDITION.
            return Err(Error::Storage(StorageError::WalKeyringNotInstalled {
                reason: "no WAL keyring is installed; runtime uninstall only supports the \
                         encrypted → plaintext (Some → None) transition (a plaintext WAL is \
                         already un-encrypted)"
                    .to_string(),
            }));
        }

        let cell = Arc::clone(&self.wal_keyring);
        // Store `None` into the shared cell strictly between sealing the encrypted
        // segment and opening the fresh (now plaintext) one, under the writer
        // mutex — the atomic hand-off point, the exact inverse of install.
        self.seal_active_segment_for_rotation(move || {
            cell.store(None);
        })
    }

    /// Whether every on-disk WAL segment is stamped with `key_version` — the
    /// rotation driver's "old generation fully retired" signal (Issue #3617).
    /// See [`FlushCoordinator::all_segments_use_key_version`].
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn all_segments_use_key_version(&self, key_version: u32) -> bool {
        self.coordinator.all_segments_use_key_version(key_version)
    }

    /// Retire (delete) every sealed old-generation WAL segment, keeping only
    /// segments stamped `keep_key_version` and the active segment (Issue #3617).
    /// See [`FlushCoordinator::retire_old_generation_segments`].
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn retire_old_generation_segments(&self, keep_key_version: u32) -> Result<usize> {
        self.coordinator
            .retire_old_generation_segments(keep_key_version)
    }

    /// Force-roll the active WAL segment for a full-MEK key rotation (Issue
    /// #3617): drain and durably flush every in-flight ring-buffer entry into
    /// the CURRENT (old-generation) segment, then atomically seal it, run
    /// `advance` (which flips the WAL keyring to the new generation), and open a
    /// fresh segment stamped with — and encrypting under — the new generation.
    ///
    /// # Quiesce / correctness
    ///
    /// Appends land in the lock-free ring buffer BEFORE any flush. This method
    /// first drains and flushes them (with fsync) into the old segment, so no
    /// acknowledged entry is lost across the roll. The subsequent seal + keyring
    /// flip + reopen all run under the coordinator `writer` mutex, which every
    /// flush also holds while it reads the write cipher and writes the segment
    /// header — so the (segment header generation, frame-encrypting generation)
    /// pair is always consistent and a batch can never straddle two generations.
    /// Any append racing the roll is simply picked up by whichever flush runs
    /// next and written, consistently, to whatever segment is current at that
    /// flush. `advance` runs while holding only the `writer` mutex, so it must
    /// not acquire any lock ordered after `wal` (no cold flush inside it).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn seal_active_segment_for_rotation<F: FnOnce()>(&self, advance: F) -> Result<()> {
        // 1. Drain + flush all pending entries into the current (old) segment,
        //    with fsync, so nothing acknowledged is left only in the ring buffer.
        let entries = self.wal.drain_all();
        if !entries.is_empty() {
            self.coordinator.flush(entries, true)?;
        }
        // 2. Seal old + flip keyring + open new, atomically under the writer mutex.
        self.coordinator.seal_active_segment_and_reopen(advance)
    }

    /// Read WAL entries from disk, starting from the specified LSN.
    ///
    /// This reads all segment files in the WAL directory and returns entries
    /// with LSN >= start_lsn. Used for recovery.
    ///
    /// Honors the configured crash-torn-tail recovery policy (Issue #3433): by
    /// default an undecodable trailing entry in the final segment stops replay
    /// there (keeping the intact prefix); with `tolerate_torn_tail = false` any
    /// parse failure hard-errors.
    pub fn read_from(&self, start_lsn: LSN) -> Result<Vec<super::WalEntry>> {
        // Thread the configured WAL cipher (if any) into the reader: encrypted
        // segments cannot be decoded without it, so an encryption-at-rest
        // database replaying its WAL tail after a crash must decrypt here.
        // Passing `None` (no encryption) preserves plaintext behavior exactly.
        // Snapshot the shared presence cell for the duration of the read. Holding
        // the guard keeps the loaded keyring alive while the reader borrows it.
        let keyring_guard = self.wal_keyring.load();
        crate::storage::wal::segment_reader::read_entries_from_dir_with_keyring(
            self.wal_dir(),
            start_lsn,
            keyring_guard.as_ref().map(|k| k.as_ref()),
            self.tolerate_torn_tail,
        )
    }

    /// Shutdown the WAL system gracefully.
    ///
    /// This signals the background thread to stop, waits for it to finish,
    /// and performs a final flush of all pending entries.
    pub fn shutdown(&mut self) {
        // Gracefully shutdown the WAL.
        // This stops accepting new writes, waits for active batches to complete,
        // and then closes the ring buffers.
        self.wal.shutdown_graceful();

        // Signal shutdown
        self.shutdown_signal.store(true, Ordering::Relaxed);

        // Wake up flush thread so it can see the shutdown signal
        self.flush_notifier.notify();

        // Wait for flush thread to finish
        if let Some(handle) = self.flush_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ConcurrentWalSystem {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GLOBAL_INTERNER;
    use crate::core::id::NodeId;
    use crate::core::property::PropertyMap;
    use crate::core::temporal::time;
    use tempfile::tempdir;

    fn create_test_operation(id: u64) -> WalOperation {
        WalOperation::CreateNode {
            node_id: NodeId::new(id).unwrap(),
            label: GLOBAL_INTERNER.intern(format!("Node{}", id)).unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
            provenance: None,
        }
    }

    // ── Issue #3617: WAL DEK force-roll (dual-generation rotation) ────────

    fn wal_test_cipher(seed: u8) -> Arc<dyn crate::encryption::cipher::Cipher> {
        use zeroize::Zeroizing;
        let key = Zeroizing::new([seed; 32]);
        Arc::new(crate::encryption::Aes256GcmCipher::new(&key))
    }

    fn encrypted_sync_config(
        dir: &std::path::Path,
        cipher: Arc<dyn crate::encryption::cipher::Cipher>,
    ) -> ConcurrentWalSystemConfig {
        let mut config = ConcurrentWalSystemConfig::new(dir);
        config.durability_mode = DurabilityMode::Synchronous;
        config.wal_cipher = Some(cipher);
        config
    }

    #[test]
    fn force_roll_writes_new_generation_and_recovers_both() {
        let dir = tempdir().unwrap();
        let old_dek = wal_test_cipher(7);
        let new_dek = wal_test_cipher(9);
        let wal = ConcurrentWalSystem::new(encrypted_sync_config(dir.path(), Arc::clone(&old_dek)))
            .unwrap();

        // Write a couple entries under the OLD generation (v16, key_version 1).
        wal.append(create_test_operation(1)).unwrap();
        wal.append(create_test_operation(2)).unwrap();

        // Force-roll: seal the old segment and start a new one under a NEW DEK
        // (key_version 2). The keyring flip runs inside the atomic hand-off.
        let ring = wal.wal_keyring().unwrap().clone();
        let advance_cipher = Arc::clone(&new_dek);
        wal.seal_active_segment_for_rotation(move || {
            ring.add_generation(2, advance_cipher);
        })
        .unwrap();

        // Subsequent appends land in the fresh, new-generation segment.
        wal.append(create_test_operation(3)).unwrap();
        wal.append(create_test_operation(4)).unwrap();
        wal.flush().unwrap();

        // Recovery via the (now two-generation) keyring recovers every entry —
        // old segments under the old DEK, new under the new.
        let entries = wal.read_from(LSN::initial()).unwrap();
        let ids: Vec<u64> = entries
            .iter()
            .filter_map(|e| match &e.operation {
                WalOperation::CreateNode { node_id, .. } => Some(node_id.as_u64()),
                _ => None,
            })
            .collect();
        assert_eq!(
            ids,
            vec![1, 2, 3, 4],
            "all entries across both generations recover"
        );
    }

    /// Hammer appends from many threads WHILE a force-roll happens: every
    /// acknowledged entry must recover (none lost) and decrypt (none unreadable),
    /// proving the seal is atomic w.r.t. concurrent appenders (Issue #3617).
    #[test]
    fn append_hammer_across_force_roll_loses_nothing() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicU64, Ordering as AtOrd};

        let dir = tempdir().unwrap();
        let old_dek = wal_test_cipher(7);
        let new_dek = wal_test_cipher(9);
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::GroupCommit {
            max_batch_size: 16,
            max_delay_ms: 2,
        };
        config.wal_cipher = Some(Arc::clone(&old_dek));
        let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

        let threads = 4u64;
        let per_thread = 200u64;
        let next_id = Arc::new(AtomicU64::new(1));
        let barrier = Arc::new(Barrier::new(threads as usize + 1));

        let mut handles = Vec::new();
        for _ in 0..threads {
            let wal = Arc::clone(&wal);
            let next_id = Arc::clone(&next_id);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..per_thread {
                    let id = next_id.fetch_add(1, AtOrd::Relaxed);
                    let (_lsn, handle) = wal
                        .wal
                        .append_with_handle(create_test_operation(id))
                        .unwrap();
                    // Ensure the entry is flushed so nothing is stuck unflushed.
                    let _ = wal.commit();
                    let _ = handle.wait();
                }
            }));
        }

        // Release the appenders, then force-roll partway through the storm.
        barrier.wait();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let ring = wal.wal_keyring().unwrap().clone();
        let advance_cipher = Arc::clone(&new_dek);
        wal.seal_active_segment_for_rotation(move || {
            ring.add_generation(2, advance_cipher);
        })
        .unwrap();

        for h in handles {
            h.join().unwrap();
        }
        wal.flush().unwrap();

        let total = threads * per_thread;
        let entries = wal.read_from(LSN::initial()).unwrap();
        let mut ids: Vec<u64> = entries
            .iter()
            .filter_map(|e| match &e.operation {
                WalOperation::CreateNode { node_id, .. } => Some(node_id.as_u64()),
                _ => None,
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len() as u64,
            total,
            "every acknowledged entry across the roll must recover (none lost, none unreadable)"
        );
        assert_eq!(*ids.first().unwrap(), 1);
        assert_eq!(*ids.last().unwrap(), total);
    }

    // ── Issue #3616 PR2: runtime WAL keyring install (plaintext → encrypted) ──

    // NOTE: `seed` is a CIPHER SEED (distinguishes one test cipher's key bytes
    // from another), NOT a key_version. `WalKeyring::single` always stamps
    // INITIAL_WAL_KEY_VERSION (== 1) as the on-disk key_version regardless of the
    // seed, which is why T3 asserts the post-install cohort is "key_version 1".
    fn wal_keyring(seed: u8) -> crate::encryption::wal_encryption::WalKeyring {
        crate::encryption::wal_encryption::WalKeyring::single(wal_test_cipher(seed))
    }

    fn recovered_ids(entries: &[crate::storage::wal::WalEntry]) -> Vec<u64> {
        entries
            .iter()
            .filter_map(|e| match &e.operation {
                WalOperation::CreateNode { node_id, .. } => Some(node_id.as_u64()),
                _ => None,
            })
            .collect()
    }

    /// T1 — None→Some transition: a fresh plaintext WAL reports `is_encrypted()
    /// == false`; after `install_wal_keyring` it reports `true`; a record
    /// appended after the install reads back correctly, and pre-install records
    /// still recover (mixed plaintext/encrypted directory).
    #[test]
    fn install_keyring_none_to_some_transition() {
        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        assert!(
            !wal.is_encrypted(),
            "a fresh plaintext WAL must not be encrypted"
        );
        wal.append(create_test_operation(1)).unwrap();
        wal.append(create_test_operation(2)).unwrap();

        wal.install_wal_keyring(wal_keyring(7)).unwrap();
        assert!(
            wal.is_encrypted(),
            "installing a keyring must flip the WAL to encrypted"
        );

        // A record appended after the install lands in the fresh encrypted
        // segment and must read back correctly.
        wal.append(create_test_operation(3)).unwrap();
        wal.flush().unwrap();

        let entries = wal.read_from(LSN::initial()).unwrap();
        assert_eq!(
            recovered_ids(&entries),
            vec![1, 2, 3],
            "pre-install plaintext and post-install encrypted records both recover in order"
        );
    }

    /// T2 — concurrent appenders during install lose nothing. N appender threads
    /// hammer while one thread installs a keyring mid-storm (mirrors
    /// `append_hammer_across_force_roll_loses_nothing`). Every acknowledged
    /// append must recover, none lost, none unreadable, no hang.
    #[test]
    fn install_keyring_during_concurrent_appends_loses_nothing() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicU64, Ordering as AtOrd};

        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::GroupCommit {
            max_batch_size: 16,
            max_delay_ms: 2,
        };
        // Starts PLAINTEXT — the install flips it to encrypted mid-storm.
        let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

        let threads = 4u64;
        let per_thread = 200u64;
        let next_id = Arc::new(AtomicU64::new(1));
        let barrier = Arc::new(Barrier::new(threads as usize + 1));

        let mut handles = Vec::new();
        for _ in 0..threads {
            let wal = Arc::clone(&wal);
            let next_id = Arc::clone(&next_id);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..per_thread {
                    let id = next_id.fetch_add(1, AtOrd::Relaxed);
                    let (_lsn, handle) = wal
                        .wal
                        .append_with_handle(create_test_operation(id))
                        .unwrap();
                    let _ = wal.commit();
                    let _ = handle.wait();
                }
            }));
        }

        // Release the appenders, then install a keyring partway through.
        barrier.wait();
        std::thread::sleep(std::time::Duration::from_millis(3));
        wal.install_wal_keyring(wal_keyring(7)).unwrap();
        assert!(wal.is_encrypted());

        for h in handles {
            h.join().unwrap();
        }
        wal.flush().unwrap();

        let total = threads * per_thread;
        // In-process recovery via the live system's shared keyring cell (same
        // in-process limitation as `append_hammer_across_force_roll_loses_nothing`
        // — a full process-restart reopen is out of scope for this seam test).
        let entries = wal.read_from(LSN::initial()).unwrap();
        let mut ids = recovered_ids(&entries);
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len() as u64,
            total,
            "every acknowledged entry across the install must recover (none lost, none unreadable)"
        );
        assert_eq!(*ids.first().unwrap(), 1);
        assert_eq!(*ids.last().unwrap(), total);
    }

    /// T3 — install-then-roll ordering / mixed-format recovery. Records written
    /// before install live in plaintext (v13) segments; records after install
    /// live in encrypted (v16) segment(s). A full recovery reads BOTH cohorts
    /// back correctly in order, and the on-disk directory really is mixed-format.
    #[test]
    fn install_keyring_mixed_format_recovery() {
        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        // Pre-install cohort → plaintext segment(s).
        for id in 1..=5 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();
        assert!(
            !wal.all_segments_use_key_version(1),
            "pre-install segments must be plaintext (no key_version stamp)"
        );

        wal.install_wal_keyring(wal_keyring(9)).unwrap();

        // Post-install cohort → encrypted v16 segment(s).
        for id in 6..=10 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();

        // The directory now holds BOTH plaintext and encrypted segments, so no
        // single key_version covers every segment.
        assert!(
            !wal.all_segments_use_key_version(1),
            "a mixed plaintext+encrypted directory is not wholly one key_version"
        );

        // POSITIVE proof the install actually produced an encrypted (v16) segment
        // on disk — not merely flipped the in-memory flag while still writing
        // plaintext. `!all_segments_use_key_version(1)` above is satisfied by the
        // surviving pre-install PLAINTEXT segments alone, so it cannot prove the
        // "mixed-format" claim; the max stamped key_version across the directory
        // being `Some(1)` requires at least one keyversioned v16 segment to exist.
        assert_eq!(
            crate::storage::wal::segment_reader::max_key_version_in_dir(dir.path()),
            Some(1),
            "the post-install cohort must be written as an encrypted v16 segment \
             stamped key_version 1 (INITIAL_WAL_KEY_VERSION), proving the mixed format"
        );

        let entries = wal.read_from(LSN::initial()).unwrap();
        assert_eq!(
            recovered_ids(&entries),
            (1..=10).collect::<Vec<_>>(),
            "both plaintext and encrypted cohorts recover in order"
        );
    }

    /// Encryption-at-rest negative control (FIX B, Issue #3616 PR2 review).
    /// The five install tests before this one are all satisfied by the surviving
    /// pre-install PLAINTEXT segments — none POSITIVELY assert that a post-install
    /// segment is actually v16-encrypted on disk, so an install that flipped the
    /// in-memory flag but kept writing plaintext (a future shared-cell desync)
    /// would pass them all. This test closes that blind spot two ways:
    ///
    /// 1. POSITIVE: at least one on-disk segment header is
    ///    `WAL_VERSION_ENCRYPTED_KEYVERSIONED` (v16), proven via the reader's own
    ///    header-only scan (`max_key_version_in_dir` returns `Some(1)`).
    /// 2. STRONGEST: re-reading the WAL directory with NO keyring must NOT recover
    ///    the post-install cohort as plaintext (it is genuinely undecryptable),
    ///    while the pre-install cohort still reads back as plaintext v13.
    #[test]
    fn install_keyring_post_install_segment_is_encrypted_at_rest() {
        use crate::storage::wal::segment_reader::{
            max_key_version_in_dir, read_entries_from_dir_with_keyring,
        };

        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        // Pre-install cohort → plaintext v13 segment(s).
        for id in 1..=4 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();
        assert_eq!(
            max_key_version_in_dir(dir.path()),
            None,
            "before install, no segment carries a stamped key_version (all plaintext)"
        );

        wal.install_wal_keyring(wal_keyring(7)).unwrap();

        // Post-install cohort → encrypted v16 segment(s).
        for id in 5..=8 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();

        // (1) POSITIVE v16-at-rest proof: a keyversioned segment now exists on
        // disk, stamped INITIAL_WAL_KEY_VERSION (1). A desync that kept writing
        // plaintext would leave this `None`.
        assert_eq!(
            max_key_version_in_dir(dir.path()),
            Some(1),
            "the post-install cohort must be an encrypted v16 segment on disk"
        );

        // Full recovery WITH the keyring reads every cohort back in order.
        let entries = wal.read_from(LSN::initial()).unwrap();
        assert_eq!(
            recovered_ids(&entries),
            (1..=8).collect::<Vec<_>>(),
            "with the keyring, both plaintext and encrypted cohorts recover in order"
        );

        // (2) STRONGEST proof: re-read the SAME directory with NO keyring. The
        // post-install (v16) cohort must NOT be recoverable-as-plaintext — either
        // the read errors (undecryptable) or it returns only the plaintext prefix.
        // The pre-install (v13) cohort is plaintext and reads transparently.
        match read_entries_from_dir_with_keyring(dir.path(), LSN::initial(), None, true) {
            Ok(no_key_entries) => {
                let ids = recovered_ids(&no_key_entries);
                for id in 1..=4 {
                    assert!(
                        ids.contains(&id),
                        "pre-install plaintext record {id} must still read without a keyring"
                    );
                }
                for id in 5..=8 {
                    assert!(
                        !ids.contains(&id),
                        "post-install record {id} must NOT be recoverable as plaintext \
                         without the keyring (it is encrypted at rest)"
                    );
                }
            }
            Err(_) => {
                // Undecryptable without the keyring is itself proof of
                // encryption-at-rest for the post-install cohort.
            }
        }
    }

    /// T4 — double install rejected. Installing a second keyring when one is
    /// already present returns a structured error (not a silent replace) and
    /// loses no data.
    #[test]
    fn install_keyring_twice_is_rejected() {
        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        wal.append(create_test_operation(1)).unwrap();
        wal.install_wal_keyring(wal_keyring(7)).unwrap();
        wal.append(create_test_operation(2)).unwrap();

        let err = wal
            .install_wal_keyring(wal_keyring(9))
            .expect_err("a second install must be rejected, not silently applied");
        let msg = err.to_string();
        assert!(
            msg.contains("keyring") || msg.contains("already"),
            "rejection error should explain a keyring is already present, got: {msg}"
        );

        // Still encrypted and no data lost.
        assert!(wal.is_encrypted());
        wal.append(create_test_operation(3)).unwrap();
        wal.flush().unwrap();
        let entries = wal.read_from(LSN::initial()).unwrap();
        assert_eq!(
            recovered_ids(&entries),
            vec![1, 2, 3],
            "a rejected double-install must not lose or corrupt any records"
        );
    }

    /// T5 — no torn reads on the presence cell. Concurrent loaders read the cell
    /// while one thread stores into it (via install); a load must always yield
    /// either the old `None` or a fully-valid `Some`, never a partial/invalid
    /// keyring.
    #[test]
    fn install_keyring_no_torn_reads_on_cell() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicBool, Ordering as AtOrd};

        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

        let loaders = 6usize;
        let stop = Arc::new(AtomicBool::new(false));
        let barrier = Arc::new(Barrier::new(loaders + 1));

        let mut handles = Vec::new();
        for _ in 0..loaders {
            let wal = Arc::clone(&wal);
            let stop = Arc::clone(&stop);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                // Per-thread record of which cell states this loader observed, so
                // after join we can prove the None→Some store is actually seen
                // across threads (not merely that a load never tore). The bare
                // `k.current().is_some()` check alone can never fail, so it cannot
                // catch a store that is never observed.
                let mut saw_none = false;
                let mut saw_some = false;
                barrier.wait();
                while !stop.load(AtOrd::Relaxed) {
                    match wal.wal_keyring() {
                        None => saw_none = true,
                        Some(k) => {
                            saw_some = true;
                            // Any observed keyring must be fully valid (never
                            // torn): a Some always has a resolvable current gen.
                            assert!(
                                k.current().is_some(),
                                "a loaded keyring must be fully-constructed (current generation present)"
                            );
                        }
                    }
                }
                (saw_none, saw_some)
            }));
        }

        barrier.wait();
        // Let the loaders spin on the pre-install `None` first, so that state is
        // observed before the store flips it.
        std::thread::sleep(std::time::Duration::from_millis(5));
        // Hammer the store against the concurrent loads.
        wal.install_wal_keyring(wal_keyring(7)).unwrap();
        // Keep the loaders running long enough to observe the post-install `Some`.
        std::thread::sleep(std::time::Duration::from_millis(5));
        stop.store(true, AtOrd::Relaxed);

        let mut any_saw_none = false;
        let mut any_saw_some = false;
        for h in handles {
            let (saw_none, saw_some) = h.join().unwrap();
            any_saw_none |= saw_none;
            any_saw_some |= saw_some;
        }
        assert!(
            any_saw_none,
            "loaders must have observed the pre-install None state"
        );
        assert!(
            any_saw_some,
            "loaders must have observed the post-install Some state \
             (proving the store is seen across threads)"
        );
        assert!(
            wal.wal_keyring().is_some(),
            "install must be visible after store"
        );
    }

    /// FIX A (Issue #3616 PR2 review) — concurrent double-install TOCTOU. Two
    /// threads race `install_wal_keyring` on the same system. Without the install
    /// lock both could observe `None`, both pass the presence check, and both run
    /// seal→store→reopen — the second silently replacing the first keyring and
    /// producing an undecryptable segment. With the lock, EXACTLY one returns
    /// `Ok` and one returns `Err`, no data is lost, and the survivor keyring
    /// decrypts the post-install cohort (no undecryptable segment).
    #[test]
    fn install_keyring_concurrent_double_install_rejected() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicUsize, Ordering as AtOrd};

        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

        // A few acknowledged appends before the install (plaintext v13).
        for id in 1..=3 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();

        let ok_count = Arc::new(AtomicUsize::new(0));
        let err_count = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let mut handles = Vec::new();
        for seed in [7u8, 9u8] {
            let wal = Arc::clone(&wal);
            let ok_count = Arc::clone(&ok_count);
            let err_count = Arc::clone(&err_count);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                match wal.install_wal_keyring(wal_keyring(seed)) {
                    Ok(()) => ok_count.fetch_add(1, AtOrd::Relaxed),
                    Err(_) => err_count.fetch_add(1, AtOrd::Relaxed),
                };
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Exactly one installer wins; the other is rejected (never a silent
        // second seal→store→reopen).
        assert_eq!(
            ok_count.load(AtOrd::Relaxed),
            1,
            "exactly one concurrent install must succeed"
        );
        assert_eq!(
            err_count.load(AtOrd::Relaxed),
            1,
            "exactly one concurrent install must be rejected"
        );
        assert!(wal.is_encrypted(), "the survivor keyring must be installed");

        // Post-install cohort is written under the survivor keyring (encrypted
        // v16 at rest).
        for id in 4..=6 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();
        assert_eq!(
            crate::storage::wal::segment_reader::max_key_version_in_dir(dir.path()),
            Some(1),
            "the post-install cohort must be an encrypted v16 segment (survivor keyring), \
             not an undecryptable segment from a double seal→store→reopen"
        );

        // Every acknowledged append recovers in order, and the survivor keyring
        // decrypts the post-install cohort.
        let entries = wal.read_from(LSN::initial()).unwrap();
        assert_eq!(
            recovered_ids(&entries),
            (1..=6).collect::<Vec<_>>(),
            "no acknowledged append is lost and the survivor keyring decrypts the \
             post-install cohort"
        );
    }

    // ── Issue #3616 PR4: runtime WAL keyring UNINSTALL (encrypted → plaintext) ──
    //
    // The exact inverse of the PR2 install seam: seal the encrypted (v16) active
    // segment, store `None` into the shared presence cell, and reopen a fresh
    // PLAINTEXT (v13) segment — the structural seam the encrypted → plaintext
    // DISABLE engine drives. Because dropping the keyring removes read-decrypt
    // capability, the pre-uninstall encrypted segments become undecryptable
    // through the live cell (`read_from` now snapshots `None`); the disable
    // DRIVER retires them after capturing a plaintext snapshot (a later slice).
    // These seam tests model that retire by deleting the pre-uninstall segment
    // files, then prove the post-uninstall cohort is plaintext at rest.

    /// D1 — Some→None transition: an encrypted WAL reports `is_encrypted() ==
    /// true`; after `uninstall_wal_keyring` it reports `false`; records appended
    /// after the uninstall land in a PLAINTEXT (v13) segment readable with NO
    /// keyring, and a full round-trip with the retired keyring still recovers
    /// every cohort (encrypted pre-uninstall + plaintext post-uninstall) in order.
    #[test]
    fn uninstall_keyring_some_to_none_transition() {
        use crate::storage::wal::segment_reader::read_entries_from_dir_with_keyring;
        use std::collections::HashSet;

        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        // Reach the ENCRYPTED state the disable engine strips (None → Some).
        wal.install_wal_keyring(wal_keyring(7)).unwrap();
        assert!(
            wal.is_encrypted(),
            "the WAL must be encrypted before uninstall"
        );

        // Pre-uninstall cohort → encrypted v16 segment(s).
        for id in 1..=4 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();

        // Snapshot the encrypted segment files (the driver retires these after
        // capturing a plaintext snapshot); retain a keyring clone to model the
        // driver's decrypt capability over them until then.
        let pre_uninstall_stems: HashSet<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".log").map(str::to_string)
            })
            .collect();
        let retired = wal
            .wal_keyring()
            .expect("an encrypted WAL must expose its keyring");

        // Reverse seam.
        wal.uninstall_wal_keyring().unwrap();
        assert!(
            !wal.is_encrypted(),
            "uninstalling the keyring must flip the WAL to plaintext"
        );

        // Post-uninstall cohort → plaintext v13 segment(s).
        for id in 5..=8 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();

        // (a) ROUND-TRIP: with the retired keyring the encrypted pre-uninstall
        // cohort and the plaintext post-uninstall cohort both recover in order
        // (plaintext segments parse regardless of a present keyring).
        let all =
            read_entries_from_dir_with_keyring(dir.path(), LSN::initial(), Some(&retired), true)
                .unwrap();
        assert_eq!(
            recovered_ids(&all),
            (1..=8).collect::<Vec<_>>(),
            "with the retired keyring both cohorts recover in order across the uninstall"
        );

        // (b) PLAINTEXT-AT-REST: model the driver's retire of the encrypted
        // segments, then a read with NO keyring recovers the post-uninstall
        // cohort as plaintext — positive proof the reopened segment is v13, not
        // an in-memory-flag flip that kept writing encrypted.
        for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name
                .strip_suffix(".log")
                .or_else(|| name.strip_suffix(".log.meta"));
            if let Some(stem) = stem
                && pre_uninstall_stems.contains(stem)
            {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
        let plaintext_only =
            read_entries_from_dir_with_keyring(dir.path(), LSN::initial(), None, true).unwrap();
        assert_eq!(
            recovered_ids(&plaintext_only),
            (5..=8).collect::<Vec<_>>(),
            "after retiring the encrypted segments, the post-uninstall cohort reads back \
             as plaintext with NO keyring (the reopened segment is v13 at rest)"
        );
    }

    /// D2 — uninstall on a plaintext WAL is rejected. Uninstalling when NO keyring
    /// is present returns a structured error (not a silent no-op or a spurious
    /// roll) and loses no data — the mirror of the double-install rejection.
    #[test]
    fn uninstall_keyring_when_plaintext_is_rejected() {
        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        wal.append(create_test_operation(1)).unwrap();
        wal.append(create_test_operation(2)).unwrap();

        let err = wal
            .uninstall_wal_keyring()
            .expect_err("uninstalling a plaintext WAL must be rejected, not silently applied");
        let msg = err.to_string();
        assert!(
            msg.contains("keyring") || msg.contains("plaintext") || msg.contains("not"),
            "rejection error should explain no keyring is installed, got: {msg}"
        );

        // Still plaintext and no data lost.
        assert!(!wal.is_encrypted());
        wal.append(create_test_operation(3)).unwrap();
        wal.flush().unwrap();
        let entries = wal.read_from(LSN::initial()).unwrap();
        assert_eq!(
            recovered_ids(&entries),
            vec![1, 2, 3],
            "a rejected uninstall must not lose or corrupt any records"
        );
    }

    #[test]
    fn test_concurrent_wal_system_creation() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        assert_eq!(wal.total_appends(), 0);
        assert_eq!(wal.current_lsn(), LSN(1));
    }

    #[test]
    fn test_append_sync_mode() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let lsn = wal.append(create_test_operation(1)).unwrap();
        assert_eq!(lsn, LSN(1));
        assert_eq!(wal.total_appends(), 1);
    }

    #[test]
    fn test_append_sync_mode_handles_more_than_stripe_capacity() {
        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        // Keep capacity intentionally tiny to regression-test the benchmark footgun:
        // mode-aware `append()` must continue making progress even when the buffered
        // async path would hit backpressure quickly.
        config.stripe_capacity = 8;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        for i in 1..=64 {
            let lsn = wal.append(create_test_operation(i)).unwrap();
            assert_eq!(lsn, LSN(i));
        }

        assert_eq!(wal.total_appends(), 64);
        assert_eq!(wal.total_flushed(), 64);
    }

    #[test]
    fn test_append_async_mode() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_flush_interval_ms(10_000) // Explicitly set config interval to avoid racing with default 10ms
            .with_durability_mode(DurabilityMode::Async {
                flush_interval_ms: 10_000,
            });
        let mut wal = ConcurrentWalSystem::new(config).unwrap();

        // Append several entries
        for i in 1..=10 {
            let lsn = wal.append(create_test_operation(i)).unwrap();
            assert_eq!(lsn, LSN(i));
        }

        assert_eq!(wal.total_appends(), 10);

        // Explicit flush - ensure all entries are durable.
        // Note: The background flush thread may have already flushed some/all
        // entries, so we check total_flushed() rather than the return stats.
        // This makes the test deterministic regardless of timing.
        wal.flush().unwrap();

        // Wait for flush to complete (handle race with background thread)
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(5);
        while wal.total_flushed() < 10 {
            // LCOV_EXCL_START
            if start.elapsed() > timeout {
                break;
            }
            // LCOV_EXCL_STOP
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(wal.total_flushed(), 10, "All 10 entries should be flushed");

        wal.shutdown();
        assert_eq!(wal.total_flushed(), 10, "All 10 entries should be flushed");
    }

    #[test]
    fn test_concurrent_appends() {
        use std::sync::Arc;
        use std::thread;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Async {
                flush_interval_ms: 100,
            })
            .with_num_stripes(4);
        let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

        let num_threads = 4;
        let ops_per_thread = 100;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let wal = Arc::clone(&wal);
                thread::spawn(move || {
                    for i in 0..ops_per_thread {
                        let id = (t * ops_per_thread + i + 1) as u64;
                        wal.append_async(create_test_operation(id)).unwrap();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(wal.total_appends(), (num_threads * ops_per_thread) as u64);
    }

    #[test]
    fn test_flush_persists_entries() {
        let dir = tempdir().unwrap();
        // Use Synchronous mode to avoid background flush thread interference
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let mut wal = ConcurrentWalSystem::new(config).unwrap();

        // Append entries (append_async in Sync mode still buffers)
        for i in 1..=5 {
            wal.append_async(create_test_operation(i)).unwrap();
        }

        // Force flush - since no background thread, all 5 entries should be flushed here
        let stats = wal.flush().unwrap();
        assert_eq!(stats.entries_flushed, 5);

        // Verify flushed count
        assert_eq!(wal.total_flushed(), 5);

        wal.shutdown();
    }

    #[test]
    fn test_group_commit_mode() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::GroupCommit {
                max_batch_size: 10,
                max_delay_ms: 10,
            })
            .with_flush_interval_ms(5);
        let mut wal = ConcurrentWalSystem::new(config).unwrap();

        // Append entries
        for i in 1..=5 {
            wal.append(create_test_operation(i)).unwrap();
        }

        // Wait for background flush with polling (more resilient than single sleep)
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(5); // Increased timeout for CI
        let mut flushed = false;
        while start.elapsed() < timeout {
            if wal.total_flushed() >= 1 {
                flushed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        // Should have been flushed by background thread
        assert!(
            flushed,
            "Expected at least 1 entry to be flushed within {}ms, but got {} flushed",
            timeout.as_millis(),
            wal.total_flushed()
        );

        wal.shutdown();
    }

    #[test]
    fn test_shutdown_flushes_remaining() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path()).with_durability_mode(
            DurabilityMode::Async {
                flush_interval_ms: 100,
            },
        );
        let mut wal = ConcurrentWalSystem::new(config).unwrap();

        // Append entries without explicit flush
        for i in 1..=5 {
            wal.append_async(create_test_operation(i)).unwrap();
        }

        // Shutdown should flush remaining
        wal.shutdown();

        // All entries should be flushed
        assert_eq!(wal.total_flushed(), 5);
    }

    // ============================================================
    // Batch Append Tests (Issue #219)
    // ============================================================

    #[test]
    fn test_append_batch_async() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path()).with_durability_mode(
            DurabilityMode::Async {
                flush_interval_ms: 10_000,
            },
        );
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let ops = vec![
            create_test_operation(1),
            create_test_operation(2),
            create_test_operation(3),
        ];

        let lsns = wal.append_batch(ops).unwrap();

        assert_eq!(lsns.len(), 3);
        assert_eq!(lsns[0], LSN(1));
        assert_eq!(lsns[1], LSN(2));
        assert_eq!(lsns[2], LSN(3));
        assert_eq!(wal.total_appends(), 3);
    }

    #[test]
    fn test_append_batch_sync() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let ops = vec![create_test_operation(1), create_test_operation(2)];

        let lsns = wal.append_batch(ops).unwrap();

        assert_eq!(lsns.len(), 2);
        assert_eq!(lsns[0], LSN(1));
        assert_eq!(lsns[1], LSN(2));
        assert_eq!(wal.total_appends(), 2);
    }

    #[test]
    fn test_append_batch_empty() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let lsns = wal.append_batch(vec![]).unwrap();

        assert_eq!(lsns.len(), 0);
        assert_eq!(wal.total_appends(), 0);
    }

    #[test]
    fn test_append_batch_large() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path()).with_durability_mode(
            DurabilityMode::Async {
                flush_interval_ms: 10_000,
            },
        );
        let wal = ConcurrentWalSystem::new(config).unwrap();

        // Create 100 operations
        let ops: Vec<_> = (1..=100).map(create_test_operation).collect();

        let lsns = wal.append_batch(ops).unwrap();

        assert_eq!(lsns.len(), 100);
        assert_eq!(lsns[0], LSN(1));
        assert_eq!(lsns[99], LSN(100));
        assert_eq!(wal.total_appends(), 100);
    }

    #[test]
    fn test_append_sync_persistence_guarantee() {
        // This test verifies that append_sync actually waits for the flush.
        // While we can't easily deterministic race condition, we can verify basic
        // persistence guarantee: immediately after append_sync returns, total_flushed
        // must be incremented.

        let dir = tempdir().unwrap();
        // Use Synchronous mode
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let wal = ConcurrentWalSystem::new(config).unwrap();

        // 1. Initial state
        assert_eq!(wal.total_flushed(), 0);

        // 2. Perform append_sync
        let lsn = wal.append_sync(create_test_operation(1)).unwrap();

        // 3. Immediately assert flushed count
        // If append_sync didn't wait, and flush was async/delayed, this might fail.
        // But since it's sync, it MUST be 1.
        assert_eq!(
            wal.total_flushed(),
            1,
            "Should be flushed immediately after return"
        );
        assert_eq!(lsn, LSN(1));

        // 4. Batch append sync
        let ops = vec![create_test_operation(2), create_test_operation(3)];
        let lsns = wal.append_batch(ops).unwrap();

        // 5. Assert flushed count increased by 2
        assert_eq!(
            wal.total_flushed(),
            3,
            "Batch should be flushed immediately"
        );
        assert_eq!(lsns.len(), 2);
    }

    // ── Issue #3798: background flusher liveness ─────────────────────────
    //
    // A flush thread that dies (or wedges) is currently invisible: writers
    // simply block forever on full ring buffers and the health check keeps
    // reporting "healthy". These tests pin the observable contract —
    // a heartbeat, a survivable error path, and a health check that reads it.

    /// Watchdog for every bounded poll below. No assertion here may depend on
    /// timing luck: each one either observes its condition inside this window
    /// or fails with an explicit message.
    const LIVENESS_WATCHDOG: Duration = Duration::from_secs(5);

    /// Poll `cond` every 10ms until it holds or the watchdog elapses.
    fn poll_until(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + LIVENESS_WATCHDOG;
        while std::time::Instant::now() < deadline {
            if cond() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        cond()
    }

    fn group_commit_config(
        dir: &std::path::Path,
        flush_interval_ms: u64,
    ) -> ConcurrentWalSystemConfig {
        ConcurrentWalSystemConfig::new(dir)
            .with_durability_mode(DurabilityMode::GroupCommit {
                max_batch_size: 8,
                max_delay_ms: 5,
            })
            .with_flush_interval_ms(flush_interval_ms)
    }

    /// The system's own coordinator, for tests that drive flush cycles by hand.
    fn coordinator_of(system: &ConcurrentWalSystem) -> Arc<GroupCommitCoordinator> {
        Arc::clone(
            system
                .group_commit_coordinator()
                .expect("GroupCommit mode has a coordinator"),
        )
    }

    /// A [`BackgroundFlusher`] over `system`'s own parts, driven synchronously
    /// on the calling thread.
    ///
    /// Every flush-outcome test below needs one, and the struct is private, so
    /// building it lives here rather than being copied per test.
    fn test_flusher(
        system: &ConcurrentWalSystem,
        gc: &Arc<GroupCommitCoordinator>,
    ) -> BackgroundFlusher {
        BackgroundFlusher {
            wal: Arc::clone(&system.wal),
            coordinator: Arc::clone(&system.coordinator),
            shutdown: Arc::clone(&system.shutdown_signal),
            flush_notifier: Arc::clone(&system.flush_notifier),
            group_commit: Some(Arc::clone(gc)),
            error_counter: Arc::clone(&system.consecutive_flush_errors),
            interval: Duration::from_millis(10),
            sync_on_flush: false,
            flush_heartbeat: Arc::clone(&system.flush_heartbeat),
            last_beat_micros: Arc::clone(&system.last_beat_micros),
            flush_cycle_errors: Arc::clone(&system.flush_cycle_errors),
            test_inject_cycle_error: Arc::clone(&system.test_inject_cycle_error),
            pending_finishes: std::cell::RefCell::new(std::collections::VecDeque::new()),
            carried_failure: std::cell::RefCell::new(None),
        }
    }

    /// A system whose own flusher has run its startup cycle and then parked
    /// for an hour, so the cycles a test drives by hand are the only ones.
    fn quiesced_group_commit_system(dir: &std::path::Path) -> ConcurrentWalSystem {
        let system = ConcurrentWalSystem::new(group_commit_config(dir, 3_600_000)).unwrap();
        assert!(
            poll_until(|| system.flush_heartbeat() >= 1),
            "the startup flush cycle never completed, so this test cannot own the timeline"
        );
        system
    }

    /// A `finish_flush` the coordinator refuses must NOT strand its epoch.
    ///
    /// `start_flush` has already advanced `current_epoch` by then, and the
    /// frontier only moves over a contiguous run, so an abandoned epoch is a
    /// permanent wall: every later commit would wait out its full
    /// deadlock-detection timeout and fail, forever, on a WAL that is in fact
    /// healthy. The flusher must hold the un-finished epoch and land it later.
    ///
    /// Deterministic by construction: the cycles are driven synchronously on
    /// this thread and the single acquisition failure is injected rather than
    /// raced (the window between `start_flush` and `finish_flush` is
    /// microseconds wide, so racing a real acquisition timeout into it is not
    /// reproducible).
    #[test]
    fn test_stranded_finish_flush_is_retried_and_unwedges_the_frontier() {
        let dir = tempdir().unwrap();
        let system = quiesced_group_commit_system(dir.path());
        let gc = coordinator_of(&system);
        let flusher = test_flusher(&system, &gc);

        // A transaction waiting on the epoch the next cycle will flush.
        let (epoch, _) = gc.register_transaction().expect("registration succeeds");
        let errors_before = system.flush_cycle_errors();

        // Cycle 1: start_flush opens `epoch`, finish_flush is refused.
        gc.fail_next_acquisition_at_for_test(
            crate::storage::wal::group_commit::LockSite::FinishFlush,
        );
        flusher.perform_flush_cycle();

        assert_eq!(
            system.flush_cycle_errors(),
            errors_before + 1,
            "the refused finish_flush must be counted, not swallowed silently"
        );
        assert!(
            gc.flushed_epoch().expect("frontier readable") < epoch,
            "precondition: the frontier is frozen behind the stranded epoch"
        );

        // Cycle 2: the retained epoch is retried and lands.
        flusher.perform_flush_cycle();
        assert!(
            gc.flushed_epoch().expect("frontier readable") >= epoch,
            "the durability frontier never recovered: epoch {} was stranded for good \
             (flushed_epoch = {})",
            epoch,
            gc.flushed_epoch().unwrap_or(0)
        );
        gc.wait_for_flush(epoch)
            .expect("the transaction waiting on the stranded epoch must be released");

        // ...and the coordinator keeps working for transactions registered
        // afterwards, which is what "unwedged" has to mean.
        let (next_epoch, _) = gc.register_transaction().expect("registration succeeds");
        flusher.perform_flush_cycle();
        assert!(
            gc.flushed_epoch().expect("frontier readable") >= next_epoch,
            "a transaction registered after the strand must still reach durability"
        );
        gc.wait_for_flush(next_epoch)
            .expect("post-recovery commit must not wait out its deadlock-detection timeout");
    }

    /// The `LockSite` the flush-outcome tests inject failures at, spelled once.
    const FINISH_FLUSH: crate::storage::wal::group_commit::LockSite =
        crate::storage::wal::group_commit::LockSite::FinishFlush;
    const START_FLUSH: crate::storage::wal::group_commit::LockSite =
        crate::storage::wal::group_commit::LockSite::StartFlush;

    /// A synthetic disk-flush failure, exactly as `perform_flush_cycle` would
    /// hand one to `handle_flush_result` when `coordinator.flush()` fails.
    ///
    /// Driven through that seam rather than by breaking the real coordinator:
    /// the sequence under test needs the failure to land on a *specific* cycle
    /// while a stranded epoch is outstanding, which no I/O-level fault
    /// injection can schedule deterministically.
    fn flush_failure(what: &str) -> Result<()> {
        Err(crate::core::error::Error::other(what.to_string()))
    }

    /// A FAILED disk flush must never be silently discarded because an earlier
    /// epoch is still stranded.
    ///
    /// The exact #3798-round-2 sequence: epoch A's `finish_flush` is refused
    /// and strands; the next cycle's `coordinator.flush()` FAILS for epoch B;
    /// a later cycle lands A and then advances the frontier past B. If the
    /// failing outcome was dropped on the way in (because a pending finish was
    /// outstanding), B's waiters are told their transaction is durable when it
    /// never reached disk -- an acknowledged-but-lost commit, the worst failure
    /// this subsystem can produce.
    #[test]
    fn test_failed_flush_outcome_survives_an_outstanding_pending_finish() {
        let dir = tempdir().unwrap();
        let system = quiesced_group_commit_system(dir.path());
        let gc = coordinator_of(&system);
        let flusher = test_flusher(&system, &gc);

        // Epoch A: opened, then stranded by a refused finish_flush.
        let (epoch_a, _) = gc.register_transaction().expect("registration succeeds");
        gc.fail_next_acquisition_at_for_test(FINISH_FLUSH);
        flusher.perform_flush_cycle();
        assert!(
            gc.flushed_epoch().expect("frontier readable") < epoch_a,
            "precondition: epoch {epoch_a} must be stranded before the failing flush"
        );

        // Epoch B: a transaction registered behind the strand, whose flush
        // then fails on disk.
        let (epoch_b, _) = gc.register_transaction().expect("registration succeeds");
        assert!(epoch_b > epoch_a, "B must be the epoch after A");
        flusher.handle_flush_result(flush_failure("injected disk flush failure"));

        // Recovery: A lands, then the frontier is free to move past B.
        flusher.perform_flush_cycle();
        flusher.perform_flush_cycle();
        assert!(
            gc.flushed_epoch().expect("frontier readable") >= epoch_b,
            "the frontier never recovered past epoch {epoch_b}"
        );

        gc.wait_for_flush(epoch_a)
            .expect("epoch A's flush succeeded, so its waiters must be released cleanly");

        let err = gc.wait_for_flush(epoch_b).expect_err(
            "epoch B's flush FAILED, but its waiters were told the transaction is durable: \
             the failing outcome was discarded because a pending finish was outstanding",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("injected disk flush failure"),
            "the epoch's own flush error must reach its waiters, got: {msg}"
        );
    }

    /// Two stranded finishes must drain in order and each deliver its OWN
    /// outcome.
    ///
    /// A queue that coalesced them, reordered them, or reused one outcome for
    /// both would either wedge the contiguous-advance logic or hand a
    /// successful epoch someone else's error (and vice versa).
    #[test]
    fn test_queued_finishes_drain_fifo_with_their_own_outcomes() {
        let dir = tempdir().unwrap();
        let system = quiesced_group_commit_system(dir.path());
        let gc = coordinator_of(&system);
        let flusher = test_flusher(&system, &gc);

        // Strand A with a SUCCESSFUL outcome.
        let (epoch_a, _) = gc.register_transaction().expect("registration succeeds");
        gc.fail_next_acquisition_at_for_test(FINISH_FLUSH);
        flusher.perform_flush_cycle();

        // Strand B, behind A, with a FAILING outcome.
        let (epoch_b, _) = gc.register_transaction().expect("registration succeeds");
        gc.fail_next_acquisition_at_for_test(FINISH_FLUSH);
        flusher.handle_flush_result(flush_failure("epoch B never reached disk"));

        // One cycle drains both, oldest first.
        flusher.perform_flush_cycle();
        assert!(
            gc.flushed_epoch().expect("frontier readable") >= epoch_b,
            "both queued finishes must land, frontier is at {}",
            gc.flushed_epoch().unwrap_or(0)
        );

        gc.wait_for_flush(epoch_a)
            .expect("epoch A succeeded and must resolve as a success");
        let msg = gc
            .wait_for_flush(epoch_b)
            .expect_err("epoch B failed and must resolve as a failure")
            .to_string();
        assert!(
            msg.contains("epoch B never reached disk"),
            "each queued epoch must carry its own outcome, got: {msg}"
        );
    }

    /// A failing outcome whose epoch could not even be OPENED must still reach
    /// the epoch those transactions land in.
    ///
    /// A refused `start_flush` advances nothing, so `current_epoch` still
    /// covers exactly the same registered transactions -- the ones whose data
    /// was just lost. Letting the next cycle open that epoch and report a
    /// clean success would be the same acknowledged-but-lost commit by another
    /// route.
    #[test]
    fn test_failed_flush_outcome_survives_a_refused_start_flush() {
        let dir = tempdir().unwrap();
        let system = quiesced_group_commit_system(dir.path());
        let gc = coordinator_of(&system);
        let flusher = test_flusher(&system, &gc);

        let (epoch, _) = gc.register_transaction().expect("registration succeeds");

        // The disk flush fails AND the epoch cannot be opened to report it.
        gc.fail_next_acquisition_at_for_test(START_FLUSH);
        flusher.handle_flush_result(flush_failure("disk exploded before the epoch opened"));

        // The next cycle opens that same epoch (nothing advanced).
        flusher.perform_flush_cycle();
        assert!(
            gc.flushed_epoch().expect("frontier readable") >= epoch,
            "the frontier must still advance once the coordinator recovers"
        );

        let msg = gc
            .wait_for_flush(epoch)
            .expect_err(
                "the lost flush was reported as a clean success: a failure whose start_flush \
                 was refused must still reach the epoch covering those transactions",
            )
            .to_string();
        assert!(
            msg.contains("disk exploded before the epoch opened"),
            "the carried failure must name the original error, got: {msg}"
        );
    }

    /// A transient (non-poison) coordinator error must NOT take down the flush
    /// thread: it should be counted, the cycle skipped, and flushing resumed.
    #[test]
    fn test_flush_thread_survives_injected_cycle_error() {
        let dir = tempdir().unwrap();
        let wal = ConcurrentWalSystem::new(group_commit_config(dir.path(), 20)).unwrap();

        // Make the next flush cycle observe a non-poison error, exactly as a
        // transient coordinator failure would.
        wal.test_inject_cycle_error.store(true, Ordering::Relaxed);

        assert!(
            poll_until(|| wal.flush_cycle_errors() >= 1),
            "flush thread died on non-poison error (current .expect() behavior): \
             flush_cycle_errors stayed at {} for {:?}, so no cycle was ever skipped-and-counted",
            wal.flush_cycle_errors(),
            LIVENESS_WATCHDOG
        );

        // Surviving means the heartbeat keeps advancing AFTER the error.
        let beat_after_error = wal.flush_heartbeat();
        assert!(
            poll_until(|| wal.flush_heartbeat() > beat_after_error),
            "flush heartbeat frozen at {} after the injected error: the flush thread \
             did not survive it",
            beat_after_error
        );

        // ...and the WAL still round-trips real work.
        wal.append(create_test_operation(1))
            .expect("append after an injected flush-cycle error must succeed");
        wal.flush()
            .expect("flush after an injected flush-cycle error must succeed");
        assert!(
            wal.total_flushed() >= 1,
            "the post-error append must reach disk"
        );
    }

    /// The flusher must publish a strictly increasing heartbeat so its
    /// liveness is observable from outside the thread.
    #[test]
    fn test_flush_heartbeat_advances() {
        let dir = tempdir().unwrap();
        let wal = ConcurrentWalSystem::new(group_commit_config(dir.path(), 10)).unwrap();

        assert!(
            poll_until(|| wal.flush_heartbeat() >= 1),
            "flush_heartbeat() never reached 1 within {:?} with a 10ms flush interval: \
             the flusher publishes no liveness signal",
            LIVENESS_WATCHDOG
        );

        let first = wal.flush_heartbeat();
        assert!(
            poll_until(|| wal.flush_heartbeat() > first),
            "flush_heartbeat() stuck at {} within {:?}: the signal does not advance per cycle",
            first,
            LIVENESS_WATCHDOG
        );
    }

    /// A flusher that runs its startup cycle and then sleeps ~forever is a
    /// wedged flusher; the health check must say so even though no flush has
    /// actually errored.
    #[test]
    fn test_is_healthy_detects_stale_heartbeat() {
        let dir = tempdir().unwrap();
        // One startup cycle, then a ~1h sleep: a heartbeat that cannot advance
        // again on its own -- wedged by configuration.
        let wal = ConcurrentWalSystem::new(group_commit_config(dir.path(), 3_600_000)).unwrap();
        wal.set_health_staleness_override_micros(50_000); // 50ms

        assert!(
            poll_until(|| !wal.is_healthy()),
            "is_healthy() stayed true for {:?} with the heartbeat frozen far beyond the \
             50ms staleness override: a wedged flusher is invisible to health checks",
            LIVENESS_WATCHDOG
        );
        assert_eq!(
            wal.consecutive_flush_errors(),
            0,
            "no flush actually failed here; only the heartbeat is stale"
        );
    }

    /// Edge pin (passes before AND after the liveness fix): a durability mode
    /// with no flush thread has no heartbeat to go stale, so it must stay
    /// healthy no matter how strict the staleness threshold is.
    #[test]
    fn test_is_healthy_true_without_flush_thread() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let wal = ConcurrentWalSystem::new(config).unwrap();

        assert_eq!(
            wal.flush_heartbeat(),
            0,
            "Synchronous mode runs no background flusher"
        );
        wal.set_health_staleness_override_micros(1); // absurdly strict
        thread::sleep(Duration::from_millis(20));

        assert!(
            wal.is_healthy(),
            "a WAL with no flush thread has no heartbeat that can go stale"
        );
        wal.append(create_test_operation(1)).unwrap();
        assert!(
            wal.is_healthy(),
            "synchronous appends keep the WAL healthy regardless of heartbeat staleness"
        );
    }

    /// Pure formula: ten flush intervals, floored at five seconds.
    ///
    /// The floor is what the default 10ms interval actually gets, and it is
    /// deliberately far above a plausible scheduling or fsync hiccup: a
    /// stalled flusher stays stalled, so the only thing a tight floor buys is
    /// false alarms.
    #[test]
    fn test_staleness_threshold_formula() {
        assert_eq!(
            heartbeat_staleness_threshold(Duration::from_millis(10)),
            Duration::from_secs(5),
            "short intervals are floored at 5s"
        );
        assert_eq!(
            heartbeat_staleness_threshold(Duration::from_millis(200)),
            Duration::from_secs(5),
            "2s of ten-intervals is still below the floor"
        );
        assert_eq!(
            heartbeat_staleness_threshold(Duration::from_millis(1000)),
            Duration::from_secs(10),
            "above the floor the threshold is 10x the interval"
        );
        assert_eq!(
            heartbeat_staleness_threshold(Duration::ZERO),
            Duration::from_secs(5),
            "a zero interval still gets the floor"
        );
    }

    /// No panicking construct may remain on the flush-thread path.
    ///
    /// A panic on that thread kills the only consumer of the ring buffers,
    /// which IS the #3798 stall: writers then pile up behind full buffers with
    /// nothing left to drain them. The scan therefore covers every region the
    /// flush thread executes -- the flusher itself, the spawn site, and the
    /// loop entry point -- delimited by explicit marker comments so the guard
    /// travels with the code instead of with a heading someone may rename.
    ///
    /// The poison arm's deliberate `panic!` stays allowed: a poisoned
    /// coordinator means a thread died holding the lock, and continuing would
    /// hang every waiter (see `note_cycle_error`).
    #[test]
    fn test_no_panicking_constructs_on_flush_thread_path() {
        const SOURCE: &str = include_str!("concurrent_system.rs");

        // Markers and needles are assembled at runtime so this test's own
        // source can never match itself.
        let begin = format!("// ---- flush-thread path: {} ----", "begin");
        let end = format!("// ---- flush-thread path: {} ----", "end");

        let mut regions: Vec<&str> = Vec::new();
        let mut cursor = 0usize;
        while let Some(rel) = SOURCE[cursor..].find(begin.as_str()) {
            let start = cursor + rel + begin.len();
            let rel_end = SOURCE[start..]
                .find(end.as_str())
                .expect("every flush-thread begin marker needs a matching end marker");
            regions.push(&SOURCE[start..start + rel_end]);
            cursor = start + rel_end + end.len();
        }

        assert_eq!(
            regions.len(),
            3,
            "expected three marked flush-thread regions (the flusher, the spawn site, and \
             flush_loop); markers were moved or dropped"
        );
        // Coverage anchors: a region pair collapsed around nothing would keep
        // the scan green while guarding no code at all.
        let all = regions.concat();
        for anchor in ["impl BackgroundFlusher", "thread::spawn", "fn flush_loop"] {
            assert!(
                all.contains(anchor),
                "the marked regions no longer cover `{anchor}`: the guard is watching the \
                 wrong code"
            );
        }

        for needle in [
            format!("{}{}", ".expect", "("),
            format!("{}{}", ".unwrap", "()"),
            format!("{}{}", "print", "ln!"),
            format!("{}{}", "eprint", "ln!"),
        ] {
            let hits: usize = regions.iter().map(|r| r.matches(&needle).count()).sum();
            assert_eq!(
                hits, 0,
                "found {} `{}` on the flush-thread path: a non-poison error must be counted \
                 and skipped, and logging must go through the non-panicking helper (the \
                 print macros panic on EPIPE)",
                hits, needle
            );
        }
    }
}
