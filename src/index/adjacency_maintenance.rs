//! Background adjacency maintenance (ADR-0026 Phase 5/6, Issue #3810).
//!
//! # Why this exists
//!
//! [`IncrementalAdjacencyIndex`] is a two-tier (LSM-shaped) structure: an
//! immutable frozen CSR plus a mutable delta buffer and a tombstone set. Its
//! documented ~8-14ns read fast path
//! ([`IncrementalAdjacencyIndex::frozen_view`]) is only available when the
//! delta buffer **and** the tombstone set are *globally empty* -- a state only
//! `compact()` can produce.
//!
//! ADR-0026 Phase 5 shipped a per-index [`CompactionScheduler`], but nothing
//! outside the tests ever started one: `CurrentStorage::new()` -- the single
//! constructor behind both `AletheiaDB::new()` and `AletheiaDB::open()` -- built
//! its indexes without a scheduler. Compaction therefore never ran in a real
//! database, for any graph size, for the life of the process, and every
//! `get_outgoing_edges`/`get_incoming_edges` call permanently paid the merged
//! (delta) path: a `DashMap` hash lookup, iterator machinery, and a
//! tombstone-emptiness branch the frozen path skips entirely (Issue #3810
//! measured 100% of 129,000 adjacency reads on the slow path).
//!
//! # What this module does
//!
//! One **process-wide** worker thread services every registered adjacency index
//! through [`Weak`] references. That shape is deliberate:
//!
//! - **One thread per process, not two per database.** A database owns two
//!   adjacency indexes (outgoing + incoming); the test suite alone constructs
//!   hundreds of ephemeral databases. Registering into a shared worker keeps
//!   that at one thread total.
//! - **No shutdown obligation.** Registrations are `Weak`, so dropping a
//!   database deregisters its indexes with no `Drop` impl, no join, and no
//!   teardown cost. (Contrast [`CompactionScheduler`], whose
//!   `shutdown_background_compaction` takes `&mut self` and must be called by
//!   hand.)
//! - **Zero hot-path cost.** All policy -- quiescence detection, rate limiting,
//!   thresholds -- runs on the worker. Neither the read path nor the write path
//!   gains a single instruction; in particular no per-read atomic RMW, which
//!   would undo the multi-threaded read scaling work in #3811.
//!
//! # Policy
//!
//! Per tick, for each live index:
//!
//! 1. `pending = delta_edges + tombstones`. Nothing pending -> nothing to do.
//! 2. **Write quiescence**: `pending` unchanged across `quiet_ticks` consecutive
//!    ticks means no write landed in that window (both counters are monotonic
//!    between compactions, so equality *is* absence of writes -- no new
//!    write-path counter is needed). A quiescent index is compacted, which is
//!    what actually unlocks the read fast path: draining the delta *during* a
//!    write burst does not, because the very next insert re-disables it.
//! 3. **Size thresholds**: [`IncrementalAdjacencyIndex::should_compact`] still
//!    applies, bounding delta growth under a write burst that never goes quiet.
//! 4. **Rate limit**: after a compaction an index is ineligible until
//!    `max(min_compaction_interval, cost * (100 - duty_cycle_percent) /
//!    duty_cycle_percent)` has elapsed, where `cost` is how long that compaction
//!    actually took. This is a self-tuning duty-cycle budget: compaction may
//!    consume at most `duty_cycle_percent` of one core's wall time per index, so
//!    a large graph (where a rebuild is expensive) is compacted proportionally
//!    less often and a write/read-interleaved workload can never be driven into
//!    the O(E log E) rebuild cliff ADR-0026 exists to avoid.
//!
//! Rule 2 is also what closes the "threshold bootstrap gap" called out in Issue
//! #3810: `should_compact()`'s ratio branch (`frozen > 0 && delta >= frozen *
//! ratio`) is dead on a fresh index because `frozen` starts at 0, so a graph
//! below the 10,000-edge absolute threshold could never trigger compaction on
//! size alone. Quiescence is size-independent, so a 600-edge graph reaches the
//! frozen fast path just as a 6,000,000-edge one does.
//!
//! # Not enabled everywhere
//!
//! Registration is a no-op on `wasm32` (no threads) and under Miri (which has
//! no reason to interpret a background poller). Both fall back to exactly the
//! pre-#3810 behavior: correct reads via the merged path, and explicit
//! `compact_adjacency()` still available.

