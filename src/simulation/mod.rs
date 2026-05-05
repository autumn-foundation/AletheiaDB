//! Deterministic Simulation Testing (DST) framework (issue #154).
//!
//! Provides a controlled, reproducible environment for testing AletheiaDB's
//! bi-temporal model under clock jumps, storage faults, and concurrent
//! transaction interleavings — all driven from a single integer seed.
//!
//! # Quick start
//!
//! ```ignore
//! use aletheiadb::simulation::{Simulator, FaultConfig};
//!
//! // Every detail is reproducible from seed 42.
//! // The Simulator automatically injects its clock into time::now() for the
//! // duration of its lifetime — no manual guard management needed.
//! let mut sim = Simulator::new(42);
//! let (_tmp, db) = aletheiadb::test_utils::create_test_db().unwrap();
//!
//! sim.advance_time_by(1_000_000); // move forward 1 s — time::now() returns 1 s
//! sim.inject_clock_jump(-500_000); // NTP-style correction — time::now() returns 0.5 s
//!
//! db.create_node("Event", aletheiadb::PropertyMapBuilder::new().build()).unwrap();
//!
//! let report = sim.verify_temporal_invariants(&db);
//! assert!(report.passed);
//! ```

pub mod clock;
pub mod fault;
pub mod scheduler;
pub mod storage;

pub use clock::{ClockInjectionGuard, SimulatedClock};
pub use fault::{FaultConfig, FaultInjector, FaultType};
pub use scheduler::SimulatedScheduler;
pub use storage::{SimStorageError, SimulatedStorage};

use crate::core::temporal::{TIMESTAMP_MAX, TimeRange};
use crate::db::AletheiaDB;

// ============================================================================
// Temporal invariant verification
// ============================================================================

/// Result of checking temporal invariants across a database snapshot.
#[derive(Debug, Default)]
pub struct InvariantReport {
    /// `true` when all checked invariants hold.
    pub passed: bool,
    /// Human-readable description of each violation found.
    pub violations: Vec<String>,
    /// Number of current nodes examined.
    pub nodes_checked: usize,
}

