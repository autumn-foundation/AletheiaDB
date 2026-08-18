//! Group commit coordination for batched WAL flushes.
//!
//! This module provides [`GroupCommitCoordinator`], which manages the epoch-based
//! waiting mechanism for [`GroupCommit`](super::DurabilityMode::GroupCommit) mode.
//!
//! # Error Handling Strategy
//!
//! All public methods return `Result` types to handle lock poisoning gracefully.
//! Lock poisoning occurs when a thread panics while holding the coordinator's mutex.
//!
//! ## For Callers
//!
//! When a `StorageError::LockPoisoned` error is returned:
//! - **Flush thread**: Should panic immediately. Continuing would leave waiting
//!   transactions hanging indefinitely. This is an unrecoverable state.
//! - **Transaction threads**: Should propagate the error to the caller. The
//!   transaction cannot complete and must be rolled back.
//!
//! ## Rationale
//!
//! Lock poisoning in the coordinator indicates severe corruption. The alternatives
//! are:
//! 1. **Panic everywhere** (too aggressive for transaction threads)
//! 2. **Continue silently** (leaves transactions hanging - worse than panicking)
//! 3. **Return Result** (chosen approach - lets callers decide appropriate action)
//!
//! The flush thread uses `.expect()` to panic on lock poisoning because silent
//! degradation is worse than fail-fast behavior for background infrastructure.

use std::collections::{BTreeSet, VecDeque};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::core::error::{Error, StorageError};

/// Source of the next never-recycled thread token.
///
/// Starts at 1 because `0` is the "no owner" sentinel stored in
/// [`GroupCommitCoordinator::owner`]. Tokens are never reused, so a token
/// recorded in an error payload unambiguously identifies one thread for the
/// lifetime of the process.
static NEXT_THREAD_TOKEN: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// This thread's stable identity for re-entrancy detection.
    ///
    /// `std::thread::current().id()` is not usable here: `ThreadId` is opaque
    /// and cannot be stored in an `AtomicU64`, and its values *are* recycled
    /// once a thread is joined.
    static THREAD_TOKEN: u64 = NEXT_THREAD_TOKEN.fetch_add(1, Ordering::Relaxed);
}

/// Return this thread's never-recycled lock-ownership token (always non-zero).
///
/// Pure infrastructure: it has no effect on locking behavior on its own, and
/// is the value the re-entrancy check compares against
/// [`GroupCommitCoordinator::owner`].
#[allow(dead_code)] // Consumed by the #3798 GREEN implementation of `lock_state`.
fn current_thread_token() -> u64 {
    THREAD_TOKEN.with(|token| *token)
}

/// Which coordinator method is acquiring (or holding) the state mutex.
///
/// Recorded in diagnostics so a stuck acquisition names both the blocked call
/// site and the holder's call site instead of an anonymous "mutex".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockSite {
    /// [`GroupCommitCoordinator::register_transaction`] — commit path.
    RegisterTransaction,
    /// [`GroupCommitCoordinator::wait_for_flush`] — commit path.
    WaitForFlush,
    /// [`GroupCommitCoordinator::start_flush`] — flusher path.
    StartFlush,
    /// [`GroupCommitCoordinator::finish_flush`] — flusher path.
    FinishFlush,
    /// [`GroupCommitCoordinator::current_batch_size`] — observation.
    CurrentBatchSize,
    /// [`GroupCommitCoordinator::current_epoch`] — observation.
    CurrentEpoch,
    /// [`GroupCommitCoordinator::flushed_epoch`] — observation.
    FlushedEpoch,
    /// [`GroupCommitCoordinator::should_flush`] — observation.
    ShouldFlush,
}

impl LockSite {
    /// Snake-case name of the method that owns this site.
    ///
    /// This exact spelling appears in the acquisition-failure error payloads,
    /// so callers reading a log line can jump straight to the method.
    #[allow(dead_code)] // Consumed by the #3798 GREEN error-payload builders.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            LockSite::RegisterTransaction => "register_transaction",
            LockSite::WaitForFlush => "wait_for_flush",
            LockSite::StartFlush => "start_flush",
            LockSite::FinishFlush => "finish_flush",
            LockSite::CurrentBatchSize => "current_batch_size",
            LockSite::CurrentEpoch => "current_epoch",
            LockSite::FlushedEpoch => "flushed_epoch",
            LockSite::ShouldFlush => "should_flush",
        }
    }

    /// Atomic-storable encoding (never `0`, which means "no site recorded").
    pub(crate) fn as_u64(self) -> u64 {
        match self {
            LockSite::RegisterTransaction => 1,
            LockSite::WaitForFlush => 2,
            LockSite::StartFlush => 3,
            LockSite::FinishFlush => 4,
            LockSite::CurrentBatchSize => 5,
            LockSite::CurrentEpoch => 6,
            LockSite::FlushedEpoch => 7,
            LockSite::ShouldFlush => 8,
        }
    }
}

/// Owner-token tracking, released independently of (and before) the mutex.
///
/// Split out of [`StateGuard`] so field drop order alone guarantees the token
/// is cleared while the mutex is still held — no `Drop` impl on `StateGuard`
/// itself, which keeps [`StateGuard::surrender`] a plain move.
struct OwnerSlot<'a> {
    coordinator: &'a GroupCommitCoordinator,
    site: LockSite,
}

impl<'a> OwnerSlot<'a> {
    /// Record this thread as the holder of the coordinator's state mutex.
    fn arm(coordinator: &'a GroupCommitCoordinator, site: LockSite) -> Self {
        // RED SCAFFOLD (#3798): ownership is not published yet, so the
        // re-entrancy check has nothing to observe. GREEN stores
        // `current_thread_token()` into `coordinator.owner` and
        // `site.as_u64()` into `coordinator.owner_site` here.
        Self { coordinator, site }
    }
}

impl Drop for OwnerSlot<'_> {
    fn drop(&mut self) {
        // RED SCAFFOLD (#3798): nothing was published by `arm`, so there is
        // nothing to clear. GREEN stores 0 into both `owner` and `owner_site`
        // here — which, thanks to `StateGuard`'s field order, happens while
        // the mutex is still held. The reads below keep both fields live and
        // document exactly what GREEN must touch.
        let _ = (
            self.coordinator.owner.load(Ordering::Relaxed),
            self.site.as_u64(),
        );
    }
}

/// RAII wrapper around the group-commit state mutex guard.
///
/// Derefs to [`GroupCommitState`], so the call sites read exactly as they did
/// when they locked the mutex directly.
pub(crate) struct StateGuard<'a> {
    /// FIELD ORDER IS LOAD-BEARING: struct fields drop in declaration order,
    /// so `owner` clears the holder token BEFORE `inner` unlocks the mutex.
    /// Reversed, another thread could acquire the mutex and still observe a
    /// stale owner token — a false re-entrancy report.
    owner: OwnerSlot<'a>,
    inner: MutexGuard<'a, GroupCommitState>,
}

impl<'a> StateGuard<'a> {
    /// Re-arm ownership tracking around an already-held raw `MutexGuard`.
    ///
    /// Used both by [`GroupCommitCoordinator::lock_state`] and (in GREEN) by
    /// `wait_for_flush` when `Condvar::wait_timeout` hands the guard back.
    fn reassemble(
        coordinator: &'a GroupCommitCoordinator,
        site: LockSite,
        inner: MutexGuard<'a, GroupCommitState>,
    ) -> Self {
        Self {
            owner: OwnerSlot::arm(coordinator, site),
            inner,
        }
    }

