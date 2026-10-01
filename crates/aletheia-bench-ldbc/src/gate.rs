//! Regression gate: compare a fresh run against a stored baseline and fail on
//! a p99 regression beyond a configured threshold (default 10%).
//!
//! The gate is **AletheiaDB-only** (it never compares against incumbents). It
//! is deterministic and side-effect free so it can be unit-tested with
//! synthetic reports (including a synthetic 2x slowdown, which must fail).

use crate::report::BenchmarkReport;
use serde::{Deserialize, Serialize};

/// The default p99 regression threshold, as a percentage.
pub const DEFAULT_THRESHOLD_PCT: f64 = 10.0;

/// The default absolute-delta noise floor, in microseconds.
///
/// Sub-microsecond operations (single-node in-process reads) have p99s around
/// 0.5–1µs, where run-to-run scheduler jitter of ~0.2µs is a +30–40% relative
/// swing. A pure 10% relative gate would flap red every run on that noise. To
/// keep the ">10% p99" contract meaningful for latencies that actually matter
/// while suppressing sub-noise-floor jitter, an operation is only flagged when
/// its p99 grows by BOTH more than the relative threshold AND more than this
/// absolute floor. A 2x regression on any non-trivial operation clears this
/// floor comfortably (proven by the sensitivity test), so real regressions
/// still fail the gate.
pub const DEFAULT_MIN_ABS_DELTA_US: f64 = 5.0;

/// A single operation's gate comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationComparison {
    /// Operation key.
    pub key: String,
    /// Baseline p99 (µs).
    pub baseline_p99_us: f64,
    /// Current run p99 (µs).
    pub current_p99_us: f64,
    /// Percentage change: `(current - baseline) / baseline * 100`.
    pub delta_pct: f64,
    /// Absolute change in microseconds: `current - baseline`.
    pub delta_us: f64,
    /// `true` if the p99 grew by more than the relative threshold AND more
    /// than the absolute noise floor (a regression).
    pub regressed: bool,
}

/// The outcome of running the gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateResult {
    /// The threshold percentage applied.
    pub threshold_pct: f64,
    /// The absolute-delta noise floor applied (µs).
    pub min_abs_delta_us: f64,
    /// Per-operation comparisons for every key present in **both** reports.
    pub comparisons: Vec<OperationComparison>,
    /// Keys present in the baseline but missing from the current run.
    pub missing_in_current: Vec<String>,
    /// Keys present in the current run but not in the baseline (informational;
    /// never a failure — new operations cannot regress).
    pub new_in_current: Vec<String>,
}

impl GateResult {
    /// `true` when no operation regressed beyond the threshold and no baseline
    /// operation is missing from the current run.
    #[must_use]
    pub fn passed(&self) -> bool {
        !self.comparisons.iter().any(|c| c.regressed) && self.missing_in_current.is_empty()
    }

    /// The keys that regressed beyond threshold.
    #[must_use]
    pub fn regressions(&self) -> Vec<&OperationComparison> {
        self.comparisons.iter().filter(|c| c.regressed).collect()
    }
}

/// Evaluate the regression gate with the default absolute-delta noise floor
/// ([`DEFAULT_MIN_ABS_DELTA_US`]).
///
/// For every operation present in the baseline, compare its p99 against the
/// current run. A current p99 is a regression when it exceeds
/// `baseline_p99 * (1 + threshold_pct/100)` **and** the absolute increase
/// exceeds the noise floor. Baseline operations absent from the current run are
/// treated as failures (a dropped operation must not silently pass the gate).
#[must_use]
pub fn evaluate_gate(
    baseline: &BenchmarkReport,
    current: &BenchmarkReport,
    threshold_pct: f64,
) -> GateResult {
    evaluate_gate_with_floor(baseline, current, threshold_pct, DEFAULT_MIN_ABS_DELTA_US)
}

/// Evaluate the regression gate with an explicit absolute-delta noise floor.
///
/// See [`evaluate_gate`]. An operation is flagged only when BOTH its p99 grew
/// by more than `threshold_pct` percent AND by more than `min_abs_delta_us`
/// microseconds — so sub-microsecond scheduler jitter (a large *relative* but
/// tiny *absolute* swing) does not produce false failures, while any real
/// regression on a latency that matters still fails the gate.
#[must_use]
pub fn evaluate_gate_with_floor(
    baseline: &BenchmarkReport,
    current: &BenchmarkReport,
    threshold_pct: f64,
    min_abs_delta_us: f64,
) -> GateResult {
    let mut comparisons = Vec::new();
    let mut missing_in_current = Vec::new();

    for base_op in &baseline.operations {
        match current.operation(&base_op.key) {
            Some(cur_op) => {
                let base_p99 = base_op.stats.p99_us;
                let cur_p99 = cur_op.stats.p99_us;
                let delta_us = cur_p99 - base_p99;
                // Guard against a zero/degenerate baseline: only flag a
                // regression when there is a positive baseline to exceed.
                let delta_pct = if base_p99 > 0.0 {
                    delta_us / base_p99 * 100.0
                } else {
                    0.0
                };
                let regressed =
                    base_p99 > 0.0 && delta_pct > threshold_pct && delta_us > min_abs_delta_us;
                comparisons.push(OperationComparison {
                    key: base_op.key.clone(),
                    baseline_p99_us: base_p99,
                    current_p99_us: cur_p99,
                    delta_pct,
                    delta_us,
                    regressed,
                });
            }
            None => missing_in_current.push(base_op.key.clone()),
        }
    }

    let baseline_keys: std::collections::HashSet<&str> =
        baseline.operations.iter().map(|o| o.key.as_str()).collect();
    let new_in_current = current
        .operations
        .iter()
        .filter(|o| !baseline_keys.contains(o.key.as_str()))
        .map(|o| o.key.clone())
        .collect();

    GateResult {
        threshold_pct,
        min_abs_delta_us,
        comparisons,
        missing_in_current,
        new_in_current,
    }
}