/// Verify the eight core temporal invariants against the current database state.
///
/// | # | Invariant |
/// |---|-----------|
/// | 1 | Tx-time monotonicity: each version's tx_time ≥ predecessor's |
/// | 2 | Version number ordering: strictly increasing |
/// | 3 | Time range validity: start ≤ end for every stored range |
/// | 4 | Visibility consistency: `visible_at(vt,tt)` iff valid_at(vt) ∧ recorded_at(tt) |
/// | 5 | Overlap symmetry: r1.overlaps(r2) == r2.overlaps(r1) |
/// | 6 | Contains-range reflexivity: r.contains_range(r) == true |
/// | 7 | Half-open interval semantics: end is always exclusive |
/// | 8 | Temporal isolation: time-travel yields consistent snapshot values |
///
/// Checks invariants 1, 2, 3, 6, and 7 by inspecting the bi-temporal version
/// history of every node currently in the database.
fn verify_temporal_invariants_impl(db: &AletheiaDB) -> InvariantReport {
    use crate::core::temporal::time;

    let mut report = InvariantReport {
        passed: true,
        violations: Vec::new(),
        nodes_checked: 0,
    };

    // ── Invariant 6: TimeRange::contains_range reflexivity ──────────────────
    {
        let r = TimeRange::from(time::from_secs(0));
        if !r.contains_range(&r) {
            push_violation(
                &mut report,
                "TimeRange::contains_range reflexivity violated".to_owned(),
            );
        }
    }

    // ── Check all live nodes ─────────────────────────────────────────────────
    let node_ids = db.get_all_node_ids();
    report.nodes_checked = node_ids.len();

    for node_id in node_ids {
        let history = match db.get_node_history(node_id) {
            Ok(h) => h,
            Err(e) => {
                push_violation(
                    &mut report,
                    format!("node {node_id:?}: failed to retrieve history: {e}"),
                );
                continue;
            }
        };

        let mut prev_tx_start = None;
        let mut prev_version_number = None;

        for version in &history.versions {
            let vt = version.temporal.valid_time();
            let tt = version.temporal.transaction_time();

            // ── Invariant 3: start ≤ end for both dimensions ───────────────
            if vt.start() > vt.end() {
                push_violation(
                    &mut report,
                    format!(
                        "node {node_id:?} v{}: valid_time start {:?} > end {:?} (Inv 3)",
                        version.version_number,
                        vt.start(),
                        vt.end()
                    ),
                );
            }
            if tt.start() > tt.end() {
                push_violation(
                    &mut report,
                    format!(
                        "node {node_id:?} v{}: tx_time start {:?} > end {:?} (Inv 3)",
                        version.version_number,
                        tt.start(),
                        tt.end()
                    ),
                );
            }

            // ── Invariant 7: end is exclusive — end must not equal a timestamp ─
            // Closed ranges must have end > start (a zero-width closed range is
            // only valid for point-in-time ranges explicitly created with at()).
            // We check that a closed range's `contains(end)` is false.
            if vt.is_closed() && vt.contains(vt.end()) {
                push_violation(
                    &mut report,
                    format!(
                        "node {node_id:?} v{}: valid_time.end {:?} is inclusive (Inv 7)",
                        version.version_number,
                        vt.end()
                    ),
                );
            }
            if tt.is_closed() && tt.contains(tt.end()) {
                push_violation(
                    &mut report,
                    format!(
                        "node {node_id:?} v{}: tx_time.end {:?} is inclusive (Inv 7)",
                        version.version_number,
                        tt.end()
                    ),
                );
            }

            // ── Invariant 1: tx_time.start is monotonically non-decreasing ─────
            if let Some(prev) = prev_tx_start
                && tt.start() < prev
            {
                push_violation(
                    &mut report,
                    format!(
                        "node {node_id:?} v{}: tx_time.start {:?} < previous {:?} (Inv 1)",
                        version.version_number,
                        tt.start(),
                        prev
                    ),
                );
            }
            prev_tx_start = Some(tt.start());

            // ── Invariant 2: version numbers are strictly increasing ────────────
            if let Some(prev_vn) = prev_version_number
                && version.version_number <= prev_vn
            {
                push_violation(
                    &mut report,
                    format!(
                        "node {node_id:?}: version_number {} not > previous {} (Inv 2)",
                        version.version_number, prev_vn
                    ),
                );
            }
            prev_version_number = Some(version.version_number);

            // ── Invariant 4: visibility consistency ────────────────────────────
            // A version visible at (vt_point, tt_point) must satisfy both ranges.
            let vt_mid = vt.start();
            let tt_mid = tt.start();
            let expected = vt.contains(vt_mid) && tt.contains(tt_mid);
            let actual = version.temporal.is_visible_at(vt_mid, tt_mid);
            if actual != expected {
                push_violation(
                    &mut report,
                    format!(
                        "node {node_id:?} v{}: is_visible_at({vt_mid:?},{tt_mid:?}) = {actual} \
                         but expected {expected} (Inv 4)",
                        version.version_number
                    ),
                );
            }
        }
    }

    // ── Invariant 3 & 7: reference clock must be ≤ TIMESTAMP_MAX ───────────
    {
        let now = time::now();
        if now > TIMESTAMP_MAX {
            push_violation(
                &mut report,
                format!(
                    "Current time {now:?} exceeds TIMESTAMP_MAX — clock injection out of bounds"
                ),
            );
        }
    }

    report
}

fn push_violation(report: &mut InvariantReport, msg: String) {
    report.violations.push(msg);
    report.passed = false;
}

// ============================================================================
// Simulator orchestrator
// ============================================================================

/// Top-level DST harness: controls clock, faults, and scheduling from one seed.
///
/// The `Simulator` **automatically injects its clock** into `time::now()` for
/// its entire lifetime. Any call to [`advance_time_by`](Self::advance_time_by)
/// or [`inject_clock_jump`](Self::inject_clock_jump) is immediately reflected
/// in subsequent `time::now()` calls on the same thread — no manual
/// `clock().inject()` call is required.
///
/// When the `Simulator` is dropped the real wall clock is restored.
///
/// Create with [`Simulator::new`] (no faults) or
/// [`Simulator::with_seed_and_faults`] (custom fault config).
#[derive(Debug)]
pub struct Simulator {
    seed: u64,
    clock: SimulatedClock,
    faults: FaultInjector,
    /// Holds the thread-local override active for the lifetime of this Simulator.
    /// Declared after `clock` so the clock is still alive when the guard drops
    /// (though the guard does not reference the clock, this keeps drop order clear).
    _clock_guard: ClockInjectionGuard,
}