    /// Give up ownership tracking and hand back the raw `MutexGuard`.
    ///
    /// `Condvar::wait_timeout` consumes a `MutexGuard`, and a thread parked in
    /// the condvar holds no lock — so it must not be recorded as the owner for
    /// the duration of the wait. Destructuring (rather than a `Drop` impl on
    /// `StateGuard`) is what makes this a plain move with no panic path.
    fn surrender(self) -> MutexGuard<'a, GroupCommitState> {
        let StateGuard { owner, inner } = self;
        drop(owner);
        inner
    }
}

impl Deref for StateGuard<'_> {
    type Target = GroupCommitState;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for StateGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Coordinates group commit batching and waiting.
///
/// The coordinator uses an epoch-based system where:
/// 1. Each transaction registers and receives the current epoch number
/// 2. Multiple transactions accumulate in the same epoch
/// 3. When the batch is flushed, the epoch advances
/// 4. All waiting transactions for that epoch are notified
///
/// This allows ACID durability with amortized fsync cost across many transactions.
///
/// # Why?
///
/// Group commit is a crucial performance optimization for a database's Write-Ahead Log.
/// Fsync operations on modern NVMe drives can still take ~1-2ms. If every transaction
/// synchronously blocks on an fsync, throughput is capped at ~500-1000 tx/sec. By
/// batching multiple concurrent transactions into a single "epoch" and performing one
/// fsync for all of them, throughput can scale to hundreds of thousands of tx/sec while
/// still maintaining strict ACID durability.
///
/// # Epoch Model
///
/// ```text
/// Epoch 0: [tx1, tx2, tx3] → flush → advance to epoch 1
/// Epoch 1: [tx4, tx5] → flush → advance to epoch 2
/// ...
/// ```
///
/// Each transaction waits on its epoch number. When `mark_flushed()` is called,
/// all transactions waiting on that epoch are released.
///
/// # Error Propagation
///
/// If a flush fails, the error is stored and propagated to all waiting transactions.
/// This ensures no transaction incorrectly believes its data is durable.
///
/// # Examples
///
/// ```
/// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
/// use std::sync::Arc;
/// use std::thread;
///
/// let coordinator = Arc::new(GroupCommitCoordinator::new(10, 100));
///
/// // Transaction 1
/// let (epoch1, _) = coordinator.register_transaction().unwrap();
///
/// // Background Flush Thread
/// let flush_coord = Arc::clone(&coordinator);
/// thread::spawn(move || {
///     let epoch = flush_coord.start_flush().unwrap();
///     // Flush writes to disk here...
///     // Then notify waiting transactions
///     flush_coord.finish_flush(epoch, Ok(())).unwrap();
/// });
///
/// // Transaction 1 waits for flush
/// coordinator.wait_for_flush(epoch1).unwrap();
/// ```
pub struct GroupCommitCoordinator {
    /// State protected by mutex
    state: Mutex<GroupCommitState>,
    /// Condition variable for flush completion
    flush_complete: Condvar,
    /// Configuration
    config: GroupCommitConfig,

    // ---- Lock-acquisition instrumentation (Issue #3798) --------------------
    //
    // These are the fields the hardened `lock_state` needs. Several are only
    // written/read by the GREEN implementation; they are declared here in RED
    // so the field set is pinned by the API contract and so the failing tests
    // below compile against the final shape. `#[allow(dead_code)]` marks the
    // ones not yet touched — every one of them is consumed by GREEN.
    /// Thread token of the current [`StateGuard`] holder (0 = unheld).
    owner: AtomicU64,
    /// [`LockSite::as_u64`] of the current holder (0 = none recorded).
    #[allow(dead_code)]
    owner_site: AtomicU64,
    /// Threads currently blocked trying to acquire the state mutex.
    #[allow(dead_code)]
    waiters: AtomicU64,
    /// Acquisitions that could not be satisfied by the uncontended fast path.
    #[allow(dead_code)]
    contended_acquires: AtomicU64,
    /// Re-entrant acquisitions refused (see [`GroupCommitCoordinator::reentrancy_detections`]).
    reentrancy_detections: AtomicU64,
    /// Acquisitions abandoned at the deadline (see [`GroupCommitCoordinator::acquire_timeouts`]).
    acquire_timeouts: AtomicU64,
    /// Relaxed write-through mirror of `state.current_epoch`.
    ///
    /// Mirrors exist ONLY so a failed acquisition can describe the coordinator
    /// without taking the very mutex it could not take. They are never read
    /// for a control decision.
    #[allow(dead_code)]
    mirror_current_epoch: AtomicU64,
    /// Relaxed write-through mirror of `state.flushed_epoch` (diagnostics only).
    #[allow(dead_code)]
    mirror_flushed_epoch: AtomicU64,
    /// Relaxed write-through mirror of `state.batch_count` (diagnostics only).
    #[allow(dead_code)]
    mirror_batch_count: AtomicU64,
    /// Runtime-overridable copy of [`GroupCommitConfig::acquire_timeout_ms`].
    ///
    /// Held as an atomic (rather than read from `config`) so tests can shrink
    /// the deadline on an already-constructed coordinator.
    #[allow(dead_code)]
    acquire_timeout_ms: AtomicU64,

    /// When true, `finish_flush` probes whether the state mutex is free at the
    /// instant it calls `notify_all` (see `test_pre_notify_try_lock_ok`).
    #[cfg(test)]
    test_pre_notify_probe: std::sync::atomic::AtomicBool,
    /// Result of the most recent pre-notify probe: 0 = unset, 1 = the mutex was
    /// free (guard already dropped), 2 = it would have blocked (guard still held).
    #[cfg(test)]
    test_pre_notify_try_lock_ok: AtomicU64,
}

/// Configuration for group commit behavior.
#[derive(Debug, Clone)]
pub struct GroupCommitConfig {
    /// Maximum time to wait for more transactions before flushing.
    pub max_delay_ms: u64,
    /// Maximum transactions to batch before forcing a flush.
    pub max_batch_size: usize,

    /// Multiplier for max_delay_ms to calculate base timeout.
    pub timeout_multiplier: u32,
    /// Fixed overhead added to base timeout in milliseconds.
    pub timeout_base_ms: u64,
    /// Minimum timeout in milliseconds.
    pub timeout_min_ms: u64,
    /// Maximum timeout in milliseconds.
    pub timeout_max_ms: u64,
    /// Maximum number of recent errors to keep in history.
    pub recent_errors_capacity: usize,

    /// Bound on acquiring the group-commit state mutex itself, in milliseconds.
    ///
    /// This is a **deadlock detection mechanism, NOT a performance SLA** — the
    /// same contract `timeout_max_ms` carries for `wait_for_flush`. The default
    /// (120_000) is deliberately far larger than the `timeout_max_ms` default
    /// (60_000) so that a stuck *flusher* is always reported by the
    /// `wait_for_flush` deadlock detector first, and this bound only ever fires
    /// for a genuinely stuck *mutex*.
    ///
    /// `0` disables the bound and restores legacy unbounded blocking
    /// acquisition (`Mutex::lock`).
    pub acquire_timeout_ms: u64,
}

impl Default for GroupCommitConfig {
    fn default() -> Self {
        Self {
            max_delay_ms: 10,
            max_batch_size: 200,
            timeout_multiplier: 50,
            timeout_base_ms: 5000,
            timeout_min_ms: 10000,
            timeout_max_ms: 60000,
            recent_errors_capacity: 1024,
            acquire_timeout_ms: 120_000,
        }
    }
}

