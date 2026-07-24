//! Background replica applier (Issue #3355, Slice B).
//!
//! [`ReplicaApplierHandle`] owns a [`ReplicationSource`] and drives the
//! fetch → apply-complete-frames → publish-progress → periodic-persist loop
//! on a dedicated background thread, following this repo's background-thread
//! idiom (`FlushThread`, `src/storage/wal/flush_coordinator.rs`): an
//! `Arc<AtomicBool>` shutdown flag + `JoinHandle` + `Drop` that stops and
//! joins the thread.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::db::ReplicaProgressStats;
use crate::storage::wal::WalEntry;

use super::apply::{ReplicaStorageHandles, apply_replica_batch};
use super::feed::FetchOutcome;
use super::source::ReplicationSource;

const STATE_CONNECTING: u8 = 0;
const STATE_STREAMING: u8 = 1;
const STATE_RESYNC_REQUIRED: u8 = 2;
const STATE_STOPPED: u8 = 3;

fn state_str(state: u8) -> &'static str {
    match state {
        STATE_STREAMING => "streaming",
        STATE_RESYNC_REQUIRED => "resync_required",
        STATE_STOPPED => "stopped",
        _ => "connecting",
    }
}

/// Options controlling a replica applier's polling/persistence cadence.
///
/// Re-exported at `crate::db::replication::ReplicationOptions` (the public
/// surface); kept here too since the applier is the thing that actually reads
/// it.
#[derive(Debug, Clone)]
pub struct ReplicationOptions {
    /// How long to wait between fetch attempts when there is nothing new (or
    /// after a fetch error / resync-required state).
    pub poll_interval: Duration,
    /// Maximum WAL entries requested per `fetch_entries` call.
    pub batch_max_entries: usize,
    /// Persist indexes after at least this much wall-clock time has elapsed
    /// since the last persist, provided at least one entry was applied.
    /// `None` disables the time-based trigger (count-based persistence via
    /// `persist_every_entries` still applies).
    pub persist_every: Option<Duration>,
    /// Persist indexes after at least this many WAL entries have been applied
    /// since the last persist. `0` disables the count-based trigger.
    pub persist_every_entries: u64,
}

impl Default for ReplicationOptions {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(50),
            batch_max_entries: 500,
            persist_every: Some(Duration::from_secs(5)),
            persist_every_entries: 500,
        }
    }
}

/// Shared, atomics-based replica progress/lag state.
///
/// Cheap to read frequently (all-atomic except the rare `last_error` string),
/// so `stats()` and `replication_progress()` can poll it without contending
/// with the applier thread.
pub(crate) struct ReplicaProgress {
    state: AtomicU8,
    last_applied_lsn: AtomicU64,
    /// `0` sentinel means "not yet observed" (LSNs start at 1).
    primary_flushed_lsn: AtomicU64,
    /// `i64::MIN` sentinel means "not yet observed".
    last_applied_wallclock_micros: AtomicI64,
    /// `i64::MIN` sentinel means "not yet observed".
    last_primary_wallclock_micros: AtomicI64,
    last_error: Mutex<Option<String>>,
}

impl ReplicaProgress {
    fn new(start_lsn: u64) -> Self {
        Self {
            state: AtomicU8::new(STATE_CONNECTING),
            last_applied_lsn: AtomicU64::new(start_lsn.saturating_sub(1)),
            primary_flushed_lsn: AtomicU64::new(0),
            last_applied_wallclock_micros: AtomicI64::new(i64::MIN),
            last_primary_wallclock_micros: AtomicI64::new(i64::MIN),
            last_error: Mutex::new(None),
        }
    }

    pub(crate) fn last_applied_lsn(&self) -> u64 {
        self.last_applied_lsn.load(Ordering::Acquire)
    }

    fn set_error(&self, message: String) {
        if let Ok(mut guard) = self.last_error.lock() {
            *guard = Some(message);
        }
    }

    fn clear_error(&self) {
        if let Ok(mut guard) = self.last_error.lock() {
            *guard = None;
        }
    }