use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use crate::index::incremental_adjacency::IncrementalAdjacencyIndex;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Maximum consecutive compaction panics tolerated for one index before the
/// worker stops servicing it.
///
/// Mirrors [`CompactionScheduler`]'s guard: a panicking compaction is a bug, and
/// retrying it forever would hide it while burning CPU. Dropping just that
/// registration keeps every other database in the process serviced.
const MAX_CONSECUTIVE_PANICS: u32 = 5;

/// Upper bound on how long the worker sleeps between ticks when it has
/// registrations. Only reached if every registration configures a longer
/// interval than this.
const MAX_SLEEP: Duration = Duration::from_secs(60);

/// Configuration for background adjacency maintenance (Issue #3810).
///
/// Reachable from the unified config as
/// [`AletheiaDBConfig::adjacency`](crate::config::AletheiaDBConfig::adjacency).
/// The default is **enabled**: without it the frozen-CSR read fast path is
/// unreachable in a shipping database.
///
/// # Example
///
/// ```
/// use aletheiadb::AletheiaDBConfig;
/// use aletheiadb::index::adjacency_maintenance::AdjacencyMaintenanceConfig;
///
/// // Opt out entirely (reads stay correct; they just keep taking the merged path).
/// let config = AletheiaDBConfig::builder()
///     .adjacency(AdjacencyMaintenanceConfig::disabled())
///     .build();
/// assert!(!config.adjacency.enabled);
/// ```
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct AdjacencyMaintenanceConfig {
    /// Whether this database's adjacency indexes are serviced by the shared
    /// background worker. Default: `true`.
    pub enabled: bool,

    /// How often the worker re-evaluates an index that has pending work
    /// (milliseconds). Default: 50.
    pub tick_interval_ms: u64,

    /// How often the worker wakes when every registered index is already
    /// compacted (milliseconds). Default: 500 -- an idle process pays two
    /// wakeups a second, each a handful of relaxed atomic loads.
    pub idle_tick_interval_ms: u64,

    /// Consecutive ticks an index's pending count must stay unchanged before it
    /// is considered write-quiescent. Default: 1 (i.e. two equal samples, so
    /// compaction lands roughly two ticks after the last write).
    pub quiet_ticks: u32,

    /// Floor on the interval between two compactions of the same index
    /// (milliseconds). Default: 250.
    pub min_compaction_interval_ms: u64,

    /// Share of wall time one index's compaction may consume, in percent
    /// (1..=100). Default: 10, i.e. a compaction that took 200ms makes that
    /// index ineligible for the next 1.8s.
    pub duty_cycle_percent: u32,
}

impl Default for AdjacencyMaintenanceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tick_interval_ms: 50,
            idle_tick_interval_ms: 500,
            quiet_ticks: 1,
            min_compaction_interval_ms: 250,
            duty_cycle_percent: 10,
        }
    }
}

impl AdjacencyMaintenanceConfig {
    /// Configuration with background maintenance turned off.
    ///
    /// Reads stay correct -- they take the merged (delta) path -- and
    /// [`AletheiaDB::compact_adjacency`](crate::AletheiaDB::compact_adjacency)
    /// remains available for explicit control.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// Clamp out-of-range values instead of failing a database open.
    ///
    /// A nonsensical interval is an operator typo, not a reason to refuse to
    /// start: every field is clamped into a range that keeps the worker
    /// well-behaved.
    fn normalized(mut self) -> Self {
        self.tick_interval_ms = self.tick_interval_ms.clamp(1, 60_000);
        self.idle_tick_interval_ms = self
            .idle_tick_interval_ms
            .clamp(self.tick_interval_ms, 60_000);
        self.min_compaction_interval_ms = self.min_compaction_interval_ms.min(600_000);
        self.duty_cycle_percent = self.duty_cycle_percent.clamp(1, 100);
        self
    }

    fn tick(&self) -> Duration {
        Duration::from_millis(self.tick_interval_ms)
    }

    fn idle_tick(&self) -> Duration {
        Duration::from_millis(self.idle_tick_interval_ms)
    }