/// `pub(crate)` only because it is the `Deref` target of the `pub(crate)`
/// [`StateGuard`]; every field stays private to this module, so no code
/// outside can read or mutate group-commit state without going through the
/// coordinator's API.
pub(crate) struct GroupCommitState {
    /// Current epoch (increments on each flush)
    current_epoch: u64,
    /// Number of transactions in current batch
    batch_count: usize,
    /// Epoch that has been durably flushed
    flushed_epoch: u64,
    /// Recent flush errors, stored as (epoch, error_message).
    /// Used to verify that a specific epoch was successfully flushed.
    recent_errors: VecDeque<(u64, String)>,
    /// The oldest epoch in the recent_errors list (or older if evicted).
    /// Used to detect if we've lost history.
    oldest_error_epoch: u64,
    /// Set of completed epochs that are ahead of flushed_epoch
    completed_epochs: BTreeSet<u64>,
}

impl GroupCommitCoordinator {
    /// Create a new GroupCommitCoordinator with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `max_delay_ms` - Maximum time to wait for more transactions before flushing.
    /// * `max_batch_size` - Maximum transactions to batch before forcing a flush.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    ///
    /// // Batches up to 100 transactions, waiting at most 10ms
    /// let coordinator = GroupCommitCoordinator::new(10, 100);
    /// ```
    pub fn new(max_delay_ms: u64, max_batch_size: usize) -> Self {
        Self::with_config(GroupCommitConfig {
            max_delay_ms,
            max_batch_size,
            recent_errors_capacity: 1024,
            ..GroupCommitConfig::default()
        })
    }

    /// Create a new GroupCommitCoordinator with the given full configuration.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::{GroupCommitCoordinator, GroupCommitConfig};
    ///
    /// let config = GroupCommitConfig {
    ///     max_delay_ms: 10,
    ///     max_batch_size: 100,
    ///     ..GroupCommitConfig::default()
    /// };
    /// let coordinator = GroupCommitCoordinator::with_config(config);
    /// ```
    pub fn with_config(config: GroupCommitConfig) -> Self {
        let acquire_timeout_ms = AtomicU64::new(config.acquire_timeout_ms);
        Self {
            state: Mutex::new(GroupCommitState {
                current_epoch: 1, // Start at 1 so flushed_epoch=0 means "nothing flushed yet"
                batch_count: 0,
                flushed_epoch: 0,
                recent_errors: VecDeque::new(),
                oldest_error_epoch: 0,
                completed_epochs: BTreeSet::new(),
            }),
            flush_complete: Condvar::new(),
            config,
            owner: AtomicU64::new(0),
            owner_site: AtomicU64::new(0),
            waiters: AtomicU64::new(0),
            contended_acquires: AtomicU64::new(0),
            reentrancy_detections: AtomicU64::new(0),
            acquire_timeouts: AtomicU64::new(0),
            // Mirrors start matching the initial state above.
            mirror_current_epoch: AtomicU64::new(1),
            mirror_flushed_epoch: AtomicU64::new(0),
            mirror_batch_count: AtomicU64::new(0),
            acquire_timeout_ms,
            #[cfg(test)]
            test_pre_notify_probe: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            test_pre_notify_try_lock_ok: AtomicU64::new(0),
        }
    }