    /// Snapshot for `DatabaseStats.replication.replica` / `replication_progress()`.
    pub(crate) fn snapshot(&self) -> ReplicaProgressStats {
        let state = self.state.load(Ordering::Acquire);
        let last_applied_lsn = self.last_applied_lsn.load(Ordering::Acquire);
        let primary_flushed_raw = self.primary_flushed_lsn.load(Ordering::Acquire);
        let primary_flushed_lsn = (primary_flushed_raw > 0).then_some(primary_flushed_raw);
        let entries_behind = primary_flushed_lsn.map(|p| p.saturating_sub(last_applied_lsn));

        let lag_ms = match entries_behind {
            Some(n) if n > 0 => {
                let last_ts = self.last_applied_wallclock_micros.load(Ordering::Acquire);
                let primary_ts = self.last_primary_wallclock_micros.load(Ordering::Acquire);
                if last_ts > i64::MIN && primary_ts > i64::MIN {
                    Some((primary_ts.saturating_sub(last_ts)).max(0) as u64 / 1000)
                } else {
                    Some(0)
                }
            }
            _ => Some(0),
        };

        let last_error = self.last_error.lock().ok().and_then(|g| g.clone());

        ReplicaProgressStats {
            state: state_str(state).to_string(),
            last_applied_lsn,
            primary_flushed_lsn,
            entries_behind,
            lag_ms,
            last_error,
        }
    }
}