    fn min_interval(&self) -> Duration {
        Duration::from_millis(self.min_compaction_interval_ms)
    }

    /// Cooldown after a compaction that took `cost`: the larger of the
    /// configured floor and the duty-cycle budget.
    fn cooldown(&self, cost: Duration) -> Duration {
        let budget = cost
            .saturating_mul(100u32.saturating_sub(self.duty_cycle_percent))
            .checked_div(self.duty_cycle_percent)
            .unwrap_or(Duration::ZERO);
        budget.max(self.min_interval())
    }
}

/// One registered adjacency index plus the worker's policy state for it.
struct Registration {
    id: u64,
    index: Weak<IncrementalAdjacencyIndex>,
    config: AdjacencyMaintenanceConfig,
    /// Pending (delta + tombstone) count observed on the previous tick.
    last_pending: Option<usize>,
    /// Consecutive ticks with an unchanged pending count.
    stable_ticks: u32,
    /// Earliest instant this index may be compacted again (duty-cycle limiter).
    next_eligible: Instant,
    consecutive_panics: u32,
}

#[derive(Default)]
struct ServiceState {
    entries: Vec<Registration>,
    next_id: u64,
    /// Set once a worker thread is running (or once spawning it failed, in
    /// which case maintenance degrades to "not running" rather than panicking).
    worker_started: bool,
}

struct Service {
    state: Mutex<ServiceState>,
    wakeup: Condvar,
}

static SERVICE: OnceLock<Service> = OnceLock::new();

fn service() -> &'static Service {
    SERVICE.get_or_init(|| Service {
        state: Mutex::new(ServiceState::default()),
        wakeup: Condvar::new(),
    })
}

/// Register an adjacency index for background maintenance.
///
/// Holds only a [`Weak`] reference: when the owning database is dropped, the
/// registration becomes inert and is pruned on the next tick. Calling this with
/// a disabled config, on `wasm32`, or under Miri is a no-op.
pub(crate) fn register(index: &Arc<IncrementalAdjacencyIndex>, config: AdjacencyMaintenanceConfig) {
    if !config.enabled {
        return;
    }
    // No threads on wasm32; no reason to interpret a poller under Miri.
    if cfg!(any(target_arch = "wasm32", miri)) {
        return;
    }

    let config = config.normalized();
    let service = service();
    let mut state = service.state.lock();

    let id = state.next_id;
    state.next_id = state.next_id.wrapping_add(1);
    state.entries.push(Registration {
        id,
        index: Arc::downgrade(index),
        config,
        last_pending: None,
        stable_ticks: 0,
        next_eligible: Instant::now(),
        consecutive_panics: 0,
    });

    if !state.worker_started {
        state.worker_started = true;
        // Spawn failure (thread limits, sandboxes) must not take the database
        // down: log once and leave maintenance off for this process.
        if let Err(e) = std::thread::Builder::new()
            .name("aletheia-adjacency".to_string())
            .spawn(worker_loop)
        {
            state.worker_started = false;
            eprintln!(
                "[adjacency-maintenance] could not start the background compaction worker \
                 ({e}); adjacency reads will keep taking the merged path. Call \
                 AletheiaDB::compact_adjacency() explicitly if that matters."
            );
            return;
        }
    }

    drop(state);
    service.wakeup.notify_all();
}

/// Number of live (non-dropped) index registrations.
///
/// Exposed for tests and diagnostics: it is the direct evidence that dropping a
/// database releases its maintenance registration without any explicit
/// shutdown call.
pub fn registered_index_count() -> usize {
    let Some(service) = SERVICE.get() else {
        return 0;
    };
    let state = service.state.lock();
    state
        .entries
        .iter()
        .filter(|e| e.index.strong_count() > 0)
        .count()
}

/// Whether the shared worker thread is running in this process.
///
/// Exposed for tests and diagnostics.
pub fn worker_is_running() -> bool {
    SERVICE
        .get()
        .map(|s| s.state.lock().worker_started)
        .unwrap_or(false)
}

/// A compaction the worker decided to run this tick.
struct Scheduled {
    id: u64,
    index: Arc<IncrementalAdjacencyIndex>,
}