    /// Acquire the group-commit state mutex through the single chokepoint.
    ///
    /// Every method on this coordinator locks `state` through here so that
    /// ownership tracking, contention accounting, and the acquisition bound
    /// have exactly one implementation to get right.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is
    /// poisoned. Once Issue #3798 lands it additionally returns a structured
    /// `StorageError::WalError` for a re-entrant acquisition and for an
    /// acquisition that exceeds `acquire_timeout_ms`.
    pub(crate) fn lock_state(&self, site: LockSite) -> Result<StateGuard<'_>, Error> {
        // RED SCAFFOLD (#3798): this is *deliberately* the legacy behavior —
        // an unbounded, un-instrumented blocking `lock()`, exactly what all
        // eight call sites did before they were routed through here. There is
        // no re-entrancy check and no deadline, which is the defect the failing
        // tests at the bottom of this file describe. Routing the call sites
        // first keeps the GREEN change to a single function.
        let guard = self.state.lock().map_err(|_| {
            Error::Storage(StorageError::LockPoisoned {
                resource: "group_commit_state".to_string(),
            })
        })?;
        Ok(StateGuard::reassemble(self, site, guard))
    }

    /// Number of re-entrant acquisitions refused since process start.
    ///
    /// A non-zero value is always a bug in the caller: some code path holds a
    /// [`StateGuard`] and calls back into the coordinator on the same thread.
    /// It is exposed (rather than only logged) so a health check or soak test
    /// can assert it stays at zero.
    pub fn reentrancy_detections(&self) -> u64 {
        self.reentrancy_detections.load(Ordering::Relaxed)
    }

    /// Number of acquisitions abandoned at the deadline since process start.
    ///
    /// Non-zero means the state mutex was held for longer than
    /// [`GroupCommitConfig::acquire_timeout_ms`] — i.e. a suspected deadlock,
    /// not ordinary contention.
    pub fn acquire_timeouts(&self) -> u64 {
        self.acquire_timeouts.load(Ordering::Relaxed)
    }

    /// Shrink (or disable, with `0`) the acquisition bound on a live
    /// coordinator so tests do not have to wait out the two-minute default.
    #[cfg(test)]
    pub(crate) fn set_acquire_timeout_ms_for_test(&self, ms: u64) {
        self.acquire_timeout_ms.store(ms, Ordering::Relaxed);
    }

    /// Create a new GroupCommitCoordinator with default configuration.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    ///
    /// let coordinator = GroupCommitCoordinator::with_defaults();
    /// ```
    pub fn with_defaults() -> Self {
        let config = GroupCommitConfig::default();
        Self::new(config.max_delay_ms, config.max_batch_size)
    }

    /// Register a transaction for group commit.
    ///
    /// Returns the epoch number that this transaction should wait for.
    /// If the batch is full, returns `true` in the second element to signal
    /// that an immediate flush should be triggered.
    ///
    /// # Returns
    ///
    /// A tuple of (epoch_to_wait_for, should_trigger_flush)
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    pub fn register_transaction(&self) -> Result<(u64, bool), Error> {
        let mut state = self.lock_state(LockSite::RegisterTransaction)?;

        state.batch_count += 1;
        let epoch = state.current_epoch;

        // Check if we should trigger immediate flush (batch full)
        let should_flush = state.batch_count >= self.config.max_batch_size;

        Ok((epoch, should_flush))
    }

    /// Wait for the specified epoch to be flushed.
    ///
    /// This blocks until the epoch has been durably flushed. If the flush
    /// failed, the error is propagated to all waiting transactions.
    ///
    /// # Timeout Calculation
    ///
    /// The timeout is a **deadlock detection mechanism**, not a performance target.
    /// It's designed to catch stuck flush threads, not to enforce timing.
    ///
    /// Formula: `clamp(max_delay_ms * multiplier + base, min, max)`
    /// - multiplier: Allows for thread scheduling overhead
    /// - base: Fixed overhead for thread startup
    /// - min: Handles very fast configs in slow CI
    /// - max: Prevents indefinite waiting on stuck threads
    ///
    /// The timeout accounts for:
    /// - Environments with unpredictable thread scheduling (e.g., CI runners)
    /// - Systems under heavy load causing thread starvation
    /// - Variable I/O latency for disk flushes
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The flush for this epoch failed
    /// - The wait times out (indicates a stuck flush thread or excessive system load)
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    /// # let coordinator = GroupCommitCoordinator::new(10, 100);
    /// # let (epoch, _) = coordinator.register_transaction().unwrap();
    /// coordinator.wait_for_flush(epoch).unwrap();
    /// ```
    pub fn wait_for_flush(&self, epoch: u64) -> Result<(), Error> {
        // `Condvar::wait_timeout` consumes a raw `MutexGuard`, and a thread
        // parked in the condvar holds no lock — so ownership tracking is
        // surrendered for the whole body here. GREEN re-arms it via
        // `StateGuard::reassemble` on each return from the wait; RED keeps the
        // pre-existing raw-guard flow byte for byte.
        let mut state = self.lock_state(LockSite::WaitForFlush)?.surrender();

        // Deadlock detection timeout (NOT a performance SLA)
        let base_timeout =
            Duration::from_millis(self.config.max_delay_ms * self.config.timeout_multiplier as u64)
                + Duration::from_millis(self.config.timeout_base_ms);
        let timeout = base_timeout
            .max(Duration::from_millis(self.config.timeout_min_ms))
            .min(Duration::from_millis(self.config.timeout_max_ms));

        // Use a deadline to prevent spurious wakeups from resetting the timeout clock
        let deadline = std::time::Instant::now() + timeout;

        // RACE CONDITION SAFETY: If the epoch was already flushed between register_transaction()
        // and this wait (rare but possible on fast systems), this loop exits immediately since
        // flushed_epoch >= epoch, and we return Ok(()) without waiting.
        //
        // EPOCH SEMANTICS: flushed_epoch = N means "epoch N has been flushed".
        // Transaction at epoch E waits while flushed_epoch < E (i.e., E has not been flushed yet).
        while state.flushed_epoch < epoch {
            let now = std::time::Instant::now();
            let remaining = if now >= deadline {
                Duration::from_secs(0)
            } else {
                deadline - now
            };

            if remaining.as_nanos() == 0 {
                return Err(Error::Storage(StorageError::WalError {
                    reason: format!(
                        "Group commit timeout waiting for epoch {} (current flushed: {})",
                        epoch, state.flushed_epoch
                    ),
                }));
            }

            let (new_state, timeout_result) = self
                .flush_complete
                .wait_timeout(state, remaining)
                .map_err(|_| {
                    Error::Storage(StorageError::LockPoisoned {
                        resource: "group_commit_state".to_string(),
                    })
                })?;

            state = new_state;

            // Check if we timed out (either by Condvar result OR by deadline)
            if (timeout_result.timed_out() || std::time::Instant::now() >= deadline)
                && state.flushed_epoch < epoch
            {
                return Err(Error::Storage(StorageError::WalError {
                    reason: format!(
                        "Group commit timeout waiting for epoch {} (current flushed: {})",
                        epoch, state.flushed_epoch
                    ),
                }));
            }
        }

        // Check for flush errors specifically for this epoch
        for (failed_epoch, error_msg) in &state.recent_errors {
            if *failed_epoch == epoch {
                return Err(Error::Storage(StorageError::WalError {
                    reason: format!("Group commit flush failed: {}", error_msg),
                }));
            }
        }

        // Check for history eviction (False Success protection)
        // We only lose certainty about an epoch's status if its error record was EVICTED.
        // state.oldest_error_epoch tracks the threshold of lost history.
        // If epoch < state.oldest_error_epoch, it might have failed and been forgotten.
        //
        // Note: We do NOT check against recent_errors.front() because a sparse error list
        // is valid. If epoch 1 succeeded and epoch 2 failed, recent_errors = [(2, ...)].
        // epoch 1 < 2, but it shouldn't fail unless 1 < oldest_error_epoch.

        if epoch < state.oldest_error_epoch {
            return Err(Error::Storage(StorageError::WalError {
                reason: format!(
                    "Group commit status unknown: epoch {} evicted from error history (history starts at {})",
                    epoch, state.oldest_error_epoch
                ),
            }));
        }

        Ok(())
    }

    /// Start a flush operation.
    ///
    /// This method MUST be called before the actual flush begins. It advances
    /// the `current_epoch` so that any new transactions registering during the
    /// flush operation are assigned to the *next* epoch, preventing race conditions
    /// where late-arriving transactions are marked as flushed without actually being written.
    ///
    /// # Returns
    ///
    /// The epoch number that is being flushed.
    pub fn start_flush(&self) -> Result<u64, Error> {
        let mut state = self.lock_state(LockSite::StartFlush)?;

        let epoch_to_flush = state.current_epoch;

        // Advance current epoch immediately so new transactions go to the next one
        state.current_epoch += 1;
        state.batch_count = 0;

        Ok(epoch_to_flush)
    }

    /// Finish a flush operation and notify waiters.
    ///
    /// Called by the flush thread after completing a flush.
    ///
    /// # Arguments
    ///
    /// * `epoch` - The epoch that was flushed (returned by `start_flush`).
    /// * `result` - The result of the flush operation.
    pub fn finish_flush(&self, epoch: u64, result: Result<(), Error>) -> Result<(), Error> {
        let mut state = self.lock_state(LockSite::FinishFlush)?;

        // Store error if any
        if let Err(e) = result {
            state.recent_errors.push_back((epoch, e.to_string()));

            // Keep history limited
            while state.recent_errors.len() > self.config.recent_errors_capacity {
                if let Some((evicted_epoch, _)) = state.recent_errors.pop_front() {
                    // Track the newest evicted epoch to know what we've lost
                    // We set oldest_error_epoch to evicted_epoch + 1 because if
                    // evicted_epoch is gone, any check for it (or older) is invalid.
                    state.oldest_error_epoch = evicted_epoch + 1;
                }
            }
        }

        // Mark this epoch as completed
        if epoch > state.flushed_epoch {
            state.completed_epochs.insert(epoch);
        }

        // Advance flushed_epoch contiguously to wake up waiters
        // We only advance if we have a contiguous sequence of completed epochs.
        // This prevents data loss scenarios where a later successful flush
        // could mask an earlier failed (or still pending) flush.
        let mut next_epoch = state.flushed_epoch + 1;
        while state.completed_epochs.contains(&next_epoch) {
            state.completed_epochs.remove(&next_epoch);
            state.flushed_epoch = next_epoch;
            next_epoch += 1;
        }

        // Wake all waiting transactions.
        //
        // RED SCAFFOLD (#3798): the state guard is STILL HELD here, so every
        // woken waiter immediately blocks again on the mutex this thread owns
        // (a thundering herd against a held lock). GREEN drops the guard
        // first; no wakeup can be lost because a waiter re-checks
        // `flushed_epoch < epoch` under the mutex after `wait_timeout` returns.
        #[cfg(test)]
        if self
            .test_pre_notify_probe
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            // Deterministic, single-threaded probe of exactly that property:
            // can the mutex be taken at the instant `notify_all` runs?
            let observed = match self.state.try_lock() {
                Ok(_) => 1,
                Err(_) => 2,
            };
            self.test_pre_notify_try_lock_ok
                .store(observed, Ordering::Relaxed);
        }
        self.flush_complete.notify_all();

        Ok(())
    }

    /// Mark the current batch as flushed (Legacy/Combined Helper).
    ///
    /// WARNING: This method combines `start_flush` and `finish_flush` but IS NOT SAFE
    /// against the race condition described in security review (transactions registering
    /// during the flush).
    ///
    /// It is kept for backward compatibility with existing tests but `start_flush`
    /// and `finish_flush` should be used in production.
    pub fn mark_flushed(&self, result: Result<(), Error>) -> Result<(), Error> {
        let epoch = self.start_flush()?;
        self.finish_flush(epoch, result)
    }

    /// Get the current batch size.
    ///
    /// Useful for monitoring and testing to see how many transactions are currently
    /// waiting in the active epoch to be flushed.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    ///
    /// let coordinator = GroupCommitCoordinator::with_defaults();
    /// let size = coordinator.current_batch_size().unwrap();
    /// assert_eq!(size, 0);
    /// ```
    pub fn current_batch_size(&self) -> Result<usize, Error> {
        let state = self.lock_state(LockSite::CurrentBatchSize)?;
        Ok(state.batch_count)
    }

    /// Get the current epoch.
    ///
    /// Useful for monitoring and testing to determine which epoch incoming
    /// transactions will be assigned to.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    ///
    /// let coordinator = GroupCommitCoordinator::with_defaults();
    /// let epoch = coordinator.current_epoch().unwrap();
    /// assert_eq!(epoch, 1); // Starts at 1
    /// ```
    pub fn current_epoch(&self) -> Result<u64, Error> {
        let state = self.lock_state(LockSite::CurrentEpoch)?;
        Ok(state.current_epoch)
    }

    /// Get the last flushed epoch.
    ///
    /// Useful for monitoring and testing to see how far along the durability
    /// frontier has advanced.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    ///
    /// let coordinator = GroupCommitCoordinator::with_defaults();
    /// let flushed = coordinator.flushed_epoch().unwrap();
    /// assert_eq!(flushed, 0); // Nothing flushed yet
    /// ```
    pub fn flushed_epoch(&self) -> Result<u64, Error> {
        let state = self.lock_state(LockSite::FlushedEpoch)?;
        Ok(state.flushed_epoch)
    }

    /// Check if a flush should be triggered based on batch size.
    ///
    /// Called by the flush thread to check if the batch is full and should be flushed
    /// immediately, ignoring the `max_delay_ms` timeout.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::LockPoisoned` if the coordinator lock is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    ///
    /// let coordinator = GroupCommitCoordinator::with_defaults();
    /// let flush_now = coordinator.should_flush().unwrap();
    /// assert_eq!(flush_now, false);
    /// ```
    pub fn should_flush(&self) -> Result<bool, Error> {
        let state = self.lock_state(LockSite::ShouldFlush)?;
        Ok(state.batch_count >= self.config.max_batch_size)
    }

    /// Get the maximum delay for this coordinator.
    ///
    /// Returns the configured `max_delay_ms` as a `Duration`.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::wal::group_commit::GroupCommitCoordinator;
    /// use std::time::Duration;
    ///
    /// let coordinator = GroupCommitCoordinator::new(10, 100);
    /// assert_eq!(coordinator.max_delay(), Duration::from_millis(10));
    /// ```
    pub fn max_delay(&self) -> Duration {
        Duration::from_millis(self.config.max_delay_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
    use std::thread;

    /// Upper bound on how long any cross-thread probe may take to answer.
    ///
    /// Every assertion in this module is a *fast* one (microseconds of real
    /// work, or a deliberately shortened 200ms acquisition bound), so five
    /// seconds is pure slack for a loaded CI runner.
    const WATCHDOG: Duration = Duration::from_secs(5);

    /// Run `f` on a detached worker thread and hand back its result channel.
    ///
    /// WHY NOT `JoinHandle::join()`: the property under test is *"does this
    /// call ever return?"*. A test that joined a thread stuck in
    /// `Mutex::lock()` would hang the whole suite forever instead of failing,
    /// so every cross-thread probe here goes through a channel with a
    /// `recv_timeout`. Leaking a worker that never answers is deliberate and
    /// acceptable: this is a test binary that exits when the suite finishes.
    fn spawn_probe<T, F>(f: F) -> Receiver<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx
    }

    /// [`spawn_probe`] plus the watchdog wait, for the common case.
    fn run_with_watchdog<T, F>(f: F) -> Result<T, RecvTimeoutError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        spawn_probe(f).recv_timeout(WATCHDOG)
    }

    /// Park a worker thread holding a [`StateGuard`] taken at `site`.
    ///
    /// Returns `(release, holder)`: send on `release` to let the holder drop
    /// the guard, then join `holder`. The call does not return until the guard
    /// is genuinely held, so callers never need a sleep to avoid a race.
    fn hold_state_guard(
        coord: &Arc<GroupCommitCoordinator>,
        site: LockSite,
    ) -> (mpsc::Sender<()>, thread::JoinHandle<()>) {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (held_tx, held_rx) = mpsc::channel::<()>();
        let coord = Arc::clone(coord);
        let holder = thread::spawn(move || {
            let _guard = coord.lock_state(site).expect("holder acquires the guard");
            let _ = held_tx.send(());
            // Bounded so a failing test cannot wedge the suite: far longer
            // than WATCHDOG, so it never releases early either.
            let _ = release_rx.recv_timeout(Duration::from_secs(60));
        });
        held_rx
            .recv_timeout(WATCHDOG)
            .expect("holder thread never reported holding the guard");
        (release_tx, holder)
    }

    // ==================== Issue #3798: lock-acquisition hardening ====================
    //
    // These specs describe the coordinator AFTER #3798. Most of them fail
    // today, by design; the two that pass are labelled in place, because a
    // suite where "all new tests fail" is as untrustworthy as one where none
    // do — the passing ones pin invariants the fix must NOT break.

    /// A thread that already holds the state guard and calls back into the
    /// coordinator must get a structured error, not a self-deadlock.
    ///
    /// RED: today `register_transaction` calls `Mutex::lock()` on a mutex this
    /// very thread owns, which blocks forever — the watchdog fires and this
    /// test fails on the `expect` below.
    ///
    /// The acquisition bound is shortened first so that an implementation
    /// which only *bounds* acquisition (without detecting re-entrancy) still
    /// fails here: the assertion is on the word "re-entrant", which a bare
    /// timeout message does not contain. Re-entrancy is a caller bug that must
    /// be reported immediately, not two minutes later.
    ///
    /// The `ALETHEIADB_WAL_REENTRANCY_PANIC=1` CI-hardening escalation is
    /// deliberately NOT covered by a test of its own: setting an environment
    /// variable is process-global and would race every other test in the
    /// binary. This test pins the contractually important half — the default
    /// path returns `Err`, it never panics.
    #[test]
    fn test_reentrant_acquisition_returns_structured_error() {
        let coord = Arc::new(GroupCommitCoordinator::with_defaults());
        coord.set_acquire_timeout_ms_for_test(200);

        let probe = Arc::clone(&coord);
        let outcome = run_with_watchdog(move || {
            let _held = probe
                .lock_state(LockSite::CurrentEpoch)
                .expect("first acquisition succeeds");
            // Same thread, guard still held: this is the re-entrant call.
            probe.register_transaction()
        });

        let result = outcome.expect(
            "current code self-deadlocks on re-entrant acquisition: the worker never \
             returned within the watchdog window",
        );
        let err = result.expect_err("re-entrant acquisition must be refused, not granted");
        let msg = err.to_string();
        assert!(
            msg.contains("re-entrant"),
            "error must name re-entrancy (a bare acquisition timeout is not good enough): {msg}"
        );
        assert!(
            msg.contains("group_commit_state"),
            "error must name the resource: {msg}"
        );
        assert!(
            msg.contains("thread token"),
            "error must report the offending thread token: {msg}"
        );
        assert_eq!(
            coord.reentrancy_detections(),
            1,
            "the refusal must be counted"
        );
    }

    /// Ownership tracking must not survive a condvar wait.
    ///
    /// `Condvar::wait_timeout` consumes the guard and unlocks the mutex while
    /// the thread parks. If the owner token were left armed across that wait,
    /// the very next acquisition by the same thread would be misreported as
    /// re-entrant — turning a correct commit path into a permanent error.
    ///
    /// The first half of this test passes both before and after the fix (it is
    /// the false-positive guard). The final step is the red-meaningful one: it
    /// re-enters on purpose and demands the structured refusal, which today
    /// deadlocks instead — hence the watchdog.
    #[test]
    fn test_owner_cleared_across_condvar_wait_then_same_thread_reacquires() {
        let config = GroupCommitConfig {
            max_delay_ms: 1,
            max_batch_size: 100,
            timeout_multiplier: 1,
            timeout_base_ms: 10,
            timeout_min_ms: 50,
            timeout_max_ms: 60,
            ..GroupCommitConfig::default()
        };
        let coord = Arc::new(GroupCommitCoordinator::with_config(config));

        let (epoch, _) = coord.register_transaction().unwrap();

        // Nobody flushes, so this parks in the condvar and times out (~50ms).
        let waited = coord.wait_for_flush(epoch);
        assert!(waited.is_err(), "expected the deadlock-detection timeout");
        assert!(waited.unwrap_err().to_string().contains("timeout"));

        // Same thread, immediately afterwards: both must be plain successes.
        assert!(
            coord.register_transaction().is_ok(),
            "the waiter's own next acquisition must not be seen as re-entrant"
        );
        assert!(coord.current_epoch().is_ok());
        assert_eq!(
            coord.reentrancy_detections(),
            0,
            "a condvar wait is not a re-entrant acquisition"
        );

        // Genuine re-entrancy, by contrast, must still be refused — proving
        // the clean pass above came from correct bookkeeping and not from the
        // check being absent entirely.
        let probe = Arc::clone(&coord);
        let outcome = run_with_watchdog(move || {
            let _held = probe
                .lock_state(LockSite::CurrentEpoch)
                .expect("acquisition succeeds");
            probe.current_batch_size()
        });
        let result = outcome
            .expect("current code self-deadlocks on re-entrant acquisition (no watchdog answer)");
        let err = result.expect_err("re-entrant acquisition must be refused");
        assert!(
            err.to_string().contains("re-entrant"),
            "expected a re-entrancy refusal, got: {err}"
        );
    }

    /// Heavy honest contention must never be mistaken for a deadlock.
    ///
    /// NOTE: this test PASSES both before and after the fix, on purpose. Not
    /// every test in a red phase should fail — this one is the negative guard
    /// that keeps the fix from "detecting" deadlocks that are really just
    /// eight threads doing their job, and would catch a green implementation
    /// whose acquisition bound or owner bookkeeping is too eager.
    #[test]
    fn test_no_false_reentrancy_or_timeout_under_concurrent_load() {
        const THREADS: usize = 8;
        const ITERS: usize = 2000;

        let coord = Arc::new(GroupCommitCoordinator::with_defaults());
        let barrier = Arc::new(Barrier::new(THREADS));

        let mut handles = Vec::with_capacity(THREADS);
        for thread_idx in 0..THREADS {
            let coord = Arc::clone(&coord);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || -> Result<(), Error> {
                barrier.wait();
                for i in 0..ITERS {
                    coord.register_transaction()?;
                    coord.current_batch_size()?;
                    // One thread also drives the flush path, so the observation
                    // and flusher sites contend with the commit path too.
                    if thread_idx == 0 && i % 100 == 0 {
                        let epoch = coord.start_flush()?;
                        coord.finish_flush(epoch, Ok(()))?;
                    }
                }
                Ok(())
            }));
        }

        // Safe to join: none of these threads holds a guard across a call, so
        // no ordering of them can deadlock even on today's code.
        for handle in handles {
            handle
                .join()
                .expect("worker panicked")
                .expect("no acquisition may fail under honest contention");
        }

        assert_eq!(
            coord.reentrancy_detections(),
            0,
            "contention is not re-entrancy"
        );
        assert_eq!(
            coord.acquire_timeouts(),
            0,
            "contention is not a suspected deadlock"
        );
    }

    /// When another thread really is wedged holding the guard, the blocked
    /// caller must be told enough to diagnose it without a debugger.
    ///
    /// RED: today the second caller blocks in `Mutex::lock()` forever and the
    /// watchdog fires.
    ///
    /// `register_transaction` is on the commit path, so the message must also
    /// disclose that durability is UNKNOWN — the caller cannot tell whether
    /// its transaction will be replayed at recovery, and must not report a
    /// clean failure to its own client.
    #[test]
    fn test_acquisition_returns_diagnosable_error_when_guard_held_elsewhere() {
        let coord = Arc::new(GroupCommitCoordinator::with_defaults());
        coord.set_acquire_timeout_ms_for_test(200);

        let (release, holder) = hold_state_guard(&coord, LockSite::StartFlush);

        let probe = Arc::clone(&coord);
        let outcome = run_with_watchdog(move || probe.register_transaction());

        let result = outcome.expect(
            "current code blocks forever behind a wedged holder: the acquisition is unbounded",
        );
        let err = result.expect_err("acquisition behind a wedged holder must fail, not hang");
        let msg = err.to_string();
        for needle in [
            "group_commit_state",
            "register_transaction",
            "possible deadlock",
            "waited",
            "holder thread token",
            "waiters",
            "current_epoch",
            "flushed_epoch",
            "batch_count",
            "durability status UNKNOWN",
        ] {
            assert!(
                msg.contains(needle),
                "diagnostic payload is missing {needle:?}: {msg}"
            );
        }
        assert_eq!(coord.acquire_timeouts(), 1, "the timeout must be counted");

        let _ = release.send(());
        holder.join().expect("holder panicked");
    }

    /// The durability disclosure belongs ONLY to the commit-path sites.
    ///
    /// `current_batch_size` is an observation: nothing was written, so telling
    /// its caller that "durability is unknown" would be a false alarm that an
    /// operator (or an LLM reading the error) would escalate for nothing.
    ///
    /// RED: today the observation call blocks forever and the watchdog fires.
    #[test]
    fn test_acquisition_timeout_on_flush_side_site_has_no_durability_disclosure() {
        let coord = Arc::new(GroupCommitCoordinator::with_defaults());
        coord.set_acquire_timeout_ms_for_test(200);

        let (release, holder) = hold_state_guard(&coord, LockSite::StartFlush);

        let probe = Arc::clone(&coord);
        let outcome = run_with_watchdog(move || probe.current_batch_size());

        let result = outcome.expect(
            "current code blocks forever behind a wedged holder: the acquisition is unbounded",
        );
        let err = result.expect_err("acquisition behind a wedged holder must fail, not hang");
        let msg = err.to_string();
        assert!(
            !msg.contains("durability status UNKNOWN"),
            "an observation site wrote nothing, so it must not claim durability is at risk: {msg}"
        );
        assert!(
            msg.contains("group_commit_state"),
            "error must name the resource: {msg}"
        );
        assert!(
            msg.contains(LockSite::CurrentBatchSize.as_str()),
            "error must name the blocked site: {msg}"
        );

        let _ = release.send(());
        holder.join().expect("holder panicked");
    }

    /// The acquisition bound must never fire before the `wait_for_flush`
    /// deadlock detector.
    ///
    /// NOTE: this test PASSES in the red phase — the field is scaffolded with
    /// its final default. It is here to pin the ordering invariant so a later
    /// "let's make the timeout snappier" tweak cannot silently make a stuck
    /// flusher surface as an inscrutable lock error instead of the purpose-built
    /// group-commit timeout.
    #[test]
    fn test_acquire_timeout_default_strictly_exceeds_wait_timeout_max() {
        let config = GroupCommitConfig::default();
        assert!(
            config.acquire_timeout_ms > config.timeout_max_ms,
            "acquire_timeout_ms ({}) must strictly exceed timeout_max_ms ({})",
            config.acquire_timeout_ms,
            config.timeout_max_ms
        );
    }

    /// `acquire_timeout_ms = 0` is the documented escape hatch back to legacy
    /// unbounded blocking.
    ///
    /// NOTE: this test PASSES in the red phase — unbounded blocking is exactly
    /// what today's code does. It is here so the green implementation keeps the
    /// opt-out honest: an operator who disables the bound must get blocking,
    /// not a bound with a different number.
    #[test]
    fn test_acquire_timeout_zero_restores_unbounded_blocking() {
        let coord = Arc::new(GroupCommitCoordinator::with_defaults());
        coord.set_acquire_timeout_ms_for_test(0);

        let (release, holder) = hold_state_guard(&coord, LockSite::StartFlush);

        let probe = Arc::clone(&coord);
        let rx = spawn_probe(move || probe.register_transaction());

        // Longer than any plausible misfiring bound (the other tests use
        // 200ms), and the guard is still held throughout.
        thread::sleep(Duration::from_millis(400));
        assert!(
            matches!(rx.try_recv(), Err(TryRecvError::Empty)),
            "with the bound disabled the acquisition must still be blocking"
        );

        let _ = release.send(());
        holder.join().expect("holder panicked");

        let result = rx
            .recv_timeout(WATCHDOG)
            .expect("the blocked acquisition must complete once the guard is released");
        assert!(
            result.is_ok(),
            "a blocking acquisition that finally wins the mutex must succeed: {result:?}"
        );
    }

    /// `finish_flush` must release the state guard BEFORE notifying waiters.
    ///
    /// RED: today the guard is still held at `notify_all`, so every woken
    /// waiter immediately blocks again on the mutex the notifier owns — a
    /// thundering herd against a held lock, and the probe records 2.
    ///
    /// Fully deterministic and single-threaded: the probe try-locks from
    /// inside `finish_flush` itself, so there is no timing window to lose.
    #[test]
    fn test_finish_flush_does_not_hold_state_guard_while_notifying() {
        let coord = GroupCommitCoordinator::with_defaults();
        coord
            .test_pre_notify_probe
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let epoch = coord.start_flush().unwrap();
        coord.finish_flush(epoch, Ok(())).unwrap();

        assert_eq!(
            coord.test_pre_notify_try_lock_ok.load(Ordering::Relaxed),
            1,
            "finish_flush still held the state guard while calling notify_all \
             (probe: 1 = mutex free, 2 = would block, 0 = probe never ran)"
        );
    }

    // ==================== Original Tests (Updated for Result-based API) ====================

    #[test]
    fn test_new_coordinator() {
        let coord = GroupCommitCoordinator::new(10, 200);
        assert_eq!(coord.current_epoch().unwrap(), 1); // Start at 1 (flushed_epoch=0 means nothing flushed)
        assert_eq!(coord.flushed_epoch().unwrap(), 0);
        assert_eq!(coord.current_batch_size().unwrap(), 0);
    }

    #[test]
    fn test_register_transaction() {
        let coord = GroupCommitCoordinator::new(10, 5);

        // Register transactions
        for i in 0..4 {
            let (epoch, should_flush) = coord.register_transaction().unwrap();
            assert_eq!(epoch, 1); // All in epoch 1
            assert!(!should_flush, "should not flush at batch size {}", i + 1);
        }

        // Fifth transaction should trigger flush
        let (epoch, should_flush) = coord.register_transaction().unwrap();
        assert_eq!(epoch, 1); // Still in epoch 1
        assert!(should_flush, "should flush when batch is full");
    }

    #[test]
    fn test_mark_flushed_advances_epoch() {
        let coord = GroupCommitCoordinator::new(10, 100);

        coord.register_transaction().unwrap();
        coord.register_transaction().unwrap();

        assert_eq!(coord.current_epoch().unwrap(), 1); // Start at epoch 1
        assert_eq!(coord.current_batch_size().unwrap(), 2);

        coord.mark_flushed(Ok(())).unwrap();

        assert_eq!(coord.current_epoch().unwrap(), 2); // Advances to epoch 2
        assert_eq!(coord.flushed_epoch().unwrap(), 1); // Epoch 1 has been flushed
        assert_eq!(coord.current_batch_size().unwrap(), 0);
    }

    #[test]
    fn test_wait_for_flush_success() {
        let coord = Arc::new(GroupCommitCoordinator::new(100, 100));
        let coord_clone = Arc::clone(&coord);

        let (epoch, _) = coord.register_transaction().unwrap();

        // Spawn a thread to mark flushed after a short delay
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            coord_clone.mark_flushed(Ok(())).unwrap();
        });

        // Wait should succeed
        let result = coord.wait_for_flush(epoch);
        assert!(result.is_ok());

        handle.join().unwrap();
    }

    #[test]
    fn test_wait_for_flush_error_propagation() {
        let coord = Arc::new(GroupCommitCoordinator::new(100, 100));
        let coord_clone = Arc::clone(&coord);

        let (epoch, _) = coord.register_transaction().unwrap();

        // Spawn a thread to mark flushed with error
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            coord_clone
                .mark_flushed(Err(Error::Storage(StorageError::WalError {
                    reason: "disk full".to_string(),
                })))
                .unwrap();
        });

        // Wait should return the error
        let result = coord.wait_for_flush(epoch);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("disk full"));

        handle.join().unwrap();
    }

    #[test]
    fn test_wait_for_flush_timeout() {
        // Use a config with very short timeout for testing
        let config = GroupCommitConfig {
            max_delay_ms: 10,
            max_batch_size: 100,
            timeout_multiplier: 2, // 10 * 2 = 20ms
            timeout_base_ms: 10,   // + 10 = 30ms
            timeout_min_ms: 20,    // clamp min
            timeout_max_ms: 100,   // clamp max
            recent_errors_capacity: 1024,
            ..GroupCommitConfig::default()
        };
        let coord = GroupCommitCoordinator::with_config(config);

        let (epoch, _) = coord.register_transaction().unwrap();

        // Wait without anyone calling mark_flushed - should timeout quickly
        let start = std::time::Instant::now();
        let result = coord.wait_for_flush(epoch);
        let elapsed = start.elapsed();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("timeout"));

        // Verify it didn't take 10 seconds (default timeout)
        assert!(elapsed < Duration::from_millis(500));
    }

    #[test]
    fn test_multiple_waiters() {
        let coord = Arc::new(GroupCommitCoordinator::new(100, 100));

        // Register multiple transactions
        let mut epochs = Vec::new();
        for _ in 0..5 {
            let (epoch, _) = coord.register_transaction().unwrap();
            epochs.push(epoch);
        }

        // All should be same epoch (epoch 1)
        assert!(epochs.iter().all(|&e| e == 1));

        // Spawn multiple waiting threads
        let mut handles = Vec::new();
        for _ in 0..5 {
            let coord_clone = Arc::clone(&coord);
            handles.push(thread::spawn(move || coord_clone.wait_for_flush(1))); // Wait for epoch 1
        }

        // Let them start waiting
        thread::sleep(Duration::from_millis(10));

        // Mark flushed
        coord.mark_flushed(Ok(())).unwrap();

        // All should succeed
        for handle in handles {
            let result = handle.join().unwrap();
            assert!(result.is_ok());
        }
    }

    #[test]
    fn test_multiple_epochs() {
        let coord = GroupCommitCoordinator::new(10, 100);

        // First batch at epoch 1
        coord.register_transaction().unwrap();
        coord.register_transaction().unwrap();
        coord.mark_flushed(Ok(())).unwrap();

        assert_eq!(coord.current_epoch().unwrap(), 2); // Advanced to epoch 2

        // Second batch at epoch 2
        let (epoch, _) = coord.register_transaction().unwrap();
        assert_eq!(epoch, 2);

        coord.mark_flushed(Ok(())).unwrap();
        assert_eq!(coord.current_epoch().unwrap(), 3); // Advanced to epoch 3
    }

    #[test]
    fn test_should_flush() {
        let coord = GroupCommitCoordinator::new(10, 3);

        assert!(!coord.should_flush().unwrap());

        coord.register_transaction().unwrap();
        assert!(!coord.should_flush().unwrap());

        coord.register_transaction().unwrap();
        assert!(!coord.should_flush().unwrap());

        coord.register_transaction().unwrap();
        assert!(coord.should_flush().unwrap());
    }

    #[test]
    fn test_max_delay() {
        let coord = GroupCommitCoordinator::new(42, 100);
        assert_eq!(coord.max_delay(), Duration::from_millis(42));
    }

    #[test]
    fn test_with_defaults() {
        let coord = GroupCommitCoordinator::with_defaults();
        assert_eq!(coord.max_delay(), Duration::from_millis(10));
        // Can't easily test max_batch_size without registering 200 transactions
    }

    #[test]
    fn test_custom_timeout_config() {
        let config = GroupCommitConfig {
            max_delay_ms: 1,
            max_batch_size: 100,
            timeout_multiplier: 2,
            timeout_base_ms: 10,
            timeout_min_ms: 20,
            timeout_max_ms: 100,
            recent_errors_capacity: 1024,
            ..GroupCommitConfig::default()
        };
        let coord = GroupCommitCoordinator::with_config(config);

        let (epoch, _) = coord.register_transaction().unwrap();

        // Formula: clamp(1 * 2 + 10, 20, 100) = 20ms
        // Wait without anyone calling mark_flushed - should timeout quickly
        let start = std::time::Instant::now();
        let result = coord.wait_for_flush(epoch);
        let elapsed = start.elapsed();

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
        // Should be around 20ms. Let's check it's within a reasonable range.
        // We use a generous upper bound for slow CI.
        // We use a relaxed lower bound (5ms) to handle Windows CI scheduler jitter.
        assert!(elapsed >= Duration::from_millis(5));
        assert!(elapsed < Duration::from_millis(500));
    }

    #[test]
    fn test_error_history_eviction() {
        // Config with very small history
        let config = GroupCommitConfig {
            max_delay_ms: 10,
            max_batch_size: 100,
            timeout_multiplier: 2,
            timeout_base_ms: 10,
            timeout_min_ms: 20,
            timeout_max_ms: 1000,
            recent_errors_capacity: 2, // Only keep 2 recent errors
            ..GroupCommitConfig::default()
        };
        let coord = GroupCommitCoordinator::with_config(config);

        // Epoch 1 fails
        let epoch1 = coord.start_flush().unwrap();
        coord
            .finish_flush(
                epoch1,
                Err(Error::Storage(StorageError::WalError {
                    reason: "Fail 1".to_string(),
                })),
            )
            .unwrap();

        // Epoch 2 fails
        let epoch2 = coord.start_flush().unwrap();
        coord
            .finish_flush(
                epoch2,
                Err(Error::Storage(StorageError::WalError {
                    reason: "Fail 2".to_string(),
                })),
            )
            .unwrap();

        // Epoch 3 fails (Evicts Epoch 1)
        let epoch3 = coord.start_flush().unwrap();
        coord
            .finish_flush(
                epoch3,
                Err(Error::Storage(StorageError::WalError {
                    reason: "Fail 3".to_string(),
                })),
            )
            .unwrap();

        // Check Epoch 1 - Should be unknown/evicted
        let result1 = coord.wait_for_flush(epoch1);
        assert!(result1.is_err());
        assert!(
            result1
                .unwrap_err()
                .to_string()
                .contains("evicted from error history")
        );

        // Check Epoch 2 - Should be known error
        let result2 = coord.wait_for_flush(epoch2);
        assert!(result2.is_err());
        assert!(result2.unwrap_err().to_string().contains("Fail 2"));

        // Check Epoch 3 - Should be known error
        let result3 = coord.wait_for_flush(epoch3);
        assert!(result3.is_err());
        assert!(result3.unwrap_err().to_string().contains("Fail 3"));
    }

    #[test]
    fn test_flush_race_condition() {
        let coord = GroupCommitCoordinator::new(100, 100);

        // 1. Transaction A registers (Epoch 1)
        let (epoch_a, _) = coord.register_transaction().unwrap();
        assert_eq!(epoch_a, 1);

        // 2. Flush starts (Epoch 1 is being flushed)
        // This advances current_epoch to 2
        let flushing_epoch = coord.start_flush().unwrap();
        assert_eq!(flushing_epoch, 1);
        assert_eq!(coord.current_epoch().unwrap(), 2);

        // 3. Transaction B registers (Should be Epoch 2)
        // Because start_flush advanced the epoch, B is not part of the current flush
        let (epoch_b, _) = coord.register_transaction().unwrap();
        assert_eq!(epoch_b, 2);

        // 4. Flush finishes successfully
        coord.finish_flush(flushing_epoch, Ok(())).unwrap();

        // 5. Transaction A should be done
        assert!(coord.wait_for_flush(epoch_a).is_ok());

        // 6. Transaction B should NOT be done (it needs Epoch 2 flush)
        // We expect it to timeout if we wait, but we can just check flushed_epoch
        assert_eq!(coord.flushed_epoch().unwrap(), 1);

        // 7. Flush Epoch 2
        let flushing_epoch_2 = coord.start_flush().unwrap();
        assert_eq!(flushing_epoch_2, 2);
        coord.finish_flush(flushing_epoch_2, Ok(())).unwrap();

        // 8. Transaction B should be done
        assert!(coord.wait_for_flush(epoch_b).is_ok());
    }

    #[test]
    fn test_wait_for_flush_deadline_enforcement() {
        // Config: timeout ~100ms
        let config = GroupCommitConfig {
            max_delay_ms: 10,
            max_batch_size: 100,
            timeout_multiplier: 1, // 10ms
            timeout_base_ms: 10,   // + 10ms = 20ms
            timeout_min_ms: 50,    // clamp min -> 50ms
            timeout_max_ms: 200,   // clamp max -> 200ms
            recent_errors_capacity: 1024,
            ..GroupCommitConfig::default()
        };

        let coord = Arc::new(GroupCommitCoordinator::with_config(config));
        let coord_clone = Arc::clone(&coord);

        // Register a transaction for Epoch 1
        let (epoch, _) = coord.register_transaction().unwrap();

        // Spawn a thread that keeps triggering "spurious" wakeups every 10ms
        thread::spawn(move || {
            let start = std::time::Instant::now();
            // Run for 500ms (10x the timeout)
            while start.elapsed() < Duration::from_millis(500) {
                thread::sleep(Duration::from_millis(10));
                // Finish a future epoch (100) triggers notify_all but doesn't advance flushed_epoch
                let _ = coord_clone.finish_flush(100, Ok(()));
            }
        });

        let start = std::time::Instant::now();
        let result = coord.wait_for_flush(epoch);
        let elapsed = start.elapsed();

        // Should fail fast (~50ms) with timeout error
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));

        // If bug exists, it will wait ~500ms (or whatever the thread runs for)
        // If fix works, it will timeout around 50ms.
        // We set threshold at 150ms to be safe for CI.
        assert!(
            elapsed < Duration::from_millis(150),
            "Wait took {:?}, expected < 150ms",
            elapsed
        );
    }
}