/// Handle to a running background replica applier thread.
///
/// Dropping it stops and joins the thread (mirrors `FlushThread`). Promotion
/// (`AletheiaDB::promote_to_primary`) takes the handle out of the database's
/// slot and drops it explicitly so it can read `last_applied_lsn()` first.
pub(crate) struct ReplicaApplierHandle {
    pub(crate) progress: Arc<ReplicaProgress>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ReplicaApplierHandle {
    /// Spawn the background applier thread, resuming from `start_lsn`
    /// (inclusive -- the first `fetch_entries` call requests exactly this
    /// LSN).
    pub(crate) fn spawn(
        handles: ReplicaStorageHandles,
        mut source: Box<dyn ReplicationSource>,
        opts: ReplicationOptions,
        start_lsn: u64,
    ) -> Self {
        let start_lsn = start_lsn.max(1);
        let progress = Arc::new(ReplicaProgress::new(start_lsn));
        let shutdown = Arc::new(AtomicBool::new(false));

        let progress_for_thread = Arc::clone(&progress);
        let shutdown_for_thread = Arc::clone(&shutdown);

        let thread_handle = thread::spawn(move || {
            run_applier_loop(
                &handles,
                source.as_mut(),
                &opts,
                &progress_for_thread,
                &shutdown_for_thread,
            );
        });

        Self {
            progress,
            shutdown,
            handle: Some(thread_handle),
        }
    }

    /// A live snapshot of this applier's progress.
    pub(crate) fn progress_snapshot(&self) -> ReplicaProgressStats {
        self.progress.snapshot()
    }
}

impl Drop for ReplicaApplierHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_applier_loop(
    handles: &ReplicaStorageHandles,
    source: &mut dyn ReplicationSource,
    opts: &ReplicationOptions,
    progress: &Arc<ReplicaProgress>,
    shutdown: &Arc<AtomicBool>,
) {
    let mut since_persist_entries: u64 = 0;
    let mut last_persist_at = Instant::now();
    // Fetched-but-not-yet-applied entries, accumulated ACROSS polls: see
    // `apply_replica_batch`'s doc comment for why a transport that returns
    // small batches per call requires this (otherwise a multi-op
    // transaction's commit band longer than one fetch could stall forever).
    let mut pending: Vec<WalEntry> = Vec::new();

    while !shutdown.load(Ordering::Acquire) {
        // Request only the entries not already buffered in `pending`.
        let from_lsn = progress
            .last_applied_lsn()
            .saturating_add(1)
            .saturating_add(pending.len() as u64);

        match source.fetch_entries(from_lsn, opts.batch_max_entries) {
            Ok(FetchOutcome::Entries {
                entries,
                primary_flushed_lsn,
                primary_wallclock_micros,
            }) => {
                progress
                    .primary_flushed_lsn
                    .store(primary_flushed_lsn, Ordering::Release);
                progress
                    .last_primary_wallclock_micros
                    .store(primary_wallclock_micros, Ordering::Release);
                progress.state.store(STATE_STREAMING, Ordering::Release);
                progress.clear_error();

                // Trust boundary: a conforming source (the in-process feed /
                // TCP server) already guarantees a dense batch starting at
                // exactly `from_lsn`, but `ReplicationSource` is a trait --
                // verify anyway so a buggy or hostile source can never make
                // this replica silently skip LSNs or tear a frame. A
                // non-conforming batch is dropped whole (re-requested next
                // poll); `pending` stays dense from `last_applied + 1` by
                // induction.
                match batch_continuity_error(&entries, from_lsn) {
                    Some(msg) => progress.set_error(msg),
                    None => pending.extend(entries),
                }

                if !pending.is_empty() {
                    match apply_replica_batch(handles, &mut pending) {
                        Ok(Some(applied)) => {
                            progress
                                .last_applied_lsn
                                .store(applied.lsn, Ordering::Release);
                            progress
                                .last_applied_wallclock_micros
                                .store(applied.wallclock_micros, Ordering::Release);
                            since_persist_entries =
                                since_persist_entries.saturating_add(applied.entries_applied);
                        }
                        Ok(None) => {
                            // No complete frame yet even across everything
                            // buffered so far; keep accumulating on the next
                            // poll(s) until the completing CommitTx arrives.
                        }
                        Err(e) => {
                            progress.set_error(format!("apply failed: {e}"));
                        }
                    }
                }
            }
            Ok(FetchOutcome::ResyncRequired { min_available_lsn }) => {
                progress
                    .state
                    .store(STATE_RESYNC_REQUIRED, Ordering::Release);
                progress.set_error(format!(
                    "resync required: requested LSN {from_lsn} is below the primary's \
                     minimum available LSN ({min_available_lsn}); this replica must be \
                     re-bootstrapped from a fresh snapshot"
                ));
                // Deliberately idle: never skip ahead. The thread stays alive
                // so an operator can observe the state; only a fresh
                // `bootstrap_replica`/`start_replication` call recovers.
            }
            Err(e) => {
                progress.state.store(STATE_CONNECTING, Ordering::Release);
                progress.set_error(format!("fetch failed: {e}"));
            }
        }

        // Periodic durability: persist indexes stamped with `applied_lsn` (a
        // PRIMARY-space coordinate, distinct from this replica's own local
        // WAL LSN space, which the applier never appends to) so a restarted
        // replica resumes from here instead of re-streaming from scratch.
        if let Some((manager, tracker)) = handles.persistence.as_ref()
            && since_persist_entries > 0
        {
            let due_by_count = opts.persist_every_entries > 0
                && since_persist_entries >= opts.persist_every_entries;
            let due_by_time = opts
                .persist_every
                .is_some_and(|d| last_persist_at.elapsed() >= d);
            if due_by_count || due_by_time {
                let applied_lsn = progress.last_applied_lsn();
                let _ = crate::storage::index_persistence::operations::persist_all_indexes_at_lsn(
                    &handles.current,
                    &handles.historical,
                    &handles.temporal_indexes,
                    manager,
                    tracker,
                    applied_lsn,
                );
                since_persist_entries = 0;
                last_persist_at = Instant::now();
            }
        }

        park_for(shutdown, opts.poll_interval);
    }

    progress.state.store(STATE_STOPPED, Ordering::Release);
}

/// `None` if `entries` is a dense LSN run starting exactly at `expected_lsn`
/// (an empty batch is trivially conforming); otherwise a description of the
/// first violation. See the call site for why this is enforced.
fn batch_continuity_error(entries: &[WalEntry], expected_lsn: u64) -> Option<String> {
    let mut expected = expected_lsn;
    for entry in entries {
        if entry.lsn.0 != expected {
            return Some(format!(
                "replication source returned a non-contiguous batch: expected LSN \
                 {expected}, got {}; dropping batch and re-requesting",
                entry.lsn.0
            ));
        }
        expected = expected.wrapping_add(1);
    }
    None
}

/// Sleep for up to `dur`, checking `shutdown` in small increments so a
/// shutdown request is picked up promptly rather than after the full
/// interval.
fn park_for(shutdown: &AtomicBool, dur: Duration) {
    let step = Duration::from_millis(10).min(dur.max(Duration::from_millis(1)));
    let mut waited = Duration::ZERO;
    while waited < dur && !shutdown.load(Ordering::Acquire) {
        thread::sleep(step);
        waited += step;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hlc::HybridTimestamp;
    use crate::storage::wal::{LSN, WalOperation};

    fn entry(lsn: u64) -> WalEntry {
        WalEntry {
            lsn: LSN(lsn),
            timestamp: HybridTimestamp::new_unchecked(1, 0),
            operation: WalOperation::BeginTx { tx_id: 1 },
            checksum: 0,
            framed: true,
            segment_version: None,
        }
    }

    /// Review finding #1 (Issue #3355): the applier must reject any batch
    /// that is not a dense run starting exactly at the requested LSN, so a
    /// non-conforming source can never make it skip or tear.
    #[test]
    fn conforming_batches_pass() {
        assert!(batch_continuity_error(&[], 5).is_none());
        assert!(batch_continuity_error(&[entry(5)], 5).is_none());
        assert!(batch_continuity_error(&[entry(5), entry(6), entry(7)], 5).is_none());
    }

    #[test]
    fn head_mismatch_is_rejected() {
        let err = batch_continuity_error(&[entry(6)], 5).expect("must reject");
        assert!(err.contains("expected LSN 5"), "got: {err}");
    }

    #[test]
    fn interior_gap_is_rejected() {
        let err = batch_continuity_error(&[entry(5), entry(7)], 5).expect("must reject");
        assert!(err.contains("expected LSN 6"), "got: {err}");
    }
}