/// Outcome of one scheduled compaction, applied back to the registry.
struct Outcome {
    id: u64,
    cost: Duration,
    panicked: bool,
}

impl Registration {
    /// Fold one tick's observation into this registration's policy state and
    /// answer whether the index should be compacted now.
    ///
    /// Pure with respect to the index (the two facts it needs are passed in),
    /// so the policy is unit-testable without a worker thread or a real graph.
    fn evaluate(&mut self, pending: usize, should_compact: bool, now: Instant) -> bool {
        if pending == 0 {
            self.last_pending = Some(0);
            self.stable_ticks = 0;
            return false;
        }

        // Both counters are monotonic between compactions, so an unchanged
        // pending count across a tick *is* the absence of writes in that
        // window -- no extra write-path counter needed.
        let stable = self.last_pending == Some(pending);
        self.stable_ticks = if stable { self.stable_ticks + 1 } else { 0 };
        self.last_pending = Some(pending);

        let quiescent = self.stable_ticks >= self.config.quiet_ticks;
        (quiescent || should_compact) && now >= self.next_eligible
    }

    /// How long to sleep before re-evaluating this registration.
    fn poll_interval(&self, pending: usize) -> Duration {
        if pending == 0 {
            self.config.idle_tick()
        } else {
            self.config.tick()
        }
    }
}