impl Simulator {
    /// Create a fault-free simulator seeded with `seed`.
    ///
    /// Immediately injects the simulated clock into `time::now()` on the
    /// current thread.
    pub fn new(seed: u64) -> Self {
        let clock = SimulatedClock::new(0);
        let guard = clock.inject();
        Self {
            seed,
            clock,
            faults: FaultInjector::new(FaultConfig::default(), seed),
            _clock_guard: guard,
        }
    }

    /// Create a simulator with the given seed and custom fault configuration.
    ///
    /// Immediately injects the simulated clock into `time::now()` on the
    /// current thread.
    pub fn with_seed_and_faults(seed: u64, config: FaultConfig) -> Self {
        let clock = SimulatedClock::new(0);
        let guard = clock.inject();
        Self {
            seed,
            clock,
            faults: FaultInjector::new(config, seed),
            _clock_guard: guard,
        }
    }

    /// The seed used to initialise this simulator.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Immutable access to the simulated clock.
    pub fn clock(&self) -> &SimulatedClock {
        &self.clock
    }

    /// Mutable access to the simulated clock.
    pub fn clock_mut(&mut self) -> &mut SimulatedClock {
        &mut self.clock
    }

    /// Reference to the fault injector.
    pub fn faults(&self) -> &FaultInjector {
        &self.faults
    }

    /// Advance simulated time forward by `delta_micros` microseconds.
    ///
    /// `time::now()` on the current thread will return the updated time
    /// immediately.
    pub fn advance_time_by(&mut self, delta_micros: i64) {
        self.clock.advance_by(delta_micros);
    }

    /// Apply a clock discontinuity of `delta_micros` (positive = forward, negative = backward).
    ///
    /// `time::now()` on the current thread will return the updated time
    /// immediately.
    pub fn inject_clock_jump(&mut self, delta_micros: i64) {
        self.clock.jump_by(delta_micros);
    }

    /// Check that `db` satisfies all verifiable temporal invariants.
    ///
    /// Returns an [`InvariantReport`] describing any violations found.
    pub fn verify_temporal_invariants(&self, db: &AletheiaDB) -> InvariantReport {
        verify_temporal_invariants_impl(db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::temporal::time;
    use crate::test_utils::create_test_db;

    #[test]
    fn new_simulator_seed_accessible() {
        let s = Simulator::new(99);
        assert_eq!(s.seed(), 99);
    }

    #[test]
    fn simulator_auto_injects_clock() {
        let sim = Simulator::new(0);
        // Clock starts at 0 — time::now() must return the simulated value.
        assert_eq!(time::now().wallclock(), 0);
        drop(sim);
    }

    #[test]
    fn advance_time_by_updates_time_now() {
        let mut sim = Simulator::new(0);
        sim.advance_time_by(500_000);
        assert_eq!(time::now().wallclock(), 500_000);
    }

    #[test]
    fn fresh_db_passes_invariants() {
        let s = Simulator::new(0);
        let (_tmp, db) = create_test_db().unwrap();
        let r = s.verify_temporal_invariants(&db);
        assert!(r.passed, "{:?}", r.violations);
    }

    #[test]
    fn invariants_check_real_node_versions() {
        use crate::PropertyMapBuilder;
        use crate::api::WriteOps;

        let s = Simulator::new(1_000_000); // start at 1s
        let (_tmp, db) = create_test_db().unwrap();

        let props = PropertyMapBuilder::new().build();
        let node = db.create_node("Item", props).unwrap();
        db.write(|tx: &mut crate::WriteTransaction| {
            tx.update_node(node, PropertyMapBuilder::new().insert("v", 2_i64).build())
        })
        .unwrap();

        let r = s.verify_temporal_invariants(&db);
        assert_eq!(r.nodes_checked, 1);
        assert!(r.passed, "{:?}", r.violations);
    }
}