fn worker_loop() {
    let service = service();

    loop {
        let (scheduled, sleep_for) = {
            let mut state = service.state.lock();

            // Prune registrations whose database has been dropped.
            state.entries.retain(|e| e.index.strong_count() > 0);

            if state.entries.is_empty() {
                // Nothing to service: park until a database registers. The
                // worker is intentionally not shut down -- it costs one parked
                // thread and avoids a spawn per database lifecycle.
                service.wakeup.wait(&mut state);
                continue;
            }

            let now = Instant::now();
            let mut scheduled: Vec<Scheduled> = Vec::new();
            let mut sleep_for = MAX_SLEEP;

            for entry in state.entries.iter_mut() {
                let Some(index) = entry.index.upgrade() else {
                    continue;
                };

                let pending = index.delta_edge_count() + index.tombstone_count();
                sleep_for = sleep_for.min(entry.poll_interval(pending));

                if entry.evaluate(pending, index.should_compact(), now) {
                    scheduled.push(Scheduled {
                        id: entry.id,
                        index,
                    });
                }
            }

            (scheduled, sleep_for)
        };

        // Compact OFF-LOCK: a compaction is O(E log E) and must never block a
        // database construction (`register`) or another tick's bookkeeping.
        let outcomes: Vec<Outcome> = scheduled
            .into_iter()
            .map(|job| {
                let started = Instant::now();
                // `try_compact` rather than `compact`: if an explicit
                // `compact_adjacency()` is already running, the work is being
                // done and the worker should not queue behind it.
                let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    job.index.try_compact();
                }))
                .is_err();
                Outcome {
                    id: job.id,
                    cost: started.elapsed(),
                    panicked,
                }
            })
            .collect();

        let mut state = service.state.lock();
        if !outcomes.is_empty() {
            let now = Instant::now();
            for outcome in outcomes {
                let Some(entry) = state.entries.iter_mut().find(|e| e.id == outcome.id) else {
                    continue;
                };
                entry.stable_ticks = 0;
                entry.last_pending = entry
                    .index
                    .upgrade()
                    .map(|i| i.delta_edge_count() + i.tombstone_count());
                // Duty-cycle limiter: the more a compaction cost, the longer
                // this index waits before the next one.
                entry.next_eligible = now + entry.config.cooldown(outcome.cost);

                if outcome.panicked {
                    entry.consecutive_panics += 1;
                    eprintln!(
                        "[adjacency-maintenance] compaction panicked (consecutive: {})",
                        entry.consecutive_panics
                    );
                } else {
                    entry.consecutive_panics = 0;
                }
            }
            // Stop servicing an index that keeps panicking rather than burning
            // CPU on it forever; every other database stays serviced.
            state
                .entries
                .retain(|e| e.consecutive_panics < MAX_CONSECUTIVE_PANICS);
        }
        service.wakeup.wait_for(&mut state, sleep_for);
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    fn registration(config: AdjacencyMaintenanceConfig) -> Registration {
        Registration {
            id: 0,
            index: Weak::new(),
            config: config.normalized(),
            last_pending: None,
            stable_ticks: 0,
            next_eligible: Instant::now(),
            consecutive_panics: 0,
        }
    }

    #[test]
    fn quiescence_needs_two_matching_samples() {
        let mut entry = registration(AdjacencyMaintenanceConfig::default());
        let now = Instant::now();

        // First observation of pending work: nothing to compare against yet.
        assert!(!entry.evaluate(600, false, now));
        // Unchanged across a tick => no write landed => compact.
        assert!(entry.evaluate(600, false, now));
    }

    #[test]
    fn an_ongoing_write_burst_is_never_quiescent() {
        let mut entry = registration(AdjacencyMaintenanceConfig::default());
        let now = Instant::now();
        for pending in [10, 25, 60, 200, 900] {
            assert!(
                !entry.evaluate(pending, false, now),
                "a growing delta means writes are still landing"
            );
        }
    }

    #[test]
    fn size_thresholds_still_fire_during_a_burst() {
        let mut entry = registration(AdjacencyMaintenanceConfig::default());
        let now = Instant::now();
        // Never stable, but `should_compact()` says the delta is oversized.
        assert!(entry.evaluate(10_000, true, now));
    }

    #[test]
    fn nothing_pending_means_nothing_to_do() {
        let mut entry = registration(AdjacencyMaintenanceConfig::default());
        let now = Instant::now();
        assert!(!entry.evaluate(0, false, now));
        assert!(!entry.evaluate(0, true, now));
        assert_eq!(entry.stable_ticks, 0);
    }

    #[test]
    fn the_rate_limiter_defers_an_otherwise_due_compaction() {
        let mut entry = registration(AdjacencyMaintenanceConfig::default());
        let now = Instant::now();
        entry.next_eligible = now + Duration::from_secs(5);

        assert!(!entry.evaluate(600, true, now), "still cooling down");
        // Quiescence bookkeeping still advances, so it fires as soon as the
        // cooldown expires rather than needing two fresh samples.
        assert!(entry.evaluate(600, true, now + Duration::from_secs(6)));
    }

    #[test]
    fn poll_interval_backs_off_when_everything_is_compacted() {
        let entry = registration(AdjacencyMaintenanceConfig::default());
        assert_eq!(entry.poll_interval(0), entry.config.idle_tick());
        assert_eq!(entry.poll_interval(1), entry.config.tick());
        assert!(entry.config.idle_tick() >= entry.config.tick());
    }

    #[test]
    fn cooldown_respects_the_duty_cycle_budget() {
        let cfg = AdjacencyMaintenanceConfig::default();
        // A 1s compaction at a 10% duty cycle => 9s of cooldown.
        assert_eq!(cfg.cooldown(Duration::from_secs(1)), Duration::from_secs(9));
        // A trivial compaction is floored by min_compaction_interval.
        assert_eq!(
            cfg.cooldown(Duration::from_micros(10)),
            Duration::from_millis(250)
        );
    }

    #[test]
    fn cooldown_at_full_duty_cycle_is_the_floor() {
        let cfg = AdjacencyMaintenanceConfig {
            duty_cycle_percent: 100,
            ..AdjacencyMaintenanceConfig::default()
        };
        assert_eq!(
            cfg.cooldown(Duration::from_secs(5)),
            Duration::from_millis(250)
        );
    }

    #[test]
    fn normalized_clamps_nonsense_values() {
        let cfg = AdjacencyMaintenanceConfig {
            tick_interval_ms: 0,
            idle_tick_interval_ms: 0,
            duty_cycle_percent: 0,
            ..AdjacencyMaintenanceConfig::default()
        }
        .normalized();
        assert_eq!(cfg.tick_interval_ms, 1);
        assert!(cfg.idle_tick_interval_ms >= cfg.tick_interval_ms);
        assert_eq!(cfg.duty_cycle_percent, 1);
    }

    #[test]
    fn disabled_config_never_registers() {
        let before = registered_index_count();
        let index = Arc::new(IncrementalAdjacencyIndex::new());
        register(&index, AdjacencyMaintenanceConfig::disabled());
        assert_eq!(registered_index_count(), before);
    }
}
